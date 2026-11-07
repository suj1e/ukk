//! Private user-scoped transport: Unix Domain Socket or Win32 Named Pipe.
use super::*;

#[cfg(unix)]
fn socket_path() -> AppResult<std::path::PathBuf> {
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
    fs::create_dir_all(parent)?;
    // For the default location: user-only directory. A custom UKK_SOCKET
    // path is an explicit developer override; protect its parent yourself.
    if env::var_os("UKK_SOCKET").is_none() {
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    }
    // Never unlink an existing socket: another Host might own it.
    let listener = UnixListener::bind(&path)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    let auth = Arc::new(LocalAuth::install()?);
    let broker = Broker::new();
    broker.start();
    println!("ukk Host listening on {}", path.display());
    println!("Methods: read-only snapshots + authorized local Session / Pane metadata edits");
    for incoming in listener.incoming() {
        match incoming {
            Ok(stream) => {
                let broker = Arc::clone(&broker);
                let auth = Arc::clone(&auth);
                std::thread::spawn(move || {
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

// This handler is shared by UnixStream (macOS/Linux) and Win32 Named Pipe
// File handles. Every client uses the same read-only JSON-RPC dispatcher.
fn serve_one<S: Read + Write>(mut stream: S, broker: Arc<Broker>, auth: Arc<LocalAuth>) -> AppResult<()> {
    const MAX_LINE: usize = 64 * 1024;
    loop {
        let mut bytes = Vec::new();
        loop {
            let mut byte = [0u8; 1];
            if stream.read(&mut byte)? == 0 {
                if bytes.is_empty() {
                    return Ok(());
                }
                return Err("Host request not line terminated".into());
            }
            bytes.push(byte[0]);
            if bytes.len() > MAX_LINE {
                return Err("Host request too large".into());
            }
            if byte[0] == b'\n' {
                break;
            }
        }
        let input: Value = match serde_json::from_slice(&bytes) {
            Ok(value) => value,
            Err(_) => {
                write_json(&mut stream, &json!({
                    "jsonrpc":"2.0","id":null,
                    "error":{"code":-32700,"message":"invalid JSON"}
                }))?;
                continue;
            }
        };
        let result = handle_request(&input, &broker, &auth);
        write_json(&mut stream, &result)?;
    }
}

#[cfg(windows)]
pub(super) fn serve_windows() -> AppResult<()> {
    let name = windows_pipe::pipe_name()?;
    let broker = Broker::new();
    broker.start();
    println!("ukk Host listening on local Named Pipe {name}");
    println!("Methods: snapshots + authorized Session / Pane edits");
    let mut pending = windows_pipe::create_server_pipe(&name, true)?;
    let auth = Arc::new(LocalAuth::install()?);
    loop {
        windows_pipe::accept_client(&pending)?;
        // Keep a replacement server instance open *before* handing off the
        // connected handle. Otherwise the pipe namespace can briefly vanish
        // between connections and become available for name squatting.
        let replacement = windows_pipe::create_server_pipe(&name, false)?;
        let connected = std::mem::replace(&mut pending, replacement);
        let broker = Arc::clone(&broker);
        let auth = Arc::clone(&auth);
        std::thread::spawn(move || {
            if let Err(error) = serve_one(connected, broker, auth) {
                eprintln!("ukk Host named-pipe client error: {error}");
            }
        });
    }
}

fn write_json<W: Write>(writer: &mut W, response: &Value) -> AppResult<()> {
    serde_json::to_writer(&mut *writer, response)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

#[cfg(unix)]
pub(super) fn request(method: &str) -> AppResult<()> {
    let stream = UnixStream::connect(socket_path()?)?;
    request_stream(stream, method)
}

#[cfg(windows)]
pub(super) fn request_windows(method: &str) -> AppResult<()> {
    let pipe = windows_pipe::open_client()?;
    request_stream(pipe, method)
}

fn request_stream<S: Read + Write>(mut stream: S, method: &str) -> AppResult<()> {
    write_json(
        &mut stream,
        &json!({"jsonrpc":"2.0","id":1,"method":method,"params":{}}),
    )?;
    // A bounded response protects the developer command from excessive data.
    const MAX_BYTES: usize = 4 * 1024 * 1024;
    let mut bytes = Vec::new();
    loop {
        let mut byte = [0u8; 1];
        if stream.read(&mut byte)? == 0 {
            return Err("ukk Host closed before response".into());
        }
        bytes.push(byte[0]);
        if bytes.len() > MAX_BYTES {
            return Err("Host response too large".into());
        }
        if byte[0] == b'\n' {
            break;
        }
    }
    let value: Value = serde_json::from_slice(&bytes)?;
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}
