//! First, deliberately small hc protocol adapter.
//!
//! Communicates with `hc connect` using newline-delimited JSON-RPC 2.0.
//! This is the plain mode of herdr-core's preview protocol (generation 1).
//! This crate does NOT implement framed terminal.attach or any mutating methods.

use serde_json::{json, Value};
use std::env;
use std::error::Error;
use std::ffi::OsString;
use std::fmt;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};

#[derive(Debug)]
pub enum HcError {
    Io(std::io::Error),
    Json(serde_json::Error),
    Protocol(String),
    Remote(Value),
}

impl fmt::Display for HcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "hc I/O error: {error}"),
            Self::Json(error) => write!(f, "hc JSON error: {error}"),
            Self::Protocol(message) => write!(f, "hc protocol error: {message}"),
            Self::Remote(error) => write!(f, "hc RPC error: {error}"),
        }
    }
}

impl Error for HcError {}

impl From<std::io::Error> for HcError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for HcError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

#[derive(Debug, Clone)]
pub struct HcClient {
    binary: OsString,
}

impl Default for HcClient {
    fn default() -> Self {
        Self::new(env::var_os("UKK_HC_BIN").unwrap_or_else(|| OsString::from("hc")))
    }
}

impl HcClient {
    pub fn new(binary: impl Into<OsString>) -> Self {
        Self {
            binary: binary.into(),
        }
    }

    fn spawn(&self) -> Result<Child, HcError> {
        Ok(Command::new(&self.binary)
            .arg("connect")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?)
    }

    /// Execute a single, non-streaming JSON-RPC request on a fresh connection.
    ///
    /// Currently used only for `terminal.list` and `rpc.discover`.
    pub fn call(&self, method: &str, params: Value) -> Result<Value, HcError> {
        let mut child = self.spawn()?;
        let result = (|| {
            write_request(&mut child, 1, method, params)?;
            let stdout = child
                .stdout
                .take()
                .ok_or_else(|| HcError::Protocol("hc stdout unavailable".into()))?;
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            if reader.read_line(&mut line)? == 0 {
                return Err(HcError::Protocol("hc closed before responding".into()));
            }
            parse_reply(&line, 1)
        })();
        // The one-request connection is no longer needed. This never kills the
        // independent herdr-core daemon or any of its persistent terminals.
        let _ = child.kill();
        let _ = child.wait();
        result
    }

    /// Subscribe to hc's *plain-mode* notification stream.
    ///
    /// The callback receives the initial snapshot as `{"kind":"snapshot","data":...}`,
    /// then `{"kind":"notification","data":...}` for event/heartbeat/missed.
    /// A `missed` message requires a new authoritative snapshot. hc's seq
    /// resets on core restart: never persist it as ukk's durable event cursor.
    pub fn watch<F>(&self, mut emit: F) -> Result<(), HcError>
    where
        F: FnMut(Value) -> Result<(), HcError>,
    {
        let mut child = self.spawn()?;
        let result = (|| {
            write_request(&mut child, 1, "events.subscribe", json!({}))?;
            let stdout = child
                .stdout
                .take()
                .ok_or_else(|| HcError::Protocol("hc stdout unavailable".into()))?;
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            if reader.read_line(&mut line)? == 0 {
                return Err(HcError::Protocol(
                    "hc closed before subscription snapshot".into(),
                ));
            }
            let snapshot = parse_reply(&line, 1)?;
            emit(json!({"kind":"snapshot","data":snapshot}))?;
            loop {
                line.clear();
                if reader.read_line(&mut line)? == 0 {
                    return Err(HcError::Protocol(
                        "hc notification stream disconnected".into(),
                    ));
                }
                let value: Value = serde_json::from_str(&line)?;
                if value.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
                    return Err(HcError::Protocol("invalid JSON-RPC notification".into()));
                }
                if value.get("id").is_some() {
                    return Err(HcError::Protocol(
                        "unexpected response on notification stream".into(),
                    ));
                }
                if !matches!(
                    value.get("method").and_then(Value::as_str),
                    Some("event" | "heartbeat" | "missed")
                ) {
                    return Err(HcError::Protocol("unexpected notification method".into()));
                }
                emit(json!({"kind":"notification","data":value}))?;
            }
        })();
        let _ = child.kill();
        let _ = child.wait();
        result
    }
}

fn write_request(child: &mut Child, id: u64, method: &str, params: Value) -> Result<(), HcError> {
    let stdin = child
        .stdin
        .as_mut()
        .ok_or_else(|| HcError::Protocol("hc stdin unavailable".into()))?;
    serde_json::to_writer(
        &mut *stdin,
        &json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}),
    )?;
    stdin.write_all(b"\n")?;
    stdin.flush()?;
    Ok(())
}

fn parse_reply(line: &str, id: u64) -> Result<Value, HcError> {
    let value: Value = serde_json::from_str(line)?;
    if value.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(HcError::Protocol("response is not JSON-RPC 2.0".into()));
    }
    if value.get("id") != Some(&json!(id)) {
        return Err(HcError::Protocol(format!(
            "unexpected response id: {:?}",
            value.get("id")
        )));
    }
    if let Some(error) = value.get("error") {
        return Err(HcError::Remote(error.clone()));
    }
    value
        .get("result")
        .cloned()
        .ok_or_else(|| HcError::Protocol("response missing result".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_terminal_list_result() {
        let line = r#"{"jsonrpc":"2.0","id":1,"result":{"terminals":[{"address":"t3","argv":["codex"],"cwd":"/work","state":"live","created_ms":1,"screen":"exact","cols":80,"rows":24,"labels":{}}],"hidden_finished":0,"machines":[]}}"#;
        let result = parse_reply(line, 1).expect("valid reply");
        assert_eq!(result["terminals"][0]["address"], "t3");
    }

    #[test]
    fn preserves_remote_errors() {
        let line = r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32000,"message":"unavailable","data":{"kind":"unavailable"}}}"#;
        match parse_reply(line, 1) {
            Err(HcError::Remote(e)) => assert_eq!(e["data"]["kind"], "unavailable"),
            other => panic!("unexpected result: {other:?}"),
        }
    }

    #[test]
    fn rejects_wrong_request_id() {
        let line = r#"{"jsonrpc":"2.0","id":2,"result":{}}"#;
        assert!(matches!(parse_reply(line, 1), Err(HcError::Protocol(_))));
    }

    #[test]
    fn rejects_non_json_rpc() {
        assert!(matches!(
            parse_reply(r#"{"id":1,"result":{}}"#, 1),
            Err(HcError::Protocol(_))
        ));
    }

    #[test]
    fn rejects_missing_result() {
        assert!(matches!(
            parse_reply(r#"{"jsonrpc":"2.0","id":1}"#, 1),
            Err(HcError::Protocol(_))
        ));
    }
}
