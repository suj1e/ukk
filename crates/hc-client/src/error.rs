//! Typed errors for the herdr-core client.
//!
//! Remote failures keep the stable `data.kind` word documented by the pinned
//! OpenRPC schema instead of exposing an untyped JSON value. Local failures
//! (framing, timeouts, unsupported capabilities) have their own variants so a
//! caller can distinguish "the core said no" from "the client refused" or
//! "the transport died".

use serde_json::Value;
use std::error::Error;
use std::fmt;
use std::io;

pub type Result<T, E = HcError> = std::result::Result<T, E>;

/// The stable error kinds herdr-core documents in `error.data.kind`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteKind {
    NotFound,
    Finished,
    Unavailable,
    Blocked,
    StatusMismatch,
    InvalidParams,
    NameTaken,
    SpawnFailed,
    InvalidTemplate,
    MachineUnknown,
    Config,
    Live,
    NeedsFramed,
    Conflict,
    UnsupportedGeneration,
    NeedsTarget,
    Other(String),
}

impl RemoteKind {
    pub fn from_wire(kind: Option<&str>) -> Self {
        match kind {
            Some("not_found") => Self::NotFound,
            Some("finished") => Self::Finished,
            Some("unavailable") => Self::Unavailable,
            Some("blocked") => Self::Blocked,
            Some("status_mismatch") => Self::StatusMismatch,
            Some("invalid_params") => Self::InvalidParams,
            Some("name_taken") => Self::NameTaken,
            Some("spawn_failed") => Self::SpawnFailed,
            Some("invalid_template") => Self::InvalidTemplate,
            Some("machine_unknown") => Self::MachineUnknown,
            Some("config") => Self::Config,
            Some("live") => Self::Live,
            Some("needs_framed") => Self::NeedsFramed,
            Some("conflict") => Self::Conflict,
            Some("unsupported_generation") => Self::UnsupportedGeneration,
            Some("needs_target") => Self::NeedsTarget,
            Some(other) => Self::Other(other.to_owned()),
            None => Self::Other(String::new()),
        }
    }

    pub fn as_wire(&self) -> &str {
        match self {
            Self::NotFound => "not_found",
            Self::Finished => "finished",
            Self::Unavailable => "unavailable",
            Self::Blocked => "blocked",
            Self::StatusMismatch => "status_mismatch",
            Self::InvalidParams => "invalid_params",
            Self::NameTaken => "name_taken",
            Self::SpawnFailed => "spawn_failed",
            Self::InvalidTemplate => "invalid_template",
            Self::MachineUnknown => "machine_unknown",
            Self::Config => "config",
            Self::Live => "live",
            Self::NeedsFramed => "needs_framed",
            Self::Conflict => "conflict",
            Self::UnsupportedGeneration => "unsupported_generation",
            Self::NeedsTarget => "needs_target",
            Self::Other(other) => other,
        }
    }
}

/// A JSON-RPC error returned by herdr-core.
#[derive(Debug, Clone, PartialEq)]
pub struct RemoteError {
    pub code: i64,
    pub message: String,
    pub kind: RemoteKind,
    pub hint: Option<String>,
    /// The original `data` object. Kept for diagnostics, never for control
    /// flow: callers branch on [`RemoteError::kind`].
    pub data: Value,
}

impl RemoteError {
    pub fn from_value(error: &Value) -> Self {
        let code = error.get("code").and_then(Value::as_i64).unwrap_or(-32000);
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("herdr-core error")
            .to_owned();
        let data = error.get("data").cloned().unwrap_or(Value::Null);
        let kind = RemoteKind::from_wire(data.get("kind").and_then(Value::as_str));
        let hint = data.get("hint").and_then(Value::as_str).map(str::to_owned);
        Self {
            code,
            message,
            kind,
            hint,
            data,
        }
    }
}

impl fmt::Display for RemoteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "herdr-core {} (code {}): {}",
            self.kind.as_wire(),
            self.code,
            self.message
        )?;
        if let Some(hint) = &self.hint {
            write!(f, " — {hint}")?;
        }
        Ok(())
    }
}

/// A framing failure. Once a frame boundary is lost the connection cannot be
/// resynchronised, so the reader closes the whole connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameError {
    /// The header declared more than the documented 16 MiB payload limit.
    Oversized { length: usize, limit: usize },
    /// EOF in the middle of a header or payload.
    Truncated { expected: usize, got: usize },
    /// A known kind arrived with a payload the protocol cannot interpret.
    InvalidPayload(String),
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Oversized { length, limit } => {
                write!(f, "frame payload {length} exceeds limit {limit}")
            }
            Self::Truncated { expected, got } => {
                write!(f, "truncated frame: wanted {expected} bytes, got {got}")
            }
            Self::InvalidPayload(message) => write!(f, "invalid frame payload: {message}"),
        }
    }
}

#[derive(Debug)]
pub enum HcError {
    Io(io::Error),
    Json(serde_json::Error),
    Protocol(String),
    Remote(RemoteError),
    /// The connected core does not advertise the method or feature the caller
    /// requires. Never call an undocumented method as a fallback.
    Unsupported {
        method: String,
        reason: String,
    },
    /// A caller-supplied request failed local validation.
    InvalidArgument(String),
    Timeout {
        operation: String,
        seconds: u64,
    },
    /// The connection is gone. The message says why when known.
    Closed(String),
    Frame(FrameError),
    /// The consumer fell too far behind and the bounded queue was discarded.
    Backpressure {
        limit: usize,
    },
    /// An event sequence skipped ahead of the last delivered event.
    SequenceGap {
        expected: u64,
        got: u64,
    },
    /// A repeated idempotency key carried a different request.
    IdempotencyConflict(String),
}

impl HcError {
    pub fn kind(&self) -> Option<&RemoteKind> {
        match self {
            Self::Remote(error) => Some(&error.kind),
            _ => None,
        }
    }

    pub fn remote_is(&self, kind: RemoteKind) -> bool {
        matches!(self, Self::Remote(error) if error.kind == kind)
    }
}

impl fmt::Display for HcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "hc I/O error: {error}"),
            Self::Json(error) => write!(f, "hc JSON error: {error}"),
            Self::Protocol(message) => write!(f, "hc protocol error: {message}"),
            Self::Remote(error) => write!(f, "hc RPC error: {error}"),
            Self::Unsupported { method, reason } => {
                write!(f, "hc capability unavailable for {method}: {reason}")
            }
            Self::InvalidArgument(message) => write!(f, "invalid hc argument: {message}"),
            Self::Timeout { operation, seconds } => {
                write!(f, "hc {operation} timed out after {seconds}s")
            }
            Self::Closed(reason) => write!(f, "hc connection closed: {reason}"),
            Self::Frame(error) => write!(f, "hc frame error: {error}"),
            Self::Backpressure { limit } => {
                write!(f, "hc stream consumer exceeded {limit} buffered bytes")
            }
            Self::SequenceGap { expected, got } => {
                write!(f, "hc event sequence gap: expected {expected}, got {got}")
            }
            Self::IdempotencyConflict(key) => {
                write!(
                    f,
                    "hc idempotency key {key} was reused for a different request"
                )
            }
        }
    }
}

impl Error for HcError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Json(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for HcError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for HcError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

impl From<FrameError> for HcError {
    fn from(error: FrameError) -> Self {
        Self::Frame(error)
    }
}
