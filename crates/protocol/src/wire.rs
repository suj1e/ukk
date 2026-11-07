//! Wire messages exchanged after transport authentication.
//!
//! `type` selects the message shape. The envelope generation inside commands,
//! events and subscriptions must equal the negotiated generation; the handshake
//! itself carries the client's supported list and is checked by
//! [`crate::service::ProtocolService::accept_hello`].
//!
//! Snapshots have their own size bound because they carry durable domain state;
//! all other messages are bounded by the ordinary envelope limit.

use crate::authz::Denial;
use crate::cursor::{DurableCursor, StreamBounds};
use crate::device::DeviceClaim;
use crate::envelope::{
    decode_strict, encode_strict, CommandEnvelope, EnvelopeError, EventEnvelope, Snapshot,
};
use crate::id::{RequestId, StreamId};
use crate::idempotency::CommandResultEnvelope;
use crate::limits::{LimitError, Limits};
use crate::reserved;
use crate::scope::ScopeSet;
use crate::version::{Features, ProtocolGeneration};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::error::Error;
use std::fmt;

/// Maximum human-readable error message on the wire.
pub const MAX_ERROR_MESSAGE_BYTES: usize = 512;
const SNAPSHOT_WRAPPER_BYTES: usize = 512;

/// First client message on every connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientHello {
    pub request_id: RequestId,
    pub supported_generations: Vec<ProtocolGeneration>,
    /// Present only over a remote transport; local peers are authenticated by
    /// the OS and must not claim a device identity.
    pub device: Option<DeviceClaim>,
}

impl ClientHello {
    #[must_use]
    pub fn local(request_id: RequestId, supported_generations: Vec<ProtocolGeneration>) -> Self {
        Self {
            request_id,
            supported_generations,
            device: None,
        }
    }

    #[must_use]
    pub fn remote(
        request_id: RequestId,
        supported_generations: Vec<ProtocolGeneration>,
        device: DeviceClaim,
    ) -> Self {
        Self {
            request_id,
            supported_generations,
            device: Some(device),
        }
    }
}

/// Server handshake response, including the cursor bounds a client may resume.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerHello {
    pub protocol: ProtocolGeneration,
    pub request_id: RequestId,
    pub features: Features,
    pub scopes: ScopeSet,
    pub streams: Vec<StreamBounds>,
}

/// Request replay of durable events after a cursor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubscribeRequest {
    pub protocol: ProtocolGeneration,
    pub stream: StreamId,
    pub cursor: DurableCursor,
    pub max_batch: usize,
}

impl SubscribeRequest {
    #[must_use]
    pub fn new(
        protocol: ProtocolGeneration,
        stream: StreamId,
        cursor: DurableCursor,
        max_batch: usize,
    ) -> Self {
        Self {
            protocol,
            stream,
            cursor,
            max_batch,
        }
    }
}

/// Durable acknowledgement for one stream. Acking is advisory for retention;
/// clients always resume from their persisted checkpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AckRequest {
    pub protocol: ProtocolGeneration,
    pub stream: StreamId,
    pub cursor: DurableCursor,
}

impl AckRequest {
    #[must_use]
    pub fn new(protocol: ProtocolGeneration, stream: StreamId, cursor: DurableCursor) -> Self {
        Self {
            protocol,
            stream,
            cursor,
        }
    }
}

/// Client to Host message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ClientMessage {
    Hello(ClientHello),
    Command(CommandEnvelope),
    Subscribe(SubscribeRequest),
    Ack(AckRequest),
}

impl ClientMessage {
    pub fn encode(&self, limits: &Limits) -> Result<String, EnvelopeError> {
        encode_strict(self, limits)
    }

    pub fn decode(raw: &str, limits: &Limits) -> Result<Self, EnvelopeError> {
        let message: Self = decode_strict(raw, limits)?;
        match &message {
            Self::Hello(_) | Self::Ack(_) => {}
            Self::Command(envelope) => envelope.validate(limits)?,
            Self::Subscribe(request) => limits.check_replay_batch(request.max_batch)?,
        }
        Ok(message)
    }

    #[must_use]
    pub fn protocol(&self) -> Option<ProtocolGeneration> {
        match self {
            Self::Hello(_) => None,
            Self::Command(envelope) => Some(envelope.protocol),
            Self::Subscribe(request) => Some(request.protocol),
            Self::Ack(request) => Some(request.protocol),
        }
    }
}

/// Typed server-side failure delivered as data, so the transport can keep the
/// connection open for a corrected request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerError {
    pub protocol: ProtocolGeneration,
    pub request_id: Option<RequestId>,
    pub denial: Option<Denial>,
    pub message: String,
}

/// Host to client message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ServerMessage {
    Hello(ServerHello),
    Result(CommandResultEnvelope),
    Event(EventEnvelope),
    Snapshot(Snapshot),
    Error(ServerError),
}

impl ServerMessage {
    pub fn encode(&self, limits: &Limits) -> Result<String, EnvelopeError> {
        match self {
            Self::Event(event) => {
                event.encode(limits)?;
            }
            Self::Snapshot(snapshot) => {
                snapshot.encode(limits)?;
            }
            Self::Result(_) | Self::Hello(_) | Self::Error(_) => {}
        }
        if let Self::Error(error) = self {
            if error.message.len() > MAX_ERROR_MESSAGE_BYTES {
                return Err(EnvelopeError::Invalid("error message is too long"));
            }
        }
        let encoded =
            serde_json::to_string(self).map_err(|error| EnvelopeError::Json(error.to_string()))?;
        match self {
            Self::Snapshot(_) => {
                if encoded.len()
                    > limits
                        .max_snapshot_bytes
                        .saturating_add(SNAPSHOT_WRAPPER_BYTES)
                {
                    return Err(EnvelopeError::Limit(LimitError::SnapshotTooLarge {
                        actual: encoded.len(),
                        max: limits.max_snapshot_bytes,
                    }));
                }
            }
            Self::Event(_) | Self::Result(_) | Self::Hello(_) | Self::Error(_) => {
                limits.check_envelope(encoded.len())?;
            }
        }
        Ok(encoded)
    }

    pub fn decode(raw: &str, limits: &Limits) -> Result<Self, EnvelopeError> {
        let ceiling = limits
            .max_snapshot_bytes
            .saturating_add(SNAPSHOT_WRAPPER_BYTES);
        if raw.len() > ceiling {
            return Err(EnvelopeError::Limit(LimitError::SnapshotTooLarge {
                actual: raw.len(),
                max: limits.max_snapshot_bytes,
            }));
        }
        let value: Value =
            serde_json::from_str(raw).map_err(|error| EnvelopeError::Json(error.to_string()))?;
        if let Some(field) = reserved::scan_value(&value) {
            return Err(EnvelopeError::ReservedEphemeralField(field));
        }
        let message: Self = serde_json::from_value(value)
            .map_err(|error| EnvelopeError::Json(error.to_string()))?;
        match &message {
            Self::Event(event) => event.event.validate(limits)?,
            Self::Snapshot(snapshot) => {
                snapshot.encode(limits)?;
            }
            Self::Error(error) => {
                if error.message.len() > MAX_ERROR_MESSAGE_BYTES {
                    return Err(EnvelopeError::Invalid("error message is too long"));
                }
                limits.check_envelope(raw.len())?;
            }
            Self::Result(_) | Self::Hello(_) => limits.check_envelope(raw.len())?,
        }
        Ok(message)
    }
}

impl fmt::Display for ServerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "protocol {}: {}", self.protocol, self.message)
    }
}

impl Error for ServerError {}
