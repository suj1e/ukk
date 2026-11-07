//! Typed command and event envelopes.
//!
//! Envelopes are the only shape admitted on the wire. Commands carry their own
//! idempotency key, request correlation id and negotiated generation; they do
//! *not* carry a device id or plane, because those are established by the
//! authenticated handshake and must not be re-declared per message.
//!
//! Encoding is bounded: payload size is checked before the envelope is
//! serialized, and decoding checks both the frame size and the payload size
//! before any command reaches authorization.

use crate::authz::{CommandClass, ReplayPolicy};
use crate::cursor::DurableCursor;
use crate::id::{DomainId, EventId, IdempotencyKey, LeaseId, RequestId, StreamId};
use crate::limits::{LimitError, Limits};
use crate::reserved;
use crate::scope::Scope;
use crate::version::{ProtocolGeneration, CURRENT_GENERATION};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::error::Error;
use std::fmt;

/// Maximum title length accepted by `session.create`.
pub const MAX_TITLE_BYTES: usize = 512;
/// Maximum pointer events in one `pointer_input` command.
pub const MAX_POINTER_EVENTS: usize = 32;
/// Maximum key code length in a pointer/keyboard event.
pub const MAX_KEY_CODE_BYTES: usize = 64;

/// Typed envelope failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvelopeError {
    Json(String),
    Limit(LimitError),
    /// A local runtime field such as `hc_seq` or `host_epoch` appeared on the
    /// durable wire.
    ReservedEphemeralField(&'static str),
    GenerationMismatch {
        expected: ProtocolGeneration,
        actual: ProtocolGeneration,
    },
    Invalid(&'static str),
}

impl fmt::Display for EnvelopeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(message) => write!(formatter, "invalid envelope JSON: {message}"),
            Self::Limit(error) => error.fmt(formatter),
            Self::ReservedEphemeralField(field) => write!(
                formatter,
                "field '{field}' is local runtime state and is not a durable protocol field"
            ),
            Self::GenerationMismatch { expected, actual } => {
                write!(formatter, "envelope is {actual}, connection is {expected}")
            }
            Self::Invalid(reason) => write!(formatter, "invalid envelope: {reason}"),
        }
    }
}

impl Error for EnvelopeError {}

impl From<LimitError> for EnvelopeError {
    fn from(error: LimitError) -> Self {
        Self::Limit(error)
    }
}

pub(crate) fn encode_strict<T: Serialize>(
    value: &T,
    limits: &Limits,
) -> Result<String, EnvelopeError> {
    let encoded =
        serde_json::to_string(value).map_err(|error| EnvelopeError::Json(error.to_string()))?;
    limits.check_envelope(encoded.len())?;
    Ok(encoded)
}

pub(crate) fn decode_strict<T: DeserializeOwned>(
    raw: &str,
    limits: &Limits,
) -> Result<T, EnvelopeError> {
    limits.check_envelope(raw.len())?;
    let value: Value =
        serde_json::from_str(raw).map_err(|error| EnvelopeError::Json(error.to_string()))?;
    if let Some(field) = reserved::scan_value(&value) {
        return Err(EnvelopeError::ReservedEphemeralField(field));
    }
    serde_json::from_value(value).map_err(|error| EnvelopeError::Json(error.to_string()))
}

fn serialized_len<T: Serialize>(value: &T) -> Result<usize, EnvelopeError> {
    serde_json::to_vec(value)
        .map(|bytes| bytes.len())
        .map_err(|error| EnvelopeError::Json(error.to_string()))
}

/// Create a new Session inside an existing Space.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionCreate {
    pub space_id: DomainId,
    pub title: String,
    pub expected_revision: Option<u64>,
}

/// Dispatch text to one Agent run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentSend {
    pub session_id: DomainId,
    pub agent_run_id: DomainId,
    pub text: String,
    pub expected_revision: Option<u64>,
}

/// Structured approval decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    Approve,
    Deny,
}

/// Answer an approval request. A revision is mandatory: approvals are never
/// applied against an unknown layout or agent state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalRespond {
    pub session_id: DomainId,
    pub approval_id: DomainId,
    pub decision: ApprovalDecision,
    pub revision: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PointerButton {
    Primary,
    Secondary,
    Middle,
}

/// One live input event. Coordinates use the remote screen's logical space.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "input", rename_all = "snake_case", deny_unknown_fields)]
pub enum PointerEvent {
    Move {
        x: f32,
        y: f32,
    },
    Button {
        button: PointerButton,
        pressed: bool,
    },
    Scroll {
        dx: f32,
        dy: f32,
    },
    Key {
        code: String,
        pressed: bool,
    },
}

impl PointerEvent {
    fn validate(&self) -> Result<(), EnvelopeError> {
        match self {
            Self::Move { x, y } => {
                if !x.is_finite() || !y.is_finite() {
                    return Err(EnvelopeError::Invalid("pointer coordinates must be finite"));
                }
            }
            Self::Scroll { dx, dy } => {
                if !dx.is_finite() || !dy.is_finite() {
                    return Err(EnvelopeError::Invalid("scroll deltas must be finite"));
                }
            }
            Self::Key { code, .. } => {
                if code.is_empty() || code.len() > MAX_KEY_CODE_BYTES {
                    return Err(EnvelopeError::Invalid("key code length is out of range"));
                }
            }
            Self::Button { .. } => {}
        }
        Ok(())
    }
}

/// Remote pointer/keyboard input, never replayed after reconnect.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PointerInput {
    pub session_id: DomainId,
    /// Required for remote peers; local input uses the OS permission path.
    pub lease_id: Option<LeaseId>,
    pub events: Vec<PointerEvent>,
}

/// Revoke another paired device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceRevoke {
    pub device_id: crate::id::DeviceId,
}

/// Typed client command.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    SessionCreate(SessionCreate),
    AgentSend(AgentSend),
    ApprovalRespond(ApprovalRespond),
    PointerInput(PointerInput),
    DeviceRevoke(DeviceRevoke),
}

impl Command {
    /// Authorization scope required for every peer that is not the local owner.
    #[must_use]
    pub fn required_scope(&self) -> Scope {
        match self {
            Self::SessionCreate(_) => Scope::SessionManage,
            Self::AgentSend(_) => Scope::AgentSend,
            Self::ApprovalRespond(_) => Scope::ApprovalRespond,
            Self::PointerInput(_) => Scope::ScreenControl,
            Self::DeviceRevoke(_) => Scope::DeviceManage,
        }
    }

    #[must_use]
    pub fn class(&self) -> CommandClass {
        match self {
            Self::SessionCreate(_) | Self::AgentSend(_) | Self::DeviceRevoke(_) => {
                CommandClass::IdempotentMutation
            }
            Self::ApprovalRespond(_) => CommandClass::HumanDecision,
            Self::PointerInput(_) => CommandClass::EphemeralInput,
        }
    }

    #[must_use]
    pub fn replay_policy(&self) -> ReplayPolicy {
        self.class().replay_policy()
    }

    /// Semantic and size validation applied before authorization and again
    /// before execution.
    pub fn validate(&self, limits: &Limits) -> Result<(), EnvelopeError> {
        limits.check_payload(serialized_len(self)?)?;
        match self {
            Self::SessionCreate(create) => {
                if create.title.is_empty() || create.title.len() > MAX_TITLE_BYTES {
                    return Err(EnvelopeError::Invalid(
                        "session title length is out of range",
                    ));
                }
            }
            Self::AgentSend(send) => {
                if send.text.is_empty() {
                    return Err(EnvelopeError::Invalid("agent message must not be empty"));
                }
            }
            Self::ApprovalRespond(respond) => {
                if respond.revision == 0 {
                    return Err(EnvelopeError::Invalid(
                        "approval requires a positive revision",
                    ));
                }
            }
            Self::PointerInput(input) => {
                if input.events.is_empty() {
                    return Err(EnvelopeError::Invalid("pointer input has no events"));
                }
                if input.events.len() > MAX_POINTER_EVENTS {
                    return Err(EnvelopeError::Invalid("too many pointer events"));
                }
                for event in &input.events {
                    event.validate()?;
                }
            }
            Self::DeviceRevoke(_) => {}
        }
        Ok(())
    }
}

/// Versioned command envelope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandEnvelope {
    pub protocol: ProtocolGeneration,
    pub request_id: RequestId,
    pub idempotency_key: IdempotencyKey,
    pub issued_at_ms: i64,
    pub command: Command,
}

impl CommandEnvelope {
    /// Builds an envelope for the current generation.
    #[must_use]
    pub fn new(command: Command, issued_at_ms: i64) -> Self {
        Self {
            protocol: CURRENT_GENERATION,
            request_id: RequestId::new_random(),
            idempotency_key: IdempotencyKey::new_random(),
            issued_at_ms,
            command,
        }
    }

    #[must_use]
    pub fn replay_policy(&self) -> ReplayPolicy {
        self.command.replay_policy()
    }

    pub fn validate(&self, limits: &Limits) -> Result<(), EnvelopeError> {
        self.command.validate(limits)
    }

    pub fn encode(&self, limits: &Limits) -> Result<String, EnvelopeError> {
        self.command.validate(limits)?;
        encode_strict(self, limits)
    }

    /// Decodes and validates a command envelope. The negotiated generation is
    /// checked by the service, not guessed here.
    pub fn decode(raw: &str, limits: &Limits) -> Result<Self, EnvelopeError> {
        let envelope: Self = decode_strict(raw, limits)?;
        envelope.command.validate(limits)?;
        Ok(envelope)
    }
}

/// Session lifecycle events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionCreated {
    pub space_id: DomainId,
    pub session_id: DomainId,
    pub title: String,
    pub revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionArchived {
    pub session_id: DomainId,
    pub revision: u64,
}

/// Agent runtime state as reported by a structured adapter. Never inferred from
/// terminal bytes; `Unknown` is an explicit state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Idle,
    Running,
    WaitingForApproval,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentRunStatusChanged {
    pub session_id: DomainId,
    pub agent_run_id: DomainId,
    pub status: AgentStatus,
    pub revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalRequested {
    pub session_id: DomainId,
    pub approval_id: DomainId,
    pub summary: String,
    pub revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalResolved {
    pub session_id: DomainId,
    pub approval_id: DomainId,
    pub decision: ApprovalDecision,
    pub revision: u64,
}

/// Typed durable event delivered on one stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case", deny_unknown_fields)]
pub enum Event {
    SessionCreated(SessionCreated),
    SessionArchived(SessionArchived),
    AgentRunStatusChanged(AgentRunStatusChanged),
    ApprovalRequested(ApprovalRequested),
    ApprovalResolved(ApprovalResolved),
}

impl Event {
    pub fn validate(&self, limits: &Limits) -> Result<(), EnvelopeError> {
        limits.check_payload(serialized_len(self)?)?;
        match self {
            Self::ApprovalRequested(requested) => {
                if requested.summary.len() > MAX_TITLE_BYTES {
                    return Err(EnvelopeError::Invalid("approval summary is too long"));
                }
            }
            Self::SessionCreated(created) => {
                if created.title.is_empty() || created.title.len() > MAX_TITLE_BYTES {
                    return Err(EnvelopeError::Invalid(
                        "session title length is out of range",
                    ));
                }
            }
            Self::SessionArchived(_)
            | Self::AgentRunStatusChanged(_)
            | Self::ApprovalResolved(_) => {}
        }
        Ok(())
    }
}

/// Versioned durable event envelope. The cursor is assigned by the event log;
/// clients may never choose it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventEnvelope {
    pub protocol: ProtocolGeneration,
    pub event_id: EventId,
    pub stream: StreamId,
    pub cursor: DurableCursor,
    pub emitted_at_ms: i64,
    pub event: Event,
}

impl EventEnvelope {
    pub fn encode(&self, limits: &Limits) -> Result<String, EnvelopeError> {
        self.event.validate(limits)?;
        encode_strict(self, limits)
    }

    pub fn decode(raw: &str, limits: &Limits) -> Result<Self, EnvelopeError> {
        let envelope: Self = decode_strict(raw, limits)?;
        envelope.event.validate(limits)?;
        Ok(envelope)
    }
}

/// Durable state snapshot at one cursor. Clients resume from `at_cursor` after
/// accepting the body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub protocol: ProtocolGeneration,
    pub stream: StreamId,
    pub at_cursor: DurableCursor,
    pub revision: u64,
    pub body: Value,
}

impl Snapshot {
    #[must_use]
    pub fn new(stream: StreamId, at_cursor: DurableCursor, revision: u64, body: Value) -> Self {
        Self {
            protocol: CURRENT_GENERATION,
            stream,
            at_cursor,
            revision,
            body,
        }
    }

    pub fn encode(&self, limits: &Limits) -> Result<String, EnvelopeError> {
        if !self.body.is_object() {
            return Err(EnvelopeError::Invalid(
                "snapshot body must be a JSON object",
            ));
        }
        let encoded =
            serde_json::to_string(self).map_err(|error| EnvelopeError::Json(error.to_string()))?;
        limits.check_snapshot(encoded.len())?;
        Ok(encoded)
    }

    pub fn decode(raw: &str, limits: &Limits) -> Result<Self, EnvelopeError> {
        limits.check_snapshot(raw.len())?;
        let value: Value =
            serde_json::from_str(raw).map_err(|error| EnvelopeError::Json(error.to_string()))?;
        if let Some(field) = reserved::scan_value(&value) {
            return Err(EnvelopeError::ReservedEphemeralField(field));
        }
        let snapshot: Self = serde_json::from_value(value)
            .map_err(|error| EnvelopeError::Json(error.to_string()))?;
        if !snapshot.body.is_object() {
            return Err(EnvelopeError::Invalid(
                "snapshot body must be a JSON object",
            ));
        }
        Ok(snapshot)
    }
}
