//! Typed herdr-core protocol types pinned to protocol generation 1.
//!
//! Field names and optionality follow `hc.openrpc.json` at herdr-core
//! v0.1.18 (`3965a46caae095a3ead01de237fa13955e52b8c6`). Unknown fields are
//! ignored so a newer core that only grows the API still parses; unsupported
//! *methods* are refused explicitly instead of guessed.

use crate::error::{HcError, Result};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
#[cfg(test)]
use serde_json::json;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::str::FromStr;

/// Protocol generation this crate implements.
pub const PROTOCOL_GENERATION: u32 = 1;
/// Maximum payload in one framed message, from `schema/README.md`.
pub const MAX_FRAME_PAYLOAD: usize = 16 * 1024 * 1024;
/// A stream starts with this much credit before the viewer sends CREDIT.
pub const STREAM_INITIAL_CREDIT: usize = 256 * 1024;
/// A viewer this far behind is repainted by the core instead of receiving a
/// backlog. Our local queues use the same order of magnitude as a bound.
pub const STREAM_REPAINT_THRESHOLD: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoreInfo {
    pub id: String,
    pub name: String,
}

/// The `hello` result: the capability document for one connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hello {
    pub generation: u32,
    pub version: String,
    pub core: CoreInfo,
    pub framed: bool,
    pub methods: BTreeSet<String>,
    pub features: BTreeSet<String>,
}

impl Hello {
    pub fn from_result(value: &Value) -> Result<Self> {
        let generation = value
            .get("generation")
            .and_then(Value::as_u64)
            .ok_or_else(|| HcError::Protocol("hello result missing generation".into()))?;
        if generation != u64::from(PROTOCOL_GENERATION) {
            return Err(HcError::Remote(crate::error::RemoteError {
                code: -32000,
                message: format!("unsupported generation {generation}"),
                kind: crate::error::RemoteKind::UnsupportedGeneration,
                hint: None,
                data: value.clone(),
            }));
        }
        let core: CoreInfo = serde_json::from_value(
            value
                .get("core")
                .cloned()
                .ok_or_else(|| HcError::Protocol("hello result missing core".into()))?,
        )?;
        let methods = string_set(value, "methods")?;
        let features = string_set(value, "features")?;
        Ok(Self {
            generation: generation as u32,
            version: required_string(value, "version")?,
            core,
            framed: value
                .get("framed")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            methods,
            features,
        })
    }

    pub fn supports(&self, method: &str) -> bool {
        self.methods.contains(method)
    }

    pub fn supports_feature(&self, feature: &str) -> bool {
        self.features.contains(feature)
    }

    /// Refuse a method the connected core does not advertise.
    pub fn require_method(&self, method: &str) -> Result<()> {
        if self.supports(method) {
            Ok(())
        } else {
            Err(HcError::Unsupported {
                method: method.to_owned(),
                reason: format!("herdr-core {} does not advertise this method", self.version),
            })
        }
    }

    pub fn require_feature(&self, feature: &str) -> Result<()> {
        if self.supports_feature(feature) {
            Ok(())
        } else {
            Err(HcError::Unsupported {
                method: format!("feature:{feature}"),
                reason: format!(
                    "herdr-core {} does not advertise this feature",
                    self.version
                ),
            })
        }
    }
}

fn string_set(value: &Value, key: &str) -> Result<BTreeSet<String>> {
    let array = value
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| HcError::Protocol(format!("hello result missing {key}")))?;
    array
        .iter()
        .map(|entry| {
            entry
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| HcError::Protocol(format!("hello {key} contains a non-string")))
        })
        .collect()
}

fn required_string(value: &Value, key: &str) -> Result<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| HcError::Protocol(format!("missing string field {key}")))
}

/// A terminal address: `t7`, a terminal name, or `machine/t7`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TerminalAddress(String);

impl TerminalAddress {
    pub fn parse(raw: impl Into<String>) -> Result<Self> {
        let raw = raw.into();
        validate_address(&raw)?;
        Ok(Self(raw))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TerminalAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for TerminalAddress {
    type Err = HcError;

    fn from_str(value: &str) -> Result<Self> {
        Self::parse(value)
    }
}

impl Serialize for TerminalAddress {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for TerminalAddress {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(raw).map_err(serde::de::Error::custom)
    }
}

/// Terminal names accepted by `terminal.run` (`[a-z0-9._-]`, 1–32).
pub fn validate_terminal_name(raw: &str) -> Result<()> {
    if raw.is_empty() || raw.len() > 32 {
        return Err(HcError::InvalidArgument(format!(
            "terminal name must be 1-32 characters, got {}",
            raw.len()
        )));
    }
    if !raw
        .bytes()
        .all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.'))
    {
        return Err(HcError::InvalidArgument(
            "terminal name may only contain a-z, 0-9, '-', '_' and '.'".into(),
        ));
    }
    Ok(())
}

fn validate_address(raw: &str) -> Result<()> {
    let mut parts = raw.split('/');
    let first = parts.next().unwrap_or_default();
    let second = parts.next();
    if parts.next().is_some() || first.is_empty() {
        return Err(HcError::InvalidArgument(format!(
            "invalid terminal address {raw:?}"
        )));
    }
    let check = |part: &str| -> Result<()> {
        if part.is_empty()
            || part.len() > 64
            || !part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        {
            Err(HcError::InvalidArgument(format!(
                "invalid terminal address {raw:?}"
            )))
        } else {
            Ok(())
        }
    };
    check(first)?;
    if let Some(second) = second {
        check(second)?;
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalState {
    Live,
    Finished,
    Other(String),
}

impl TerminalState {
    pub fn is_live(&self) -> bool {
        matches!(self, Self::Live)
    }

    pub fn is_finished(&self) -> bool {
        matches!(self, Self::Finished)
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::Live => "live",
            Self::Finished => "finished",
            Self::Other(other) => other,
        }
    }
}

impl Serialize for TerminalState {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for TerminalState {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Ok(match raw.as_str() {
            "live" => Self::Live,
            "finished" => Self::Finished,
            _ => Self::Other(raw),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalReason {
    Exited,
    Killed,
    ServerStop,
    HostShutdown,
    HolderLost,
    StoppedByRestart,
    Signaled,
    SpawnPending,
    Other(String),
}

impl TerminalReason {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Exited => "exited",
            Self::Killed => "killed",
            Self::ServerStop => "server_stop",
            Self::HostShutdown => "host_shutdown",
            Self::HolderLost => "holder_lost",
            Self::StoppedByRestart => "stopped_by_restart",
            Self::Signaled => "signaled",
            Self::SpawnPending => "spawn_pending",
            Self::Other(other) => other,
        }
    }
}

impl Serialize for TerminalReason {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for TerminalReason {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Ok(match raw.as_str() {
            "exited" => Self::Exited,
            "killed" => Self::Killed,
            "server_stop" => Self::ServerStop,
            "host_shutdown" => Self::HostShutdown,
            "holder_lost" => Self::HolderLost,
            "stopped_by_restart" => Self::StoppedByRestart,
            "signaled" => Self::Signaled,
            "spawn_pending" => Self::SpawnPending,
            _ => Self::Other(raw),
        })
    }
}

/// One OSC 7501 record. `state` is kept as the program's own word: the
/// protocol explicitly forbids translating it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Status {
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub app: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub msg: String,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub progress: Option<u8>,
}

impl Status {
    pub fn is_working(&self) -> bool {
        self.state == "working"
    }

    pub fn is_blocked(&self) -> bool {
        self.state == "blocked"
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Foreground {
    pub pid: i32,
    pub name: String,
    #[serde(default)]
    pub argv: Vec<String>,
    #[serde(default)]
    pub cwd: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct FirstMessage {
    pub sent: bool,
    #[serde(default)]
    pub started: Option<bool>,
    #[serde(default)]
    pub turn: Option<u64>,
    #[serde(default)]
    pub reason: Option<String>,
}

/// A terminal record. Required fields come from the pinned `Terminal` schema;
/// optional fields default so a growing API does not break parsing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Terminal {
    pub address: TerminalAddress,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    pub argv: Vec<String>,
    pub cwd: String,
    pub state: TerminalState,
    pub created_ms: u64,
    pub screen: String,
    pub cols: u16,
    pub rows: u16,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub turn: u64,
    #[serde(default)]
    pub parent: Option<TerminalAddress>,
    #[serde(default)]
    pub pid: Option<u32>,
    #[serde(default)]
    pub keep: bool,
    #[serde(default)]
    pub reason: Option<TerminalReason>,
    #[serde(default)]
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub signal: Option<i32>,
    #[serde(default)]
    pub finished_ms: Option<u64>,
    #[serde(default)]
    pub status: Option<Status>,
    #[serde(default)]
    pub records: BTreeMap<String, Status>,
    #[serde(default)]
    pub reported: bool,
    #[serde(default)]
    pub status_ms: Option<u64>,
    #[serde(default)]
    pub foreground: Option<Foreground>,
    #[serde(default)]
    pub identity: String,
    #[serde(default)]
    pub notify: bool,
    #[serde(default)]
    pub die_with_parent: bool,
    #[serde(default)]
    pub rm: bool,
    #[serde(default)]
    pub paste_gap_ms: Option<u64>,
    #[serde(default)]
    pub first_message: Option<FirstMessage>,
    #[serde(default)]
    pub template: Option<String>,
    #[serde(default)]
    pub stop: Option<String>,
    #[serde(default)]
    pub submit: Option<String>,
}

impl Terminal {
    pub fn from_value(value: Value) -> Result<Self> {
        Ok(serde_json::from_value(value)?)
    }

    /// Product-level identity check: a binding is only reusable for the core
    /// that issued this record's address.
    pub fn is_live(&self) -> bool {
        self.state.is_live()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MachineView {
    pub name: String,
    pub target: String,
    pub id: String,
    pub host: String,
    pub version: String,
    pub state: String,
    #[serde(default)]
    pub session: Option<String>,
    #[serde(default)]
    pub temporary: bool,
    #[serde(default)]
    pub via: bool,
    #[serde(default)]
    pub why: Option<String>,
    #[serde(default)]
    pub since_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ListResult {
    #[serde(default)]
    pub terminals: Vec<Terminal>,
    #[serde(default)]
    pub hidden_finished: u64,
    #[serde(default)]
    pub machines: Vec<MachineView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Display {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bg: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fg: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cell: Option<[u32; 2]>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InitialSize {
    pub cols: u16,
    pub rows: u16,
}

impl InitialSize {
    pub fn new(cols: u16, rows: u16) -> Result<Self> {
        if cols == 0 || rows == 0 {
            return Err(HcError::InvalidArgument(
                "initial terminal size must be at least 1x1".into(),
            ));
        }
        Ok(Self { cols, rows })
    }
}

/// `terminal.run` request. Only the documented fields are serialised.
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct RunRequest {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub argv: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<TerminalAddress>,
    #[serde(default)]
    pub keep: bool,
    #[serde(default)]
    pub outlive_parent: bool,
    #[serde(default)]
    pub no_notify: bool,
    #[serde(default)]
    pub no_parent: bool,
    #[serde(default)]
    pub rm: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller: Option<TerminalAddress>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<InitialSize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<Display>,
    #[serde(default)]
    pub wait: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<f64>,
}

impl RunRequest {
    pub fn validate(&self) -> Result<()> {
        if let Some(name) = &self.name {
            validate_terminal_name(name)?;
        }
        if !self.argv.is_empty() && self.line.is_some() {
            return Err(HcError::InvalidArgument(
                "terminal.run accepts argv or line, not both".into(),
            ));
        }
        if let Some(timeout) = self.timeout {
            if !timeout.is_finite() || timeout <= 0.0 {
                return Err(HcError::InvalidArgument(
                    "terminal.run timeout must be a positive number".into(),
                ));
            }
        }
        if self.no_parent && self.parent.is_some() {
            return Err(HcError::InvalidArgument(
                "terminal.run cannot set both parent and no_parent".into(),
            ));
        }
        Ok(())
    }

    pub fn to_params(&self) -> Result<Value> {
        self.validate()?;
        Ok(serde_json::to_value(self)?)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RunResult {
    pub terminal: Terminal,
    pub first_message: Option<FirstMessage>,
    pub output: Option<String>,
}

impl RunResult {
    pub fn from_value(value: Value) -> Result<Self> {
        let first_message = value
            .get("first_message")
            .filter(|message| !message.is_null())
            .cloned()
            .map(serde_json::from_value)
            .transpose()?;
        let output = value
            .get("output")
            .and_then(Value::as_str)
            .map(str::to_owned);
        Ok(Self {
            terminal: Terminal::from_value(value)?,
            first_message,
            output,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AttachRequest {
    pub terminal: TerminalAddress,
    pub cols: u16,
    pub rows: u16,
    #[serde(default)]
    pub history: u64,
    #[serde(default)]
    pub observe: bool,
    #[serde(default)]
    pub resize_notifications: bool,
    #[serde(default)]
    pub resumed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller: Option<TerminalAddress>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<Display>,
}

impl AttachRequest {
    pub fn observe(terminal: TerminalAddress, cols: u16, rows: u16) -> Self {
        Self {
            terminal,
            cols,
            rows,
            history: 0,
            observe: true,
            resize_notifications: false,
            resumed: false,
            caller: None,
            display: None,
        }
    }

    pub fn to_params(&self) -> Result<Value> {
        Ok(serde_json::to_value(self)?)
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct AttachResult {
    pub channel: u32,
    pub terminal: Terminal,
    #[serde(default)]
    pub resize_notifications: bool,
    #[serde(default)]
    pub automatic_input: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdleOrWorking {
    Idle,
    Working,
}

impl Serialize for IdleOrWorking {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(match self {
            Self::Idle => "idle",
            Self::Working => "working",
        })
    }
}

impl<'de> Deserialize<'de> for IdleOrWorking {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        match String::deserialize(deserializer)?.as_str() {
            "idle" => Ok(Self::Idle),
            "working" => Ok(Self::Working),
            other => Err(serde::de::Error::custom(format!(
                "unknown idle/working value {other:?}"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SendRequest {
    pub terminal: TerminalAddress,
    pub text: String,
    #[serde(default)]
    pub force: bool,
    #[serde(rename = "if", default, skip_serializing_if = "Option::is_none")]
    pub if_status: Option<IdleOrWorking>,
    #[serde(default)]
    pub wait: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller: Option<TerminalAddress>,
}

impl SendRequest {
    pub fn new(terminal: TerminalAddress, text: impl Into<String>) -> Self {
        Self {
            terminal,
            text: text.into(),
            force: false,
            if_status: None,
            wait: false,
            timeout: None,
            caller: None,
        }
    }

    pub fn to_params(&self) -> Result<Value> {
        if self.text.is_empty() {
            return Err(HcError::InvalidArgument(
                "terminal.send text must not be empty".into(),
            ));
        }
        if let Some(timeout) = self.timeout {
            if !timeout.is_finite() || timeout <= 0.0 {
                return Err(HcError::InvalidArgument(
                    "terminal.send timeout must be a positive number".into(),
                ));
            }
        }
        Ok(serde_json::to_value(self)?)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct SendResult {
    pub turn: u64,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub started: Option<bool>,
    #[serde(default)]
    pub wait: Option<WaitResult>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct KeysRequest {
    pub terminal: TerminalAddress,
    pub keys: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller: Option<TerminalAddress>,
}

impl KeysRequest {
    pub fn to_params(&self) -> Result<Value> {
        if self.keys.is_empty() {
            return Err(HcError::InvalidArgument(
                "terminal.keys requires at least one key".into(),
            ));
        }
        Ok(serde_json::to_value(self)?)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct KeysResult {
    pub turn: u64,
    #[serde(default)]
    pub status: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct InterruptRequest {
    pub terminal: TerminalAddress,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<f64>,
}

impl InterruptRequest {
    pub fn to_params(&self) -> Result<Value> {
        Ok(serde_json::to_value(self)?)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InterruptOutcome {
    Stopped,
    Exited,
    StillWorking,
    Unknown,
    Other(String),
}

impl InterruptOutcome {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Stopped => "stopped",
            Self::Exited => "exited",
            Self::StillWorking => "still_working",
            Self::Unknown => "unknown",
            Self::Other(other) => other,
        }
    }
}

impl Serialize for InterruptOutcome {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for InterruptOutcome {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Ok(match raw.as_str() {
            "stopped" => Self::Stopped,
            "exited" => Self::Exited,
            "still_working" => Self::StillWorking,
            "unknown" => Self::Unknown,
            _ => Self::Other(raw),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InterruptResult {
    pub outcome: InterruptOutcome,
    pub key: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RemoveResult {
    pub removed: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StopResult {
    pub stopped: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct WaitResult {
    #[serde(default)]
    pub terminal: String,
    pub ended: bool,
    pub reason: String,
    pub turn: u64,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub signal: Option<i32>,
    #[serde(default)]
    pub from: Option<String>,
    #[serde(default)]
    pub children: Vec<WaitResult>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReadRequest {
    pub terminal: TerminalAddress,
    #[serde(default)]
    pub lines: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller: Option<TerminalAddress>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReadResult {
    pub text: String,
    pub complete: bool,
    pub how: String,
    #[serde(default)]
    pub why: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HolderInfo {
    pub pid: u32,
    pub instance: String,
    pub current: bool,
    #[serde(default)]
    pub terminals: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerStatus {
    pub core: CoreInfo,
    pub version: String,
    pub pid: u32,
    pub uptime_ms: u64,
    #[serde(default)]
    pub holders: Vec<HolderInfo>,
    #[serde(default)]
    pub holder: Option<HolderInfo>,
    #[serde(default)]
    pub live: u64,
    #[serde(default)]
    pub finished: u64,
    #[serde(default)]
    pub installed_by: Option<String>,
}

/// Filter accepted by `events.subscribe`. Any filter makes sequence gaps
/// expected: other terminals' events still consume sequence numbers.
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct EventFilter {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal: Option<TerminalAddress>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub children_of: Option<TerminalAddress>,
}

impl EventFilter {
    pub fn is_filtered(&self) -> bool {
        self.terminal.is_some() || !self.labels.is_empty() || self.children_of.is_some()
    }

    pub fn to_params(&self) -> Result<Value> {
        Ok(serde_json::to_value(self)?)
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SubscribeResult {
    pub seq: u64,
    #[serde(default)]
    pub terminals: Vec<Terminal>,
    #[serde(default)]
    pub channel: Option<u32>,
}

/// Compile-time guard used by tests: typed DTOs must be deserialisable.
#[cfg(test)]
fn _assert_public_types() {
    fn assert<T: serde::de::DeserializeOwned>() {}
    assert::<Terminal>();
    assert::<ListResult>();
    assert::<AttachResult>();
    assert::<SendResult>();
    assert::<InterruptResult>();
    assert::<StopResult>();
    assert::<ServerStatus>();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_names_and_addresses() {
        assert!(validate_terminal_name("agent-1").is_ok());
        assert!(validate_terminal_name("Agent").is_err());
        assert!(validate_terminal_name("").is_err());
        assert!(TerminalAddress::parse("t7").is_ok());
        assert!(TerminalAddress::parse("rohan/t7").is_ok());
        assert!(TerminalAddress::parse("rohan/t9/extra").is_err());
        assert!(TerminalAddress::parse("room/../t9").is_err());
    }

    #[test]
    fn parses_terminal_record_and_preserves_unknown_states() {
        let value = json!({
            "address":"t7","argv":["codex"],"cwd":"/work","state":"live",
            "created_ms":1,"screen":"exact","cols":80,"rows":24,
            "labels":{"role":"coder"},"parent":"t1","status":{"state":"working"},
            "reason":"something_new"
        });
        let terminal = Terminal::from_value(value).unwrap();
        assert!(terminal.state.is_live());
        assert_eq!(terminal.parent.unwrap().as_str(), "t1");
        assert_eq!(terminal.status.unwrap().state, "working");
        assert_eq!(
            terminal.reason,
            Some(TerminalReason::Other("something_new".into()))
        );
    }

    #[test]
    fn run_request_serializes_only_documented_fields() {
        let request = RunRequest {
            argv: vec!["codex".into()],
            name: Some("agent-1".into()),
            cwd: Some("/work".into()),
            keep: true,
            ..RunRequest::default()
        };
        let params = request.to_params().unwrap();
        assert_eq!(params["name"], "agent-1");
        assert_eq!(params["keep"], true);
        assert!(params.get("line").is_none());
        assert!(params.get("on").is_none());
    }

    #[test]
    fn hello_rejects_unsupported_generation() {
        let value = json!({
            "generation":2,"version":"0.2.0","core":{"id":"c","name":"n"},
            "framed":true,"methods":["hello"],"features":[]
        });
        assert!(matches!(
            Hello::from_result(&value),
            Err(HcError::Remote(error)) if error.kind == crate::error::RemoteKind::UnsupportedGeneration
        ));
    }
}
