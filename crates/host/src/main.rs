//! Local, read-only integration spike: GPUI -> ukk Host -> herdr-core.
//!
//! The prototype uses a private Unix-domain socket. It deliberately does not
//! expose a network server, remote pairing, agent mutation, or plugin execution.
//! A production Host will need durable state, identity, permissions and an
//! event broker before the mobile client can connect.

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
          serve              Start local IPC (Unix socket / Windows Named Pipe)\n\
          request [method]   Query local Host RPC (default host.ping)\n\
        Supported Host RPC methods: host.ping, terminal.list, host.snapshot, host.wait\n\
        Environment: UKK_HC_BIN; UKK_SOCKET applies to macOS/Linux only"
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
    println!("Read-only methods: host.ping, terminal.list, host.snapshot, host.wait");
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
    let mut first = true;
    loop {
        let pipe = windows_pipe::create_server_pipe(&name, first)?;
        first = false;
        windows_pipe::accept_client(&pipe)?;
        let broker = Arc::clone(&broker);
        std::thread::spawn(move || {
            if let Err(error) = serve_one(pipe, broker) {
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
