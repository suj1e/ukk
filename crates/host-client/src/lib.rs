//! Safe, read-only local IPC adapter for ukk Agent Host.
//!
//! This bootstrap IPC is local only: Unix sockets on macOS/Linux, a private
//! per-user Named Pipe on Windows. It is not the public remote protocol.
//! A connection is opened for every request and is bounded by time and size.

use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
#[cfg(unix)]
use std::env;
use std::error::Error;
use std::fmt;
#[cfg(any(unix, windows))]
use std::io::{BufReader, Read, Write};
#[cfg(windows)]
pub mod windows_pipe;
use std::path::PathBuf;
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::net::UnixStream;

const MAX_REPLY_BYTES: usize = 4 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug)]
pub enum HostError {
    Io(std::io::Error),
    Json(serde_json::Error),
    Protocol(String),
    Remote(Value),
    UnsupportedPlatform,
}

impl fmt::Display for HostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "Host I/O error: {error}"),
            Self::Json(error) => write!(f, "Host JSON error: {error}"),
            Self::Protocol(error) => write!(f, "Host protocol error: {error}"),
            Self::Remote(error) => write!(f, "Host RPC error: {error}"),
            Self::UnsupportedPlatform => write!(f, "local Unix socket is not supported on this platform"),
        }
    }
}

impl Error for HostError {}

impl From<std::io::Error> for HostError {
    fn from(error: std::io::Error) -> Self { Self::Io(error) }
}

impl From<serde_json::Error> for HostError {
    fn from(error: serde_json::Error) -> Self { Self::Json(error) }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Terminal {
    pub address: String,
    pub name: Option<String>,
    pub title: String,
    pub cwd: String,
    pub argv: Vec<String>,
    pub lifecycle: String,
    pub parent: Option<String>,
    /// Application-reported OSC 7501 state, NOT inferred from terminal text.
    pub reported_status: Option<String>,
    pub reported_app: Option<String>,
}

impl Terminal {
    pub fn display_name(&self) -> &str {
        self.name
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .or_else(|| (!self.title.is_empty()).then_some(self.title.as_str()))
            .unwrap_or(&self.address)
    }

    pub fn state_label(&self) -> &str {
        if self.lifecycle == "finished" {
            "Finished"
        } else {
            match self.reported_status.as_deref() {
                Some("working") => "Working",
                Some("blocked") => "Needs you",
                Some("idle") => "Idle",
                Some("done") => "Done",
                Some("error") => "Error",
                _ => "Live · status unknown",
            }
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct HostSnapshot {
    pub terminals: Vec<Terminal>,
    pub hidden_finished: u64,
    /// Number of machine records advertised by hc; not interpreted as hosts
    /// managed by ukk yet.
    pub machine_count: usize,
}

impl HostSnapshot {
    /// Present terminals as a dynamic tree, using hc's actual parent field.
    ///
    /// Orphan and cyclic references remain visible as top-level entries.
    /// Iterative traversal avoids recursive stack growth from malformed data.
    pub fn hierarchy(&self) -> Vec<(&Terminal, usize)> {
        let mut by_parent: HashMap<&str, Vec<usize>> = HashMap::new();
        let known: HashSet<&str> = self
            .terminals
            .iter()
            .map(|terminal| terminal.address.as_str())
            .collect();
        for (index, terminal) in self.terminals.iter().enumerate() {
            if let Some(parent) = terminal.parent.as_deref() {
                by_parent.entry(parent).or_default().push(index);
            }
        }
        let roots = self
            .terminals
            .iter()
            .enumerate()
            .filter(|(_, terminal)| terminal.parent.as_deref().is_none_or(|p| !known.contains(p)))
            .map(|(index, _)| index);
        let seeds = roots.chain(0..self.terminals.len());
        let mut seen = HashSet::new();
        let mut output = Vec::with_capacity(self.terminals.len());
        for seed in seeds {
            if seen.contains(&seed) {
                continue;
            }
            let mut stack = vec![(seed, 0_usize)];
            while let Some((index, depth)) = stack.pop() {
                if !seen.insert(index) {
                    continue;
                }
                let current = &self.terminals[index];
                output.push((current, depth));
                if let Some(children) = by_parent.get(current.address.as_str()) {
                    for &child in children.iter().rev() {
                        if !seen.contains(&child) {
                            stack.push((child, depth.saturating_add(1)));
                        }
                    }
                }
            }
        }
        output
    }

    pub fn from_result(result: &Value) -> Result<Self, HostError> {
        let rows = result
            .get("terminals")
            .and_then(Value::as_array)
            .ok_or_else(|| HostError::Protocol("terminal.list result missing terminals array".into()))?;

        let mut terminals = Vec::with_capacity(rows.len());
        for row in rows {
            let address = required_str(row, "address")?.to_owned();
            let lifecycle = required_str(row, "state")?.to_owned();
            let status = row.get("status").filter(|status| !status.is_null());
            let reported_status = status
                .and_then(|record| record.get("state"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            let reported_app = status
                .and_then(|record| record.get("app"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            let argv = row
                .get("argv")
                .and_then(Value::as_array)
                .ok_or_else(|| HostError::Protocol(format!("terminal {address} missing argv array")))?
                .iter()
                .map(|entry| entry.as_str().map(str::to_owned))
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| HostError::Protocol(format!("terminal {address} has invalid argv")))?;

            terminals.push(Terminal {
                address,
                name: optional_str(row, "name"),
                title: optional_str(row, "title").unwrap_or_default(),
                cwd: required_str(row, "cwd")?.to_owned(),
                argv,
                lifecycle,
                parent: optional_str(row, "parent"),
                reported_status,
                reported_app,
            });
        }

        Ok(Self {
            terminals,
            hidden_finished: result
                .get("hidden_finished")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            machine_count: result
                .get("machines")
                .and_then(Value::as_array)
                .map(Vec::len)
                .unwrap_or(0),
        })
    }
}

fn optional_str(row: &Value, key: &str) -> Option<String> {
    row.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn required_str<'a>(row: &'a Value, key: &str) -> Result<&'a str, HostError> {
    row.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| HostError::Protocol(format!("terminal missing required field: {key}")))
}

/// Versioned state of the local ukk Host. The epoch changes across
/// Host restarts; revision only has meaning within one epoch.
#[derive(Debug, Clone)]
pub struct LiveHostState {
    pub host_epoch: String,
    pub revision: u64,
    pub core_connected: bool,
    pub message: Option<String>,
    pub snapshot: HostSnapshot,
}

impl LiveHostState {
    pub fn from_result(value: &Value) -> Result<Self, HostError> {
        Ok(Self {
            host_epoch: value
                .get("host_epoch")
                .and_then(Value::as_str)
                .ok_or_else(|| HostError::Protocol("missing host_epoch".into()))?
                .to_owned(),
            revision: value
                .get("revision")
                .and_then(Value::as_u64)
                .ok_or_else(|| HostError::Protocol("missing revision".into()))?,
            core_connected: value
                .get("connected")
                .and_then(Value::as_bool)
                .ok_or_else(|| HostError::Protocol("missing connected flag".into()))?,
            message: value.get("message").and_then(Value::as_str).map(str::to_owned),
            snapshot: HostSnapshot::from_result(value)?,
        })
    }
}

#[derive(Clone, Debug)]
pub struct HostClient {
    path: PathBuf,
}

impl HostClient {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    #[cfg(windows)]
    pub fn from_env() -> Self {
        Self::new(windows_pipe::pipe_name().unwrap_or_default())
    }

    #[cfg(unix)]
    pub fn from_env() -> Self {
        let path = env::var_os("UKK_SOCKET")
            .map(PathBuf::from)
            .or_else(|| env::var_os("HOME").map(|home| {
                PathBuf::from(home).join(".ukk").join("run").join("agent-host.sock")
            }))
            .unwrap_or_else(|| PathBuf::from("/nonexistent/ukk/agent-host.sock"));
        Self::new(path)
    }

    pub fn socket_path(&self) -> &std::path::Path {
        &self.path
    }

    pub fn ping(&self) -> Result<bool, HostError> {
        let value = self.request("host.ping")?;
        Ok(value.get("status").and_then(Value::as_str) == Some("ok"))
    }

    pub fn list_terminals(&self) -> Result<HostSnapshot, HostError> {
        HostSnapshot::from_result(&self.request("terminal.list")?)
    }

    pub fn host_snapshot(&self) -> Result<LiveHostState, HostError> {
        LiveHostState::from_result(&self.request("host.snapshot")?)
    }

    /// Long-poll the Host broker. A return with the same epoch/revision means
    /// no change during the bounded wait. Network/Host failures return Err.
    pub fn wait_for_change(
        &self,
        after_epoch: &str,
        after_revision: u64,
    ) -> Result<LiveHostState, HostError> {
        let value = self.request_with(
            "host.wait",
            json!({
                "after_epoch": after_epoch,
                "after_revision": after_revision,
                "timeout_ms": 10_000
            }),
            Duration::from_secs(13),
        )?;
        LiveHostState::from_result(&value)
    }

    fn request(&self, method: &str) -> Result<Value, HostError> {
        self.request_with(method, json!({}), TIMEOUT)
    }

    #[cfg(unix)]
    fn request_with(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, HostError> {
        let mut stream = UnixStream::connect(&self.path)?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(TIMEOUT))?;
        serde_json::to_writer(
            &mut stream,
            &json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}),
        )?;
        stream.write_all(b"\n")?;
        stream.flush()?;

        let mut reader = BufReader::new(stream);
        let mut bytes = Vec::with_capacity(2048);
        loop {
            let mut byte = [0u8; 1];
            if reader.read(&mut byte)? == 0 {
                return Err(HostError::Protocol("Host closed before full response".into()));
            }
            bytes.push(byte[0]);
            if bytes.len() > MAX_REPLY_BYTES {
                return Err(HostError::Protocol("Host reply exceeded size cap".into()));
            }
            if byte[0] == b'\n' {
                break;
            }
        }
        let response: Value = serde_json::from_slice(&bytes)?;
        if response.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
            || response.get("id") != Some(&json!(1))
        {
            return Err(HostError::Protocol("invalid Host JSON-RPC reply".into()));
        }
        if let Some(error) = response.get("error") {
            return Err(HostError::Remote(error.clone()));
        }
        response.get("result").cloned().ok_or_else(|| {
            HostError::Protocol("Host reply missing result".into())
        })
    }

    #[cfg(windows)]
    fn request_with(&self, method: &str, params: Value, _timeout: Duration) -> Result<Value, HostError> {
        // Named Pipe endpoint has an explicit same-user ACL; no TCP fallback.
        let mut stream = windows_pipe::open_client()?;
        serde_json::to_writer(
            &mut stream,
            &json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}),
        )?;
        stream.write_all(b"\n")?;
        stream.flush()?;
        let mut reader = BufReader::new(stream);
        let mut bytes = Vec::with_capacity(2048);
        loop {
            let mut byte = [0u8; 1];
            if reader.read(&mut byte)? == 0 {
                return Err(HostError::Protocol("Host pipe closed before full response".into()));
            }
            bytes.push(byte[0]);
            if bytes.len() > MAX_REPLY_BYTES {
                return Err(HostError::Protocol("Host reply exceeded size cap".into()));
            }
            if byte[0] == b'\n' {
                break;
            }
        }
        let reply: Value = serde_json::from_slice(&bytes)?;
        if reply.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
            || reply.get("id") != Some(&json!(1))
        {
            return Err(HostError::Protocol("invalid Host JSON-RPC reply".into()));
        }
        if let Some(error) = reply.get("error") {
            return Err(HostError::Remote(error.clone()));
        }
        reply.get("result").cloned().ok_or_else(|| {
            HostError::Protocol("Host reply missing result".into())
        })
    }

    #[cfg(not(any(unix, windows)))]
    fn request_with(&self, _method: &str, _params: Value, _timeout: Duration) -> Result<Value, HostError> {
        Err(HostError::UnsupportedPlatform)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_result() -> Value {
        json!({
            "terminals": [
                {
                    "address": "t3",
                    "name": "codex",
                    "title": "Codex agent",
                    "cwd": "/work/project",
                    "argv": ["codex"],
                    "state": "live",
                    "parent": null,
                    "status": {"app":"codex","state":"working"}
                },
                {
                    "address": "t4",
                    "name": null,
                    "title": "Claude Code",
                    "cwd": "/work/project",
                    "argv": ["claude"],
                    "state": "live",
                    "parent": "t3",
                    "status": null
                }
            ],
            "hidden_finished": 2,
            "machines": []
        })
    }

    #[test]
    fn parses_real_hc_schema_fields() {
        let parsed = HostSnapshot::from_result(&valid_result()).unwrap();
        assert_eq!(parsed.terminals[0].address, "t3");
        assert_eq!(parsed.terminals[0].state_label(), "Working");
        assert_eq!(parsed.terminals[1].state_label(), "Live · status unknown");
        assert_eq!(parsed.terminals[1].parent.as_deref(), Some("t3"));
        assert_eq!(parsed.hidden_finished, 2);
    }

    #[test]
    fn never_invents_terminal_status() {
        let mut result = valid_result();
        result["terminals"][0]["status"] = Value::Null;
        assert_eq!(
            HostSnapshot::from_result(&result).unwrap().terminals[0].state_label(),
            "Live · status unknown"
        );
    }

    #[test]
    fn rejects_invalid_snapshot_schema() {
        let mut result = valid_result();
        result["terminals"][0].as_object_mut().unwrap().remove("address");
        assert!(HostSnapshot::from_result(&result).is_err());
    }

    #[test]
    fn groups_actual_children_and_keeps_orphans_visible() {
        let mut result = valid_result();
        // Reverse order: the child is listed before the parent by hc.
        result["terminals"].as_array_mut().unwrap().reverse();
        let parsed = HostSnapshot::from_result(&result).unwrap();
        let hierarchy = parsed.hierarchy();
        assert_eq!(hierarchy[0].0.address, "t3");
        assert_eq!(hierarchy[0].1, 0);
        assert_eq!(hierarchy[1].0.address, "t4");
        assert_eq!(hierarchy[1].1, 1);
    }

    #[test]
    fn cycle_does_not_drop_any_terminal() {
        let mut result = valid_result();
        result["terminals"][0]["parent"] = json!("t4");
        let parsed = HostSnapshot::from_result(&result).unwrap();
        let hierarchy = parsed.hierarchy();
        assert_eq!(hierarchy.len(), 2);
    }

    #[test]
    fn parses_host_broker_state_with_core_health() {
        let mut payload = valid_result();
        payload["host_epoch"] = json!("test-host-1");
        payload["revision"] = json!(44);
        payload["connected"] = json!(false);
        payload["message"] = json!("Connection lost");
        let live = LiveHostState::from_result(&payload).unwrap();
        assert_eq!(live.host_epoch, "test-host-1");
        assert_eq!(live.revision, 44);
        assert!(!live.core_connected);
        assert_eq!(live.snapshot.terminals.len(), 2);
    }

    #[test]
    fn rejects_broker_snapshot_without_revision() {
        let mut payload = valid_result();
        payload["host_epoch"] = json!("test-host-1");
        payload["connected"] = json!(true);
        assert!(LiveHostState::from_result(&payload).is_err());
    }

    #[test]
    fn prefers_explicit_name_to_terminal_title() {
        let first = &HostSnapshot::from_result(&valid_result()).unwrap().terminals[0];
        assert_eq!(first.display_name(), "codex");
    }
}
