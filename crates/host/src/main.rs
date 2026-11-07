//! Local, read-only integration spike: GPUI -> ukk Host -> herdr-core.
//!
//! The prototype uses a private Unix-domain socket on macOS/Linux and an
//! ACL-protected Named Pipe on Windows. It exposes no public network server,
//! remote pairing, agent mutation or plugin execution. The in-memory event
//! broker is not yet a durable, authenticated cross-device event service.

mod broker;

use broker::Broker;
use serde_json::{json, Value};
use std::env;
use std::error::Error;
use std::io::{Read, Write};
use std::sync::Arc;
#[cfg(windows)]
use ukk_host_client::windows_pipe;

use ukk_hc_client::HcClient;
use ukk_domain::{PaneContent, SplitAxis};
use ukk_store::Store;

#[cfg(unix)]
use std::os::unix::{
    fs::PermissionsExt,
    net::{UnixListener, UnixStream},
};

type AppResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

fn main() {
    if let Err(error) = execute() {
        eprintln!("ukk-host: {error}");
        std::process::exit(1);
    }
}

fn execute() -> AppResult<()> {
    let command = env::args().nth(1).unwrap_or_else(|| "help".to_owned());
    match command.as_str() {
        "doctor" => {
            let response = HcClient::default().call("terminal.list", json!({}))?;
            let count = response
                .get("terminals")
                .and_then(Value::as_array)
                .map(Vec::len)
                .ok_or("hc returned no terminals array")?;
            println!("Connected to herdr-core. {count} visible terminals.");
            Ok(())
        }
        "list" => {
            let result = HcClient::default().call("terminal.list", json!({}))?;
            println!("{}", serde_json::to_string_pretty(&result)?);
            Ok(())
        }
        "schema" => {
            let result = HcClient::default().call("rpc.discover", json!({}))?;
            println!("{}", serde_json::to_string_pretty(&result)?);
            Ok(())
        }
        "watch" => HcClient::default()
            .watch(|event| {
                println!("{event}");
                std::io::stdout().flush()?;
                Ok(())
            })
            .map_err(Into::into),
        "workspace-dump" => {
            let snapshot = Store::open_default()?.snapshot()?;
            println!("{}", serde_json::to_string_pretty(&snapshot)?);
            Ok(())
        }
        "space-create" => {
            let mut args = env::args().skip(2);
            let title = args.next().ok_or("space-create <title> <project_root>")?;
            let root = args.next().ok_or("space-create <title> <project_root>")?;
            let space = Store::open_default()?.create_space(&title, &root)?;
            println!("{}", serde_json::to_string_pretty(&space)?);
            Ok(())
        }
        "session-create" => {
            let mut args = env::args().skip(2);
            let space_id = args.next().ok_or("session-create <space_id> <title>")?;
            let title = args.next().ok_or("session-create <space_id> <title>")?;
            let session = Store::open_default()?.create_session(&space_id, &title)?;
            println!("{}", serde_json::to_string_pretty(&session)?);
            Ok(())
        }
        "session-archive" => {
            let id = env::args().nth(2).ok_or("session-archive <session_id>")?;
            Store::open_default()?.archive_session(&id)?;
            println!("Session archived; all herdr-core agents continue running.");
            Ok(())
        }
        "agent-bind" => {
            let mut args = env::args().skip(2);
            let session_id = args.next().ok_or("agent-bind <session_id> <core_id> <terminal_address> <adapter> [parent_run_id]")?;
            let core_id = args.next().ok_or("missing herdr-core identity")?;
            let address = args.next().ok_or("missing hc terminal address")?;
            let adapter = args.next().ok_or("missing agent adapter")?;
            let parent_id = args.next();
            let run = Store::open_default()?.bind_agent(
                &session_id, &core_id, &address, &adapter, parent_id.as_deref(),
            )?;
            println!("{}", serde_json::to_string_pretty(&run)?);
            Ok(())
        }
        "pane-split" => {
            let mut args = env::args().skip(2);
            let session_id = args.next().ok_or("pane-split <session_id> <pane_id> <horizontal|vertical> <activity|files|diff|agent|terminal> [agent_run_id]")?;
            let pane_id = args.next().ok_or("missing pane_id")?;
            let axis = match args.next().as_deref() {
                Some("horizontal") => SplitAxis::Horizontal,
                Some("vertical") => SplitAxis::Vertical,
                _ => return Err("axis must be horizontal or vertical".into()),
            };
            let content = match args.next().as_deref() {
                Some("activity") => PaneContent::Activity,
                Some("files") => PaneContent::Files,
                Some("diff") => PaneContent::Diff,
                Some("agent") => PaneContent::Agent {
                    agent_run_id: args.next().ok_or("agent pane requires agent_run_id")?,
                },
                Some("terminal") => PaneContent::Terminal {
                    agent_run_id: args.next().ok_or("terminal pane requires agent_run_id")?,
                },
                _ => return Err("unknown pane surface".into()),
            };
            let mut store = Store::open_default()?;
            let layout = store.get_layout(&session_id)?;
            let root = layout.root.split_leaf(&pane_id, axis, 0.5, content)?;
            let updated = store.set_layout(&session_id, layout.revision, root)?;
            println!("{}", serde_json::to_string_pretty(&updated)?);
            Ok(())
        }
        "pane-close" => {
            let mut args = env::args().skip(2);
            let session_id = args.next().ok_or("pane-close <session_id> <pane_id>")?;
            let pane_id = args.next().ok_or("missing pane_id")?;
            let mut store = Store::open_default()?;
            let layout = store.get_layout(&session_id)?;
            let root = layout.root.close_leaf(&pane_id)?;
            let updated = store.set_layout(&session_id, layout.revision, root)?;
            println!("{}", serde_json::to_string_pretty(&updated)?);
            Ok(())
        }
        #[cfg(unix)]
        "serve" => serve(),
        #[cfg(windows)]
        "serve" => serve_windows(),
        #[cfg(windows)]
        "request" => {
            let method = env::args().nth(2).unwrap_or_else(|| "host.ping".into());
            request_windows(&method)
        },
        #[cfg(unix)]
        "request" => {
            let method = env::args().nth(2).unwrap_or_else(|| "host.ping".into());
            request(&method)
        }
        "help" | "--help" | "-h" => {
            print_help();
            Ok(())
        }
        _ => Err(format!("unknown command: {command}").into()),
    }
}

fn print_help() {
    println!(
        "ukk-host (read-only integration spike)\n\
        Commands:\n\
          doctor             Test hc connect + terminal.list\n\
          list               Print hc terminal snapshot\n\
          schema             Print hc OpenRPC schema\n\
          watch              Watch hc events/heartbeat/missed notifications\n\
          workspace-dump    Print persisted Space / Session / Pane layout JSON\n\
          space-create       Create Space: <title> <project_root>\n\
          session-create     Create Session: <space_id> <title>\n\
          session-archive    Archive Session: <session_id> (never stops Agent)\n\
          agent-bind         Bind existing terminal: <session_id> <core_id> <address> <adapter> [parent_id]\n\
          pane-split         Split view: <session_id> <pane_id> <axis> <surface> [agent_run_id]\n\
          pane-close         Close view: <session_id> <pane_id> (never stops Agent)\n\
          serve              Start local IPC (Unix socket / Windows Named Pipe)\n\
          request [method]   Query local Host RPC (default host.ping)\n\
        Supported Host RPC methods: host.ping, terminal.list, host.snapshot, host.wait\n\
        Environment: UKK_HC_BIN, UKK_DB_PATH; UKK_SOCKET only on macOS/Linux"
    );
}

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
fn serve() -> AppResult<()> {
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
    let broker = Broker::new();
    broker.start();
    println!("ukk Host listening on {}", path.display());
    println!("Read-only methods: host.ping, terminal.list, host.snapshot, host.wait, workspace.snapshot");
    for incoming in listener.incoming() {
        match incoming {
            Ok(stream) => {
                let broker = Arc::clone(&broker);
                std::thread::spawn(move || {
                    if let Err(error) = serve_one(stream, broker) {
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
fn serve_one<S: Read + Write>(mut stream: S, broker: Arc<Broker>) -> AppResult<()> {
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
        let result = handle_request(&input, &broker);
        write_json(&mut stream, &result)?;
    }
}

#[cfg(windows)]
fn serve_windows() -> AppResult<()> {
    let name = windows_pipe::pipe_name()?;
    let broker = Broker::new();
    broker.start();
    println!("ukk Host listening on local Named Pipe {name}");
    println!("Read-only methods: host.ping, terminal.list, host.snapshot, host.wait");
    let mut pending = windows_pipe::create_server_pipe(&name, true)?;
    loop {
        windows_pipe::accept_client(&pending)?;
        // Keep a replacement server instance open *before* handing off the
        // connected handle. Otherwise the pipe namespace can briefly vanish
        // between connections and become available for name squatting.
        let replacement = windows_pipe::create_server_pipe(&name, false)?;
        let connected = std::mem::replace(&mut pending, replacement);
        let broker = Arc::clone(&broker);
        std::thread::spawn(move || {
            if let Err(error) = serve_one(connected, broker) {
                eprintln!("ukk Host named-pipe client error: {error}");
            }
        });
    }
}

fn handle_request(req: &Value, broker: &Broker) -> Value {
    let id = req.get("id").cloned().unwrap_or(Value::Null);
    if req.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
        || req.get("method").and_then(Value::as_str).is_none()
        || id.is_null()
    {
        return json!({
            "jsonrpc":"2.0","id":id,
            "error":{"code":-32600,"message":"invalid JSON-RPC request"}
        });
    }
    match req["method"].as_str().unwrap_or_default() {
        "host.ping" => json!({
            "jsonrpc":"2.0","id":id,
            "result":{"status":"ok","protocol":"ukk-local-spike-1","read_only":true}
        }),
        "workspace.snapshot" => {
            match Store::open_default().and_then(|store| store.snapshot()) {
                Ok(snapshot) => json!({"jsonrpc":"2.0","id":id,"result":snapshot}),
                Err(error) => json!({"jsonrpc":"2.0","id":id,
                    "error":{"code":-32011,"message":error.to_string()}}),
            }
        },
        "host.snapshot" => json!({"jsonrpc":"2.0","id":id,"result":broker.snapshot()}),
        "host.wait" => {
            let params = req.get("params");
            let after_epoch = params
                .and_then(|p| p.get("after_epoch"))
                .and_then(Value::as_str);
            let after_revision = params
                .and_then(|p| p.get("after_revision"))
                .and_then(Value::as_u64)
                .unwrap_or_default();
            let timeout_ms = params
                .and_then(|p| p.get("timeout_ms"))
                .and_then(Value::as_u64)
                .unwrap_or(10_000);
            json!({"jsonrpc":"2.0","id":id,
                   "result":broker.wait(after_epoch, after_revision, timeout_ms)})
        }
        "terminal.list" => match HcClient::default().call("terminal.list", json!({})) {
            Ok(result) => json!({"jsonrpc":"2.0","id":id,"result":result}),
            Err(error) => json!({
                "jsonrpc":"2.0","id":id,
                "error":{"code":-32001,"message":error.to_string()}
            }),
        },
        _ => json!({
            "jsonrpc":"2.0","id":id,
            "error":{"code":-32601,"message":"method not found"}
        }),
    }
}

fn write_json<W: Write>(writer: &mut W, response: &Value) -> AppResult<()> {
    serde_json::to_writer(&mut *writer, response)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

#[cfg(unix)]
fn request(method: &str) -> AppResult<()> {
    let stream = UnixStream::connect(socket_path()?)?;
    request_stream(stream, method)
}

#[cfg(windows)]
fn request_windows(method: &str) -> AppResult<()> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ping_is_read_only() {
        let broker = Broker::new();
        let response = handle_request(&json!({
            "jsonrpc":"2.0","id":19,"method":"host.ping","params":{}
        }), &broker);
        assert_eq!(response["id"], 19);
        assert_eq!(response["result"]["read_only"], true);
    }

    #[test]
    fn rejects_unknown_or_mutating_methods() {
        let broker = Broker::new();
        let response = handle_request(&json!({
            "jsonrpc":"2.0","id":20,"method":"terminal.kill","params":{"terminal":"t1"}
        }), &broker);
        assert_eq!(response["error"]["code"], -32601);
    }

    #[test]
    fn snapshot_starts_disconnected() {
        let broker = Broker::new();
        let response = handle_request(&json!({
            "jsonrpc":"2.0","id":30,"method":"host.snapshot","params":{}
        }), &broker);
        assert_eq!(response["result"]["revision"], 0);
        assert_eq!(response["result"]["connected"], false);
        assert_eq!(response["result"]["terminals"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn wait_returns_immediately_for_new_host_epoch() {
        let broker = Broker::new();
        let response = handle_request(&json!({
            "jsonrpc":"2.0","id":31,"method":"host.wait",
            "params":{"after_epoch":"previous-host","after_revision":0,"timeout_ms":10000}
        }), &broker);
        assert_eq!(response["result"]["revision"], 0);
    }

    #[test]
    fn rejects_invalid_requests() {
        let broker = Broker::new();
        let response = handle_request(&json!({"method":"host.ping"}), &broker);
        assert_eq!(response["error"]["code"], -32600);
    }
}
