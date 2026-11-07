//! Local, read-only integration spike: GPUI -> ukk Host -> herdr-core.
//!
//! The prototype uses a private Unix-domain socket. It deliberately does not
//! expose a network server, remote pairing, agent mutation, or plugin execution.
//! A production Host will need durable state, identity, permissions and an
//! event broker before the mobile client can connect.

use serde_json::{json, Value};
use std::env;
use std::error::Error;
use std::io::{BufRead, BufReader, Read, Write};

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
          serve              Start a private, local Unix socket (macOS/Linux)\n\
          request [method]   Query the local Host socket (default host.ping)\n\
        Supported Host RPC methods: host.ping, terminal.list\n\
        Environment: UKK_HC_BIN (hc executable), UKK_SOCKET (local socket path)"
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
    println!("ukk Host listening on {}", path.display());
    println!("Read-only methods: host.ping, terminal.list");
    for incoming in listener.incoming() {
        match incoming {
            Ok(stream) => {
                std::thread::spawn(move || {
                    if let Err(error) = serve_one(stream) {
                        eprintln!("ukk Host client error: {error}");
                    }
                });
            }
            Err(error) => eprintln!("ukk Host accept error: {error}"),
        }
    }
    Ok(())
}

#[cfg(unix)]
fn serve_one(stream: UnixStream) -> AppResult<()> {
    const MAX_LINE: usize = 64 * 1024;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut writer = stream;
    loop {
        // Bounded read: do not let a local client allocate unbounded memory.
        let mut bytes = Vec::new();
        let read = reader.by_ref().take((MAX_LINE + 1) as u64).read_until(b'\n', &mut bytes)?;
        if read == 0 {
            return Ok(());
        }
        if bytes.len() > MAX_LINE || !bytes.ends_with(b"\n") {
            return Err("Host request too large or not line terminated".into());
        }
        let input: Value = match serde_json::from_slice(&bytes) {
            Ok(value) => value,
            Err(_) => {
                write_json(&mut writer, &json!({
                    "jsonrpc":"2.0","id":null,
                    "error":{"code":-32700,"message":"invalid JSON"}
                }))?;
                continue;
            }
        };
        let result = handle_request(&input);
        write_json(&mut writer, &result)?;
    }
}

#[cfg(unix)]
fn handle_request(req: &Value) -> Value {
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

#[cfg(unix)]
fn write_json<W: Write>(writer: &mut W, response: &Value) -> AppResult<()> {
    serde_json::to_writer(&mut *writer, response)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

#[cfg(unix)]
fn request(method: &str) -> AppResult<()> {
    let mut stream = UnixStream::connect(socket_path()?)?;
    write_json(
        &mut stream,
        &json!({"jsonrpc":"2.0","id":1,"method":method,"params":{}}),
    )?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Err("ukk Host socket closed before response".into());
    }
    let value: Value = serde_json::from_str(&line)?;
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;

    #[test]
    fn ping_is_read_only() {
        let response = handle_request(&json!({
            "jsonrpc":"2.0","id":19,"method":"host.ping","params":{}
        }));
        assert_eq!(response["id"], 19);
        assert_eq!(response["result"]["read_only"], true);
    }

    #[test]
    fn rejects_unknown_or_mutating_methods() {
        let response = handle_request(&json!({
            "jsonrpc":"2.0","id":20,"method":"terminal.kill","params":{"terminal":"t1"}
        }));
        assert_eq!(response["error"]["code"], -32601);
    }

    #[test]
    fn rejects_invalid_requests() {
        let response = handle_request(&json!({"method":"host.ping"}));
        assert_eq!(response["error"]["code"], -32600);
    }
}
