//! Private user-scoped transport: Unix Domain Socket or Win32 Named Pipe.
//!
//! Every connection is same-user verified, bounded in size and time, and
//! counted against a hard concurrency quota. Requests and replies are capped;
//! idle clients are disconnected instead of occupying a worker forever.
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// Largest single JSON-RPC request line, including its terminating newline
/// budget but excluding it from the content limit.
const MAX_REQUEST_BYTES: usize = 64 * 1024;
/// Largest serialized JSON-RPC reply. Mirrors the client-side cap.
const MAX_REPLY_BYTES: usize = 4 * 1024 * 1024;
/// A client may issue this many requests on one connection, then must
/// reconnect. Prevents an endless stream from pinning a worker indefinitely.
const MAX_REQUESTS_PER_CONNECTION: usize = 256;
/// Concurrent local clients. Excess connections receive a bounded error and
/// are closed before any request is read.
const MAX_CONNECTIONS: usize = 32;
/// A client that sends no complete request for this long is disconnected.
const CLIENT_IDLE_TIMEOUT: Duration = Duration::from_secs(60);
/// Writes cannot block a Host worker forever on a dead client.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);

#[cfg(unix)]
pub(super) fn socket_path() -> AppResult<std::path::PathBuf> {
    if let Some(override_path) = env::var_os("UKK_SOCKET") {
        return Ok(std::path::PathBuf::from(override_path));
    }
    let home = env::var_os("HOME").ok_or("HOME is required for the default socket path")?;
    Ok(std::path::PathBuf::from(home)
        .join(".ukk")
        .join("run")
        .join("agent-host.sock"))
}

#[cfg(unix)]
pub(super) fn serve() -> AppResult<()> {
    use std::fs;

    let path = socket_path()?;
    let parent = path.parent().ok_or("invalid socket path")?;
    let default_location = env::var_os("UKK_SOCKET").is_none();
    local_fs::secure_socket_directory(parent, default_location)
        .map_err(|error| format!("Host socket setup failed: {error}"))?;
    // Hold the lifecycle lock across the stale-socket probe, unlink and bind.
    // This is what makes "remove only if no live Host" race-free between Hosts.
    let _lock = local_fs::HostLock::acquire(parent)
        .map_err(|error| format!("Host lifecycle check failed: {error}"))?;
    local_fs::remove_stale_socket(&path)
        .map_err(|error| format!("Host endpoint recovery failed: {error}"))?;
    let listener = UnixListener::bind(&path)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    local_fs::verify_socket_permissions(&path)
        .map_err(|error| format!("Host socket is not private: {error}"))?;
    let auth = Arc::new(LocalAuth::install()?);
    let broker = Broker::new();
    broker.start();
    println!("ukk Host listening on {}", path.display());
    println!(
        "Security: same-user Unix peers only; socket 0600 in a 0700 directory; \
         max {MAX_CONNECTIONS} clients, {MAX_REQUEST_BYTES} byte requests"
    );
    let clients = Arc::new(AtomicUsize::new(0));
    for incoming in listener.incoming() {
        match incoming {
            Ok(stream) => {
                let Some(slot) = ConnectionSlot::acquire(&clients) else {
                    reject_unix_client(stream);
                    continue;
                };
                if let Err(error) = peer::verify_same_user(&stream) {
                    eprintln!("ukk Host rejected local client: {error}");
                    continue;
                }
                if let Err(error) = stream.set_read_timeout(Some(CLIENT_IDLE_TIMEOUT)) {
                    eprintln!("ukk Host could not set client read deadline: {error}");
                    continue;
                }
                if let Err(error) = stream.set_write_timeout(Some(WRITE_TIMEOUT)) {
                    eprintln!("ukk Host could not set client write deadline: {error}");
                    continue;
                }
                let broker = Arc::clone(&broker);
                let auth = Arc::clone(&auth);
                std::thread::spawn(move || {
                    let _slot = slot;
                    if let Err(error) = serve_one(stream, broker, auth) {
                        eprintln!("ukk Host client error: {error}");
                    }
                });
            }
            Err(error) => eprintln!("ukk Host accept error: {error}"),
        }
    }
    Ok(())
}

/// RAII counter slot. Dropping it releases capacity for a new client even when
/// a worker panics or a connection times out.
struct ConnectionSlot(Arc<AtomicUsize>);

impl ConnectionSlot {
    fn acquire(counter: &Arc<AtomicUsize>) -> Option<Self> {
        let mut current = counter.load(Ordering::Acquire);
        loop {
            if current >= MAX_CONNECTIONS {
                return None;
            }
            match counter.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Some(Self(Arc::clone(counter))),
                Err(next) => current = next,
            }
        }
    }
}

impl Drop for ConnectionSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

#[cfg(unix)]
fn reject_unix_client(mut stream: UnixStream) {
    let _ = stream.set_write_timeout(Some(Duration::from_secs(1)));
    let _ = write_json(&mut stream, &overloaded_error());
    eprintln!(
        "ukk Host rejected local client: concurrent connection limit ({MAX_CONNECTIONS}) reached"
    );
}

// This handler is shared by UnixStream (macOS/Linux) and Win32 Named Pipe
// PipeStream handles. Every client uses the same JSON-RPC dispatcher.
fn serve_one<S: Read + Write>(
    mut stream: S,
    broker: Arc<Broker>,
    auth: Arc<LocalAuth>,
) -> AppResult<()> {
    let mut pending: Vec<u8> = Vec::with_capacity(1024);
    let mut chunk = [0u8; 8192];
    let mut requests = 0usize;
    loop {
        while let Some(position) = pending.iter().position(|&byte| byte == b'\n') {
            if position > MAX_REQUEST_BYTES {
                return Err(format!("Host request exceeds {MAX_REQUEST_BYTES} bytes").into());
            }
            let line: Vec<u8> = pending.drain(..=position).collect();
            let response = dispatch_line(&line[..line.len() - 1], &broker, &auth);
            write_json(&mut stream, &response)?;
            requests += 1;
            if requests >= MAX_REQUESTS_PER_CONNECTION {
                return Ok(());
            }
        }
        if pending.len() > MAX_REQUEST_BYTES {
            return Err(format!("Host request exceeds {MAX_REQUEST_BYTES} bytes").into());
        }
        let read = match stream.read(&mut chunk) {
            Ok(0) => {
                return if pending.is_empty() {
                    Ok(())
                } else {
                    Err("Host request not line terminated before disconnect".into())
                };
            }
            Ok(read) => read,
            Err(error) if is_timeout(&error) => {
                return Err("Host client read deadline elapsed; closing connection".into());
            }
            Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => {
                // Normal client disconnect (including Windows ERROR_BROKEN_PIPE).
                return if pending.is_empty() {
                    Ok(())
                } else {
                    Err("Host request not line terminated before disconnect".into())
                };
            }
            Err(error) => return Err(error.into()),
        };
        pending.extend_from_slice(&chunk[..read]);
    }
}

fn dispatch_line(line: &[u8], broker: &Broker, auth: &LocalAuth) -> Value {
    let input: Value = match serde_json::from_slice(line) {
        Ok(value) => value,
        Err(_) => {
            return json!({
                "jsonrpc":"2.0","id":null,
                "error":{"code":-32700,"message":"invalid JSON"}
            })
        }
    };
    handle_request(&input, broker, auth)
}

fn overloaded_error() -> Value {
    json!({
        "jsonrpc":"2.0","id":null,
        "error":{"code":-32029,"message":"too many concurrent local Host clients"}
    })
}

fn is_timeout(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    )
}

#[cfg(windows)]
pub(super) fn serve_windows() -> AppResult<()> {
    use ukk_host_client::windows_pipe::{self, PipeStream};

    let name = windows_pipe::pipe_name()?;
    let broker = Broker::new();
    broker.start();
    println!("ukk Host listening on local Named Pipe {name}");
    println!(
        "Security: same-user Named Pipe clients only; protected DACL; remote pipe clients \
         rejected; max {MAX_CONNECTIONS} clients"
    );
    let auth = Arc::new(LocalAuth::install()?);
    let clients = Arc::new(AtomicUsize::new(0));
    let mut pending = windows_pipe::create_server_pipe(&name, true)?;
    loop {
        windows_pipe::accept_client(&pending)?;
        // Install the replacement instance before detaching the connected
        // handle so the pipe name is never absent and cannot be name-squatted.
        let replacement = windows_pipe::create_server_pipe(&name, false)?;
        let connected = std::mem::replace(&mut pending, replacement);
        let Some(slot) = ConnectionSlot::acquire(&clients) else {
            let mut stream = PipeStream::new(connected, CLIENT_IDLE_TIMEOUT, WRITE_TIMEOUT);
            let _ = write_json(&mut stream, &overloaded_error());
            eprintln!(
                "ukk Host rejected Named Pipe client: concurrent connection limit \
                 ({MAX_CONNECTIONS}) reached"
            );
            continue;
        };
        if let Err(error) = windows_pipe::verify_client_same_user(&connected) {
            eprintln!("ukk Host rejected Named Pipe client: {error}");
            continue;
        }
        let broker = Arc::clone(&broker);
        let auth = Arc::clone(&auth);
        std::thread::spawn(move || {
            let _slot = slot;
            let stream = PipeStream::new(connected, CLIENT_IDLE_TIMEOUT, WRITE_TIMEOUT);
            if let Err(error) = serve_one(stream, broker, auth) {
                eprintln!("ukk Host named-pipe client error: {error}");
            }
        });
    }
}

fn write_json<W: Write>(writer: &mut W, response: &Value) -> AppResult<()> {
    let mut bytes = serde_json::to_vec(response)?;
    if bytes.len() + 1 > MAX_REPLY_BYTES {
        bytes = serde_json::to_vec(&json!({
            "jsonrpc":"2.0","id":null,
            "error":{"code":-32012,"message":"Host reply exceeded size cap"}
        }))?;
    }
    bytes.push(b'\n');
    writer.write_all(&bytes)?;
    writer.flush()?;
    Ok(())
}

/// Read one newline-terminated frame with a hard byte cap.
fn read_line_bounded<S: Read>(stream: &mut S, max: usize) -> AppResult<Vec<u8>> {
    let mut bytes = Vec::with_capacity(2048);
    let mut chunk = [0u8; 8192];
    loop {
        let read = stream
            .read(&mut chunk)
            .map_err(|error| format!("Host response read failed: {error}"))?;
        if read == 0 {
            return Err("ukk Host closed before a complete response".into());
        }
        if let Some(position) = chunk[..read].iter().position(|&byte| byte == b'\n') {
            if bytes.len() + position > max {
                return Err("Host response too large".into());
            }
            bytes.extend_from_slice(&chunk[..position]);
            return Ok(bytes);
        }
        if bytes.len() + read > max {
            return Err("Host response too large".into());
        }
        bytes.extend_from_slice(&chunk[..read]);
    }
}

#[cfg(unix)]
pub(super) fn request(method: &str) -> AppResult<()> {
    let stream = UnixStream::connect(socket_path()?)?;
    stream.set_read_timeout(Some(Duration::from_secs(13)))?;
    stream.set_write_timeout(Some(Duration::from_secs(3)))?;
    request_stream(stream, method)
}

#[cfg(windows)]
pub(super) fn request_windows(method: &str) -> AppResult<()> {
    let mut pipe = ukk_host_client::windows_pipe::open_client()?;
    pipe.set_read_timeout(Duration::from_secs(13));
    request_stream(pipe, method)
}

fn request_stream<S: Read + Write>(mut stream: S, method: &str) -> AppResult<()> {
    write_json(
        &mut stream,
        &json!({"jsonrpc":"2.0","id":1,"method":method,"params":{}}),
    )?;
    let bytes = read_line_bounded(&mut stream, MAX_REPLY_BYTES)?;
    let value: Value = serde_json::from_slice(&bytes)?;
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}
