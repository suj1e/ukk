//! Safe, read-only local IPC adapter for ukk Agent Host.
//!
//! This bootstrap IPC is local only: Unix sockets on macOS/Linux, a private
//! per-user Named Pipe on Windows. It is not the public remote protocol.
//! A connection is opened per request with bounded request and reply sizes.
//! Unix sockets set read/write deadlines; Windows Named Pipes use overlapped
//! I/O with an explicit deadline and cancellation, so neither transport can
//! block a UI thread indefinitely. Both transports verify the same user.

use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::env;
use std::error::Error;
use std::fmt;
use std::fs;
#[cfg(any(unix, windows))]
use std::io::{Read, Write};
use ukk_domain::DomainSnapshot;
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
            Self::UnsupportedPlatform => write!(f, "local IPC is not supported on this platform"),
        }
    }
}

impl Error for HostError {}

impl From<std::io::Error> for HostError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for HostError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

/// Read one newline-terminated reply with a hard byte cap. A peer that never
/// terminates the line is rejected instead of growing memory or blocking past
/// the transport deadline.
#[cfg(any(unix, windows))]
fn read_bounded_line<R: Read>(reader: &mut R, max: usize) -> Result<Vec<u8>, HostError> {
    let mut bytes = Vec::with_capacity(2048);
    let mut chunk = [0u8; 8192];
    loop {
        let read = reader.read(&mut chunk)?;
        if read == 0 {
            return Err(HostError::Protocol(
                "Host closed before full response".into(),
            ));
        }
        if let Some(position) = chunk[..read].iter().position(|&byte| byte == b'\n') {
            if bytes.len() + position > max {
                return Err(HostError::Protocol("Host reply exceeded size cap".into()));
            }
            bytes.extend_from_slice(&chunk[..position]);
            return Ok(bytes);
        }
        if bytes.len() + read > max {
            return Err(HostError::Protocol("Host reply exceeded size cap".into()));
        }
        bytes.extend_from_slice(&chunk[..read]);
    }
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
            .filter(|(_, terminal)| {
                terminal
                    .parent
                    .as_deref()
                    .is_none_or(|p| !known.contains(p))
            })
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
            .ok_or_else(|| {
                HostError::Protocol("terminal.list result missing terminals array".into())
            })?;

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
                .ok_or_else(|| {
                    HostError::Protocol(format!("terminal {address} missing argv array"))
                })?
                .iter()
                .map(|entry| entry.as_str().map(str::to_owned))
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| {
                    HostError::Protocol(format!("terminal {address} has invalid argv"))
                })?;

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
            message: value
                .get("message")
                .and_then(Value::as_str)
                .map(str::to_owned),
            snapshot: HostSnapshot::from_result(value)?,
        })
    }
}

/// Durable Workspace metadata update. SQLite database identity changes
/// when a new database is created, while revision increases on any write.
#[derive(Debug, Clone)]
pub struct WorkspaceUpdate {
    pub db_epoch: String,
    pub revision: i64,
    /// None means the bounded wait timed out without a change.
    pub snapshot: Option<DomainSnapshot>,
}

impl WorkspaceUpdate {
    pub fn from_result(result: &Value) -> Result<Self, HostError> {
        let db_epoch = result
            .get("db_epoch")
            .and_then(Value::as_str)
            .ok_or_else(|| HostError::Protocol("workspace result missing db_epoch".into()))?
            .to_owned();
        let revision = result
            .get("revision")
            .and_then(Value::as_i64)
            .ok_or_else(|| HostError::Protocol("workspace result missing revision".into()))?;
        let snapshot = match result.get("snapshot") {
            Some(value) if !value.is_null() => Some(serde_json::from_value(value.clone())?),
            _ => None,
        };
        Ok(Self {
            db_epoch,
            revision,
            snapshot,
        })
    }
}

/// One read-only UI descriptor contributed by an enabled plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginView {
    pub plugin_id: String,
    pub view_id: String,
    pub slot: String,
    pub title: String,
    pub command: Option<String>,
    pub overlay: Option<PluginOverlay>,
}

/// Declared overlay bounds. A renderer must still clamp this to its Session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginOverlay {
    pub anchor: String,
    pub max_width: u32,
    pub max_height: u32,
}

/// One declared command, for menus and capability-lock rendering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginCommand {
    pub plugin_id: String,
    pub command_id: String,
    pub title: String,
    pub scope: String,
    pub requires: Vec<String>,
}

/// Life-cycle status of one installed plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginStatusView {
    pub plugin_id: String,
    pub trust: String,
    /// `disabled`, `starting`, `ready` or `failed`.
    pub state: String,
    pub installed: bool,
    pub enabled_by_registry: bool,
    pub command_count: usize,
    pub view_count: usize,
    pub queued_events: usize,
    pub dropped_events: u64,
    pub failure: Option<String>,
}

/// Read-only projection of the Host plugin service. Contains no helper output.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PluginSnapshot {
    pub capability: String,
    pub control_enabled: bool,
    pub control_problem: Option<String>,
    pub registry_path: String,
    pub registry_loaded: bool,
    pub registry_present: bool,
    pub registry_error: Option<String>,
    pub plugins: Vec<PluginStatusView>,
    pub session_actions: Vec<PluginView>,
    pub session_tabs: Vec<PluginView>,
    pub session_overlays: Vec<PluginView>,
    pub commands: Vec<PluginCommand>,
}

impl PluginSnapshot {
    pub fn from_result(value: &Value) -> Result<Self, HostError> {
        let registry = value
            .get("registry")
            .ok_or_else(|| HostError::Protocol("plugin snapshot missing registry".into()))?;
        let contributions = value
            .get("contributions")
            .ok_or_else(|| HostError::Protocol("plugin snapshot missing contributions".into()))?;
        Ok(Self {
            capability: optional_str(value, "capability").unwrap_or_default(),
            control_enabled: value
                .get("control_enabled")
                .and_then(Value::as_bool)
                .ok_or_else(|| {
                    HostError::Protocol("plugin snapshot missing control_enabled".into())
                })?,
            control_problem: optional_str(value, "control_problem"),
            registry_path: optional_str(registry, "path").unwrap_or_default(),
            registry_loaded: registry
                .get("loaded")
                .and_then(Value::as_bool)
                .ok_or_else(|| {
                    HostError::Protocol("plugin snapshot missing registry.loaded".into())
                })?,
            registry_present: registry
                .get("present")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            registry_error: optional_str(registry, "error"),
            plugins: array(required_field(value, "plugins")?)
                .iter()
                .map(parse_plugin_status)
                .collect::<Result<_, _>>()?,
            session_actions: parse_views(contributions, "session_actions")?,
            session_tabs: parse_views(contributions, "session_tabs")?,
            session_overlays: parse_views(contributions, "session_overlays")?,
            commands: parse_commands(contributions)?,
        })
    }
}

fn required_field<'a>(value: &'a Value, key: &str) -> Result<&'a Value, HostError> {
    value
        .get(key)
        .ok_or_else(|| HostError::Protocol(format!("plugin snapshot missing {key}")))
}

fn array(value: &Value) -> &[Value] {
    value.as_array().map(Vec::as_slice).unwrap_or(&[])
}

fn parse_plugin_status(row: &Value) -> Result<PluginStatusView, HostError> {
    Ok(PluginStatusView {
        plugin_id: required_str(row, "plugin_id")?.to_owned(),
        trust: required_str(row, "trust")?.to_owned(),
        state: required_str(row, "state")?.to_owned(),
        installed: row
            .get("installed")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        enabled_by_registry: row
            .get("enabled_by_registry")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        command_count: row
            .get("command_count")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize,
        view_count: row.get("view_count").and_then(Value::as_u64).unwrap_or(0) as usize,
        queued_events: row
            .get("queued_events")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize,
        dropped_events: row
            .get("dropped_events")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        failure: optional_str(row, "failure"),
    })
}

fn parse_views(section: &Value, key: &str) -> Result<Vec<PluginView>, HostError> {
    array(required_field(section, key)?)
        .iter()
        .map(|row| {
            let overlay = match row.get("overlay") {
                Some(Value::Null) | None => None,
                Some(overlay) => Some(PluginOverlay {
                    anchor: optional_str(overlay, "anchor").unwrap_or_default(),
                    max_width: overlay
                        .get("max_width")
                        .and_then(Value::as_u64)
                        .unwrap_or(0) as u32,
                    max_height: overlay
                        .get("max_height")
                        .and_then(Value::as_u64)
                        .unwrap_or(0) as u32,
                }),
            };
            Ok(PluginView {
                plugin_id: required_str(row, "plugin_id")?.to_owned(),
                view_id: required_str(row, "view_id")?.to_owned(),
                slot: required_str(row, "slot")?.to_owned(),
                title: optional_str(row, "title").unwrap_or_default(),
                command: optional_str(row, "command"),
                overlay,
            })
        })
        .collect()
}

fn parse_commands(section: &Value) -> Result<Vec<PluginCommand>, HostError> {
    array(required_field(section, "commands")?)
        .iter()
        .map(|row| {
            Ok(PluginCommand {
                plugin_id: required_str(row, "plugin_id")?.to_owned(),
                command_id: required_str(row, "command_id")?.to_owned(),
                title: optional_str(row, "title").unwrap_or_default(),
                scope: optional_str(row, "scope").unwrap_or_default(),
                requires: row
                    .get("requires")
                    .and_then(Value::as_array)
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default(),
            })
        })
        .collect()
}

/// Successful plugin command result. `value` is helper-authored and must be
/// rendered as untrusted text.
#[derive(Debug, Clone, PartialEq)]
pub struct PluginInvokeOutcome {
    pub plugin_id: String,
    pub command: String,
    pub provenance: String,
    pub value: Value,
}

impl PluginInvokeOutcome {
    pub fn from_result(value: &Value) -> Result<Self, HostError> {
        Ok(Self {
            plugin_id: required_str(value, "plugin_id")?.to_owned(),
            command: required_str(value, "command")?.to_owned(),
            provenance: required_str(value, "provenance")?.to_owned(),
            value: value.get("value").cloned().unwrap_or(Value::Null),
        })
    }
}

#[derive(Clone, Debug)]
pub struct HostClient {
    path: PathBuf,
    /// Explicit plugin-control token path for tests or embedded callers. When
    /// absent the platform default (or its env override) is used.
    plugin_control_path: Option<PathBuf>,
}

impl HostClient {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            plugin_control_path: None,
        }
    }

    /// Override where this client reads the separate `plugin.control` token.
    /// The token itself is still read only when an invoke is attempted.
    pub fn with_plugin_control_token_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.plugin_control_path = Some(path.into());
        self
    }

    #[cfg(windows)]
    pub fn from_env() -> Self {
        Self::new(windows_pipe::pipe_name().unwrap_or_default())
    }

    #[cfg(unix)]
    pub fn from_env() -> Self {
        let path = env::var_os("UKK_SOCKET")
            .map(PathBuf::from)
            .or_else(|| {
                env::var_os("HOME").map(|home| {
                    PathBuf::from(home)
                        .join(".ukk")
                        .join("run")
                        .join("agent-host.sock")
                })
            })
            .unwrap_or_else(|| PathBuf::from("/nonexistent/ukk/agent-host.sock"));
        Self::new(path)
    }

    pub fn socket_path(&self) -> &std::path::Path {
        &self.path
    }

    /// Same-user capability for **local workspace metadata mutations only**.
    /// The token file is outside the Relay protocol and never sent to hc.
    #[cfg(unix)]
    pub fn mutation_token_path(&self) -> PathBuf {
        self.path.with_extension("token")
    }

    #[cfg(windows)]
    pub fn mutation_token_path(&self) -> PathBuf {
        env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("C:\\nonexistent\\ukk"))
            .join("ukk")
            .join("run")
            .join("agent-host.token")
    }

    pub fn mutation_token(&self) -> Result<String, HostError> {
        let token = fs::read_to_string(self.mutation_token_path())?;
        let token = token.trim();
        if token.len() < 60 || !token.is_ascii() {
            return Err(HostError::Protocol(
                "invalid local Host mutation credential".into(),
            ));
        }
        Ok(token.to_owned())
    }

    /// `plugin.control` token path. This is separate from both the workspace
    /// metadata token and the agent-control token.
    pub fn plugin_control_token_path(&self) -> PathBuf {
        if let Some(path) = &self.plugin_control_path {
            return path.clone();
        }
        if let Some(path) = env::var_os("UKK_PLUGIN_CONTROL_TOKEN_FILE") {
            return PathBuf::from(path);
        }
        #[cfg(unix)]
        {
            self.path.with_extension("plugin-control")
        }
        #[cfg(windows)]
        {
            env::var_os("LOCALAPPDATA")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("C:\\nonexistent\\ukk"))
                .join("ukk")
                .join("run")
                .join("agent-host.plugin-control")
        }
        #[cfg(not(any(unix, windows)))]
        {
            PathBuf::from("ukk-plugin-control-token-unsupported-platform")
        }
    }

    /// Read the plugin-control token at call time. The value is never printed,
    /// stored in a snapshot or included in an error.
    fn plugin_control_token(&self) -> Result<String, HostError> {
        let path = self.plugin_control_token_path();
        let metadata = fs::symlink_metadata(&path).map_err(|error| {
            HostError::Protocol(format!(
                "plugin control capability is unavailable at {}: {error}",
                path.display()
            ))
        })?;
        if metadata.file_type().is_symlink() {
            return Err(HostError::Protocol(
                "plugin control capability must not be a symlink".into(),
            ));
        }
        if !metadata.file_type().is_file() {
            return Err(HostError::Protocol(
                "plugin control capability must be a regular file".into(),
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o077 != 0 {
                return Err(HostError::Protocol(
                    "plugin control capability permissions must be 0600".into(),
                ));
            }
        }
        let token = fs::read_to_string(&path).map_err(|error| {
            HostError::Protocol(format!(
                "cannot read plugin control capability at {}: {error}",
                path.display()
            ))
        })?;
        let token = token.trim();
        if token.len() < 60 || !token.is_ascii() {
            return Err(HostError::Protocol(
                "plugin control capability is too short or not ASCII".into(),
            ));
        }
        Ok(token.to_owned())
    }

    /// Read-only plugin status and contributions. Never requires a capability.
    pub fn plugin_snapshot_json(&self) -> Result<Value, HostError> {
        self.request("plugin.snapshot")
    }

    pub fn plugin_snapshot(&self) -> Result<PluginSnapshot, HostError> {
        PluginSnapshot::from_result(&self.plugin_snapshot_json()?)
    }

    /// Invoke one declared plugin command. The control token is read only here,
    /// at the moment of an explicit user action, and never included in errors.
    pub fn invoke_plugin(
        &self,
        plugin_id: &str,
        command: &str,
        session_id: Option<&str>,
        args: Value,
    ) -> Result<PluginInvokeOutcome, HostError> {
        let token = self.plugin_control_token()?;
        let mut params = json!({
            "plugin_id": plugin_id,
            "command": command,
            "args": args,
            "control": token,
        });
        if let Some(session_id) = session_id {
            params["session_id"] = json!(session_id);
        }
        let value = self.request_with("plugin.invoke", params, Duration::from_secs(10))?;
        PluginInvokeOutcome::from_result(&value)
    }

    /// Local desktop actions never touch PTY lifecycle or Agent approvals.
    /// A mutation needs the Host's user-only capability plus an explicit
    /// action payload (and a revision for all pane tree changes).
    pub fn local_workspace_mutation(
        &self,
        method: &str,
        mut params: Value,
    ) -> Result<Value, HostError> {
        if !matches!(
            method,
            "workspace.session.create"
                | "workspace.pane.split"
                | "workspace.pane.close"
                | "workspace.pane.resize"
                | "workspace.tab.add"
                | "workspace.tab.close"
                | "workspace.tab.reorder"
                | "workspace.tab.move"
        ) {
            return Err(HostError::Protocol(
                "method is not an allowed local workspace mutation".into(),
            ));
        }
        let object = params.as_object_mut().ok_or_else(|| {
            HostError::Protocol("workspace mutation params must be an object".into())
        })?;
        object.insert("auth".into(), Value::String(self.mutation_token()?));
        self.request_with(method, params, Duration::from_secs(5))
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

    /// Durable user-created Spaces / Sessions / AgentRun bindings / pane trees.
    /// No herdr-core process state or chat content is fabricated here.
    pub fn workspace_snapshot(&self) -> Result<DomainSnapshot, HostError> {
        let value = self.request("workspace.snapshot")?;
        Ok(serde_json::from_value(value)?)
    }

    pub fn workspace_state(&self) -> Result<WorkspaceUpdate, HostError> {
        WorkspaceUpdate::from_result(&self.request("workspace.state")?)
    }

    /// Wait for SQLite Workspace metadata changes caused by any process.
    /// Unlike Agent event sequences, revisions persist across Host restarts.
    pub fn wait_for_workspace_change(
        &self,
        after_epoch: &str,
        after_revision: i64,
    ) -> Result<WorkspaceUpdate, HostError> {
        let value = self.request_with(
            "workspace.wait",
            json!({
                "after_epoch": after_epoch,
                "after_revision": after_revision,
                "timeout_ms": 10_000
            }),
            Duration::from_secs(13),
        )?;
        WorkspaceUpdate::from_result(&value)
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
    fn request_with(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, HostError> {
        let mut stream = UnixStream::connect(&self.path)?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(TIMEOUT))?;
        serde_json::to_writer(
            &mut stream,
            &json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}),
        )?;
        stream.write_all(b"\n")?;
        stream.flush()?;
        let bytes = read_bounded_line(&mut stream, MAX_REPLY_BYTES)?;
        let response: Value = serde_json::from_slice(&bytes)?;
        if response.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
            || response.get("id") != Some(&json!(1))
        {
            return Err(HostError::Protocol("invalid Host JSON-RPC reply".into()));
        }
        if let Some(error) = response.get("error") {
            return Err(HostError::Remote(error.clone()));
        }
        response
            .get("result")
            .cloned()
            .ok_or_else(|| HostError::Protocol("Host reply missing result".into()))
    }

    #[cfg(windows)]
    fn request_with(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, HostError> {
        // Named Pipe endpoint has an explicit same-user ACL and SID checks;
        // no TCP fallback. Overlapped I/O enforces the deadline below.
        let mut stream = windows_pipe::open_client()?;
        stream.set_read_timeout(timeout);
        serde_json::to_writer(
            &mut stream,
            &json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}),
        )?;
        stream.write_all(b"\n")?;
        stream.flush()?;
        let bytes = read_bounded_line(&mut stream, MAX_REPLY_BYTES)?;
        let reply: Value = serde_json::from_slice(&bytes)?;
        if reply.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
            || reply.get("id") != Some(&json!(1))
        {
            return Err(HostError::Protocol("invalid Host JSON-RPC reply".into()));
        }
        if let Some(error) = reply.get("error") {
            return Err(HostError::Remote(error.clone()));
        }
        reply
            .get("result")
            .cloned()
            .ok_or_else(|| HostError::Protocol("Host reply missing result".into()))
    }

    #[cfg(not(any(unix, windows)))]
    fn request_with(
        &self,
        _method: &str,
        _params: Value,
        _timeout: Duration,
    ) -> Result<Value, HostError> {
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
        result["terminals"][0]
            .as_object_mut()
            .unwrap()
            .remove("address");
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
    fn local_mutation_allowlist_blocks_agent_and_terminal_commands() {
        let client = HostClient::new("/nonexistent/ukk-test.sock");
        assert!(matches!(
            client.local_workspace_mutation("terminal.kill", json!({"terminal":"t1"})),
            Err(HostError::Protocol(_))
        ));
        assert!(matches!(
            client.local_workspace_mutation("agent.approval.respond", json!({})),
            Err(HostError::Protocol(_))
        ));
    }

    #[test]
    fn workspace_update_requires_cursor_and_supports_no_change() {
        let initial = serde_json::json!({
            "db_epoch": "local-db-a", "revision": 2,
            "snapshot": {"spaces":[],"sessions":[],"agent_runs":[],"layouts":[]}
        });
        let parsed = WorkspaceUpdate::from_result(&initial).unwrap();
        assert_eq!(parsed.revision, 2);
        assert_eq!(parsed.db_epoch, "local-db-a");
        assert_eq!(parsed.snapshot.unwrap().spaces.len(), 0);
        let idle = WorkspaceUpdate::from_result(&serde_json::json!({
            "db_epoch":"local-db-a","revision":2,"snapshot":null
        }))
        .unwrap();
        assert!(idle.snapshot.is_none());
        assert!(WorkspaceUpdate::from_result(&serde_json::json!({
            "revision":1,"snapshot":null
        }))
        .is_err());
    }

    #[test]
    fn parses_plugin_snapshot_and_contributions() {
        let payload = json!({
            "capability": "separate-plugin-control",
            "control_enabled": true,
            "control_problem": null,
            "registry": {
                "path": "/home/user/.ukk/plugins/registry.json",
                "loaded": true,
                "present": true,
                "error": null
            },
            "plugins": [{
                "plugin_id": "dev.ukk.sample.session-action",
                "trust": "first-party",
                "state": "ready",
                "installed": true,
                "enabled_by_registry": true,
                "command_count": 1,
                "view_count": 3,
                "queued_events": 0,
                "dropped_events": 0,
                "failure": null
            }],
            "contributions": {
                "session_actions": [{
                    "plugin_id": "dev.ukk.sample.session-action",
                    "view_id": "sample.session.action",
                    "slot": "session.action",
                    "title": "Sample action",
                    "command": "sample.session.ping",
                    "overlay": null
                }],
                "session_tabs": [{
                    "plugin_id": "dev.ukk.sample.session-action",
                    "view_id": "sample.session.tab",
                    "slot": "session.tab",
                    "title": "Sample",
                    "command": "sample.session.ping",
                    "overlay": null
                }],
                "session_overlays": [{
                    "plugin_id": "dev.ukk.sample.session-action",
                    "view_id": "sample.session.overlay",
                    "slot": "session.overlay",
                    "title": "Sample overlay",
                    "command": "sample.session.ping",
                    "overlay": {"anchor": "top-right", "max_width": 320, "max_height": 240}
                }],
                "commands": [{
                    "plugin_id": "dev.ukk.sample.session-action",
                    "command_id": "sample.session.ping",
                    "title": "Sample: ping this session",
                    "scope": "session",
                    "requires": []
                }]
            }
        });
        let snapshot = PluginSnapshot::from_result(&payload).unwrap();
        assert!(snapshot.control_enabled);
        assert!(snapshot.registry_loaded);
        assert_eq!(snapshot.plugins.len(), 1);
        assert_eq!(snapshot.plugins[0].state, "ready");
        assert_eq!(snapshot.session_actions.len(), 1);
        assert_eq!(snapshot.session_tabs.len(), 1);
        assert_eq!(snapshot.session_overlays.len(), 1);
        assert_eq!(
            snapshot.session_overlays[0]
                .overlay
                .as_ref()
                .unwrap()
                .anchor,
            "top-right"
        );
        assert_eq!(snapshot.commands.len(), 1);
    }

    #[test]
    fn rejects_plugin_snapshot_without_required_fields() {
        assert!(PluginSnapshot::from_result(&json!({})).is_err());
        assert!(PluginSnapshot::from_result(&json!({
            "control_enabled": true,
            "registry": {"loaded": true},
            "plugins": []
        }))
        .is_err());
    }

    #[test]
    fn plugin_control_token_reader_never_accepts_missing_or_insecure_files() {
        let client = HostClient::new("/nonexistent/ukk-test.sock")
            .with_plugin_control_token_path("/nonexistent/ukk-plugin-control");
        let error = client.plugin_control_token().unwrap_err();
        assert!(error.to_string().contains("unavailable"));
    }

    #[test]
    fn prefers_explicit_name_to_terminal_title() {
        let first = &HostSnapshot::from_result(&valid_result())
            .unwrap()
            .terminals[0];
        assert_eq!(first.display_name(), "codex");
    }
}
