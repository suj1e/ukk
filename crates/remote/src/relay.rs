//! ukk-internal relay signalling envelope (version 1) and driver state machine.
//!
//! ## What this is, and what it is not
//!
//! This module defines a **ukk-internal** signalling envelope used to carry
//! bounded opaque pairing frames between a Host (or phone) and a relay. It is
//! deliberately minimal and versioned. It is **not** the alma-relay wire
//! protocol and no interoperability is claimed: alma-relay multiplexes HTTP
//! requests and bridged WebSockets (`register`/`registered`, `hreq`/`hres`,
//! `wopen`/`wmsg`, …), while this envelope carries opaque per-session frames.
//! See [docs/PAIRING.md](../../../docs/PAIRING.md) for the exact audit record
//! and gaps (frame format, authentication, TURN).
//!
//! ## Outbound-only rule
//!
//! The Host is always the outbound peer: it connects to the relay and sends a
//! [`RelayMessage::Register`]. The relay never dials the Host, and this crate
//! never opens a listener. The phone connects to the relay, asks for the Host
//! by `host_id`, and receives a relay-assigned session id.
//!
//! ## Envelope v1
//!
//! One JSON object per frame:
//!
//! ```text
//! {"type":"register","body":{"v":1,"host_id":"…","generation":1}}
//! ```
//!
//! Every body carries `v`; unknown fields, wrong types and trailing input are
//! rejected. Messages are:
//!
//! | Type | Direction | Purpose |
//! | --- | --- | --- |
//! | `register` | Host → relay | Outbound registration. `generation` is the reconnect generation. |
//! | `registered` | relay → Host | Registration accepted; echoes `host_id`/`generation`. |
//! | `connect` | phone → relay | Ask to be routed to `host_id`. |
//! | `session` | relay → phone | Route established; carries the relay session id. |
//! | `route` | relay → Host | A phone session was routed to this Host. |
//! | `frame` | both | One opaque pairing frame for the session (base64url, ≤ 4096 bytes). |
//! | `close` | both | Close the routed session with a typed reason. |
//! | `error` | relay → peer | Typed refusal (`duplicate_registration`, `unknown_route`, …). |
//!
//! ## Bounds and failure posture
//!
//! * An encoded envelope is bounded by [`MAX_SIGNALLING_FRAME_BYTES`].
//! * The opaque pairing payload is bounded by [`MAX_SIGNALLING_PAYLOAD_BYTES`]
//!   (the same 4096-byte bound as the pairing handshake).
//! * Session ids are relay-assigned `[A-Za-z0-9_-]{16,128}`.
//! * Any unexpected message, stale generation, wrong Host, duplicate
//!   registration, unknown route or oversized frame closes the driver. State
//!   never survives a reconnect generation. Resume is not part of v1, so a
//!   frame carrying resume state is rejected as an unknown field rather than
//!   guessed.
//!
//! ## Scope
//!
//! [`RelayDriver`] tracks **one** active routed session per connection. A
//! multiplexing production adapter would keep a per-session table; that is
//! deliberately out of scope here, and the driver fails closed instead of
//! silently misrouting a second route.

use crate::encoding::encode;
use crate::error::MAX_PAIRING_FRAME_BYTES;
use crate::invite::HostId;
use crate::transport::RelayConnection;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::error::Error;
use std::fmt;

/// Version of the ukk-internal signalling envelope.
pub const SIGNALLING_VERSION: u8 = 1;

/// Hard bound on an encoded signalling envelope.
pub const MAX_SIGNALLING_FRAME_BYTES: usize = 8192;

/// Hard bound on the opaque pairing payload carried by one `frame` message.
pub const MAX_SIGNALLING_PAYLOAD_BYTES: usize = MAX_PAIRING_FRAME_BYTES;

/// Exact maximum length of the canonical unpadded base64url encoding of one
/// payload, checked before allocating the decode buffer.
pub const MAX_SIGNALLING_PAYLOAD_ENCODED_BYTES: usize = encoded_len(MAX_SIGNALLING_PAYLOAD_BYTES);

const fn encoded_len(bytes: usize) -> usize {
    let full = bytes / 3 * 4;
    match bytes % 3 {
        0 => full,
        1 => full + 2,
        _ => full + 3,
    }
}

/// Minimum accepted relay session id length.
pub const MIN_SESSION_ID_BYTES: usize = 16;

/// Maximum accepted relay session id length.
pub const MAX_SESSION_ID_BYTES: usize = 128;

/// Typed failure for envelope parsing, routing and driver state transitions.
///
/// No variant carries frame contents, a session secret or a pairing payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelayError {
    /// Encoded envelope exceeds [`MAX_SIGNALLING_FRAME_BYTES`].
    FrameTooLarge { max: usize },
    /// Opaque pairing payload exceeds [`MAX_SIGNALLING_PAYLOAD_BYTES`].
    PayloadTooLarge { max: usize },
    /// The envelope is not valid JSON or has unknown/missing/wrong fields.
    Malformed,
    /// The envelope version is not [`SIGNALLING_VERSION`].
    UnsupportedVersion { found: u8 },
    /// A relay session id is malformed.
    InvalidSessionId,
    /// A reconnect generation of `0` means the adapter is not connected.
    Offline,
    /// The connection has not been registered/routed yet.
    NotConnected,
    /// A second registration for the same Host was refused by the relay.
    DuplicateRegistration,
    /// No session matches the requested id (or the sender is not a peer).
    UnknownRoute,
    /// The Host is not registered with the relay.
    NotRegistered,
    /// The routed session was already closed.
    SessionClosed,
    /// The relay or the local user revoked the route.
    Revoked,
    /// The routed session was closed; no further frames may be sent.
    Closed,
    /// The message is not valid in the current role/state.
    UnexpectedMessage,
    /// This connection generation already used `begin`.
    AlreadyConnected,
    /// A message was tagged with a stale or future reconnect generation.
    GenerationMismatch { expected: u64, found: u64 },
    /// The envelope names another Host.
    HostMismatch,
    /// The driver's state has no routed session.
    NotRouted,
    /// The relay returned a typed refusal with no more specific local variant.
    RelayRefused(RelayErrorCode),
}

impl fmt::Display for RelayError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FrameTooLarge { max } => {
                write!(formatter, "signalling frame exceeds {max} bytes")
            }
            Self::PayloadTooLarge { max } => {
                write!(formatter, "signalling payload exceeds {max} bytes")
            }
            Self::Malformed => formatter.write_str("signalling envelope is malformed"),
            Self::UnsupportedVersion { found } => {
                write!(formatter, "signalling version {found} is not supported")
            }
            Self::InvalidSessionId => formatter.write_str("relay session id is malformed"),
            Self::Offline => formatter.write_str("relay connection is not established"),
            Self::NotConnected => formatter.write_str("relay connection is not registered"),
            Self::DuplicateRegistration => {
                formatter.write_str("duplicate relay registration refused")
            }
            Self::UnknownRoute => formatter.write_str("relay route is unknown"),
            Self::NotRegistered => formatter.write_str("Host is not registered with the relay"),
            Self::SessionClosed => formatter.write_str("relay session is closed"),
            Self::Revoked => formatter.write_str("relay route was revoked"),
            Self::Closed => formatter.write_str("relay driver is closed"),
            Self::UnexpectedMessage => {
                formatter.write_str("unexpected message for the relay driver state")
            }
            Self::AlreadyConnected => {
                formatter.write_str("relay generation was already used for this connection")
            }
            Self::GenerationMismatch { expected, found } => write!(
                formatter,
                "relay generation mismatch: expected {expected}, found {found}"
            ),
            Self::HostMismatch => formatter.write_str("relay message names another Host"),
            Self::NotRouted => formatter.write_str("relay driver has no routed session"),
            Self::RelayRefused(code) => {
                write!(formatter, "relay refused the request: {code}")
            }
        }
    }
}

impl Error for RelayError {}

/// Typed refusal code carried by a [`RelayMessage::Error`] frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelayErrorCode {
    DuplicateRegistration,
    UnknownRoute,
    NotRegistered,
    SessionClosed,
    Revoked,
    FrameTooLarge,
    Malformed,
    UnsupportedVersion,
    Offline,
}

impl fmt::Display for RelayErrorCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::DuplicateRegistration => "duplicate_registration",
            Self::UnknownRoute => "unknown_route",
            Self::NotRegistered => "not_registered",
            Self::SessionClosed => "session_closed",
            Self::Revoked => "revoked",
            Self::FrameTooLarge => "frame_too_large",
            Self::Malformed => "malformed",
            Self::UnsupportedVersion => "unsupported_version",
            Self::Offline => "offline",
        };
        formatter.write_str(name)
    }
}

impl RelayErrorCode {
    fn into_error(self) -> RelayError {
        match self {
            Self::DuplicateRegistration => RelayError::DuplicateRegistration,
            Self::UnknownRoute => RelayError::UnknownRoute,
            Self::NotRegistered => RelayError::NotRegistered,
            Self::SessionClosed => RelayError::SessionClosed,
            Self::Revoked => RelayError::Revoked,
            Self::FrameTooLarge => RelayError::FrameTooLarge {
                max: MAX_SIGNALLING_FRAME_BYTES,
            },
            Self::Malformed => RelayError::Malformed,
            Self::UnsupportedVersion => RelayError::RelayRefused(Self::UnsupportedVersion),
            Self::Offline => RelayError::Offline,
        }
    }
}

/// Why a routed session was closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloseReason {
    /// The peer disconnected or the relay lost the Host/phone link.
    Offline,
    /// The route was revoked (Host revocation, relay policy or user action).
    Revoked,
    /// A new connection generation replaced this session.
    Reconnect,
    /// A peer or relay detected a protocol violation.
    ProtocolError,
}

/// Relay-assigned opaque session id: `[A-Za-z0-9_-]{16,128}`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SessionId(String);

impl SessionId {
    pub fn parse(raw: &str) -> Result<Self, RelayError> {
        let valid_len = (MIN_SESSION_ID_BYTES..=MAX_SESSION_ID_BYTES).contains(&raw.len());
        let valid_chars = raw
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_');
        if !valid_len || !valid_chars {
            return Err(RelayError::InvalidSessionId);
        }
        Ok(Self(raw.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl TryFrom<String> for SessionId {
    type Error = RelayError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<SessionId> for String {
    fn from(value: SessionId) -> Self {
        value.0
    }
}

/// Opaque, bounded pairing frame carried inside a session.
///
/// The bytes are canonical unpadded base64url on the wire. `Debug` exposes only
/// the length; pairing payloads are ciphertext but callers never need them in
/// logs.
#[derive(Clone, PartialEq, Eq)]
pub struct OpaqueFrame(Vec<u8>);

impl OpaqueFrame {
    pub fn new(payload: &[u8]) -> Result<Self, RelayError> {
        if payload.len() > MAX_SIGNALLING_PAYLOAD_BYTES {
            return Err(RelayError::PayloadTooLarge {
                max: MAX_SIGNALLING_PAYLOAD_BYTES,
            });
        }
        Ok(Self(payload.to_vec()))
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for OpaqueFrame {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "OpaqueFrame({} bytes)", self.0.len())
    }
}

impl Serialize for OpaqueFrame {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&encode(&self.0))
    }
}

impl<'de> Deserialize<'de> for OpaqueFrame {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        if value.len() > MAX_SIGNALLING_PAYLOAD_ENCODED_BYTES {
            return Err(D::Error::custom(
                "encoded frame exceeds the signalling payload bound",
            ));
        }
        let decoded = URL_SAFE_NO_PAD
            .decode(&value)
            .map_err(|_| D::Error::custom("frame must be canonical unpadded base64url"))?;
        if decoded.len() > MAX_SIGNALLING_PAYLOAD_BYTES {
            return Err(D::Error::custom(
                "frame exceeds the signalling payload bound",
            ));
        }
        if encode(&decoded) != value {
            return Err(D::Error::custom("frame is not canonical base64url"));
        }
        Ok(Self(decoded))
    }
}

/// One versioned ukk-internal signalling envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    content = "body",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum RelayMessage {
    /// Host → relay: register this outbound connection.
    Register {
        v: u8,
        host_id: HostId,
        generation: u64,
    },
    /// relay → Host: registration accepted.
    Registered {
        v: u8,
        host_id: HostId,
        generation: u64,
    },
    /// phone → relay: ask to be routed to a registered Host.
    Connect {
        v: u8,
        host_id: HostId,
        generation: u64,
    },
    /// relay → phone: route established.
    Session {
        v: u8,
        host_id: HostId,
        generation: u64,
        session_id: SessionId,
    },
    /// relay → Host: a phone was routed to this Host.
    Route {
        v: u8,
        generation: u64,
        session_id: SessionId,
    },
    /// Either peer → relay → other peer: one opaque pairing frame.
    Frame {
        v: u8,
        session_id: SessionId,
        frame: OpaqueFrame,
    },
    /// Either peer → relay → other peer: close the routed session.
    Close {
        v: u8,
        session_id: SessionId,
        reason: CloseReason,
    },
    /// relay → peer: typed refusal.
    Error { v: u8, code: RelayErrorCode },
}

impl RelayMessage {
    #[must_use]
    pub fn version(&self) -> u8 {
        match self {
            Self::Register { v, .. }
            | Self::Registered { v, .. }
            | Self::Connect { v, .. }
            | Self::Session { v, .. }
            | Self::Route { v, .. }
            | Self::Frame { v, .. }
            | Self::Close { v, .. }
            | Self::Error { v, .. } => *v,
        }
    }

    /// Encodes the envelope, enforcing the version and size bounds.
    pub fn encode(&self) -> Result<Vec<u8>, RelayError> {
        if self.version() != SIGNALLING_VERSION {
            return Err(RelayError::UnsupportedVersion {
                found: self.version(),
            });
        }
        let encoded = serde_json::to_vec(self).map_err(|_| RelayError::Malformed)?;
        if encoded.len() > MAX_SIGNALLING_FRAME_BYTES {
            return Err(RelayError::FrameTooLarge {
                max: MAX_SIGNALLING_FRAME_BYTES,
            });
        }
        Ok(encoded)
    }

    /// Decodes one envelope. Oversized input is rejected **before** parsing,
    /// and the frame must be a single JSON object: serde's adjacent tagging
    /// would otherwise also accept the two-element array form.
    pub fn decode(bytes: &[u8]) -> Result<Self, RelayError> {
        if bytes.len() > MAX_SIGNALLING_FRAME_BYTES {
            return Err(RelayError::FrameTooLarge {
                max: MAX_SIGNALLING_FRAME_BYTES,
            });
        }
        match bytes.iter().find(|byte| !byte.is_ascii_whitespace()) {
            Some(b'{') => {}
            _ => return Err(RelayError::Malformed),
        }
        let message: Self = serde_json::from_slice(bytes).map_err(|_| RelayError::Malformed)?;
        if message.version() != SIGNALLING_VERSION {
            return Err(RelayError::UnsupportedVersion {
                found: message.version(),
            });
        }
        Ok(message)
    }
}

/// Which endpoint a [`RelayDriver`] represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelayRole {
    /// The outbound Host registration.
    Host,
    /// The phone asking to be routed to a Host.
    Phone,
}

/// Explicit driver state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelayState {
    Idle,
    AwaitingRegistration { generation: u64 },
    Registered { generation: u64 },
    AwaitingSession { generation: u64 },
    Routed { session_id: SessionId },
    Closed,
}

/// Validated inbound progress reported by [`RelayDriver::handle`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelayEvent {
    RegisterAccepted {
        generation: u64,
    },
    SessionOpened {
        session_id: SessionId,
    },
    IncomingSession {
        session_id: SessionId,
    },
    Frame {
        session_id: SessionId,
        frame: OpaqueFrame,
    },
    Closed {
        session_id: Option<SessionId>,
        reason: CloseReason,
    },
}

/// Protocol state machine for one outbound relay connection.
///
/// The driver never performs I/O. The caller encodes the returned
/// [`RelayMessage`], writes it through a [`crate::RelayConnection`], decodes the
/// next inbound frame and passes it to [`RelayDriver::handle`]. Any invalid
/// transition closes the driver permanently for this generation; reconnecting
/// requires a higher generation and a fresh [`RelayDriver::begin`].
pub struct RelayDriver {
    role: RelayRole,
    host_id: HostId,
    generation: u64,
    state: RelayState,
}

impl RelayDriver {
    #[must_use]
    pub fn host(host_id: HostId) -> Self {
        Self {
            role: RelayRole::Host,
            host_id,
            generation: 0,
            state: RelayState::Idle,
        }
    }

    #[must_use]
    pub fn phone(host_id: HostId) -> Self {
        Self {
            role: RelayRole::Phone,
            host_id,
            generation: 0,
            state: RelayState::Idle,
        }
    }

    #[must_use]
    pub fn role(&self) -> RelayRole {
        self.role
    }

    #[must_use]
    pub fn state(&self) -> &RelayState {
        &self.state
    }

    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Starts a connection at the adapter's current generation.
    ///
    /// The generation is read from [`RelayConnection::generation`], never
    /// supplied by the caller. Generation `0` is not connected. A generation
    /// that was already used for this driver (including a closed one) is
    /// refused; an adapter must reconnect with a strictly larger generation.
    pub fn begin<C: RelayConnection + ?Sized>(
        &mut self,
        connection: &C,
    ) -> Result<RelayMessage, RelayError> {
        let generation = connection.generation();
        if generation == 0 {
            self.state = RelayState::Closed;
            return Err(RelayError::Offline);
        }
        if generation < self.generation {
            self.state = RelayState::Closed;
            return Err(RelayError::GenerationMismatch {
                expected: self.generation,
                found: generation,
            });
        }
        if generation == self.generation {
            self.state = RelayState::Closed;
            return Err(RelayError::AlreadyConnected);
        }
        self.generation = generation;
        match self.role {
            RelayRole::Host => {
                self.state = RelayState::AwaitingRegistration { generation };
                Ok(RelayMessage::Register {
                    v: SIGNALLING_VERSION,
                    host_id: self.host_id.clone(),
                    generation,
                })
            }
            RelayRole::Phone => {
                self.state = RelayState::AwaitingSession { generation };
                Ok(RelayMessage::Connect {
                    v: SIGNALLING_VERSION,
                    host_id: self.host_id.clone(),
                    generation,
                })
            }
        }
    }

    /// Decodes and handles one inbound envelope.
    pub fn handle_frame(&mut self, bytes: &[u8]) -> Result<RelayEvent, RelayError> {
        let message = RelayMessage::decode(bytes).inspect_err(|_| {
            self.state = RelayState::Closed;
        })?;
        self.handle(message)
    }

    /// Handles one inbound envelope and advances the state machine.
    pub fn handle(&mut self, message: RelayMessage) -> Result<RelayEvent, RelayError> {
        if message.version() != SIGNALLING_VERSION {
            return Err(self.fail(RelayError::UnsupportedVersion {
                found: message.version(),
            }));
        }
        let state = self.state.clone();
        match (self.role, state) {
            (_, RelayState::Idle) => Err(self.fail(RelayError::NotConnected)),
            (_, RelayState::Closed) => Err(RelayError::Closed),
            (RelayRole::Host, RelayState::AwaitingRegistration { generation }) => match message {
                RelayMessage::Registered {
                    host_id,
                    generation: found,
                    ..
                } => {
                    if host_id != self.host_id {
                        return Err(self.fail(RelayError::HostMismatch));
                    }
                    if found != generation {
                        return Err(self.fail(RelayError::GenerationMismatch {
                            expected: generation,
                            found,
                        }));
                    }
                    self.state = RelayState::Registered { generation };
                    Ok(RelayEvent::RegisterAccepted { generation })
                }
                RelayMessage::Error { code, .. } => Err(self.fail(code.into_error())),
                _ => Err(self.fail(RelayError::UnexpectedMessage)),
            },
            (RelayRole::Host, RelayState::Registered { generation }) => match message {
                RelayMessage::Route {
                    generation: found,
                    session_id,
                    ..
                } => {
                    if found != generation {
                        return Err(self.fail(RelayError::GenerationMismatch {
                            expected: generation,
                            found,
                        }));
                    }
                    self.state = RelayState::Routed {
                        session_id: session_id.clone(),
                    };
                    Ok(RelayEvent::IncomingSession { session_id })
                }
                RelayMessage::Error { code, .. } => Err(self.fail(code.into_error())),
                _ => Err(self.fail(RelayError::UnexpectedMessage)),
            },
            (RelayRole::Host, RelayState::Routed { session_id }) => match message {
                RelayMessage::Frame {
                    session_id: found,
                    frame,
                    ..
                } => {
                    if found != session_id {
                        return Err(self.fail(RelayError::UnknownRoute));
                    }
                    Ok(RelayEvent::Frame { session_id, frame })
                }
                RelayMessage::Close {
                    session_id: found,
                    reason,
                    ..
                } => {
                    if found != session_id {
                        return Err(self.fail(RelayError::UnknownRoute));
                    }
                    self.state = RelayState::Registered {
                        generation: self.generation,
                    };
                    Ok(RelayEvent::Closed {
                        session_id: Some(session_id),
                        reason,
                    })
                }
                RelayMessage::Error { code, .. } => Err(self.fail(code.into_error())),
                _ => Err(self.fail(RelayError::UnexpectedMessage)),
            },
            (RelayRole::Phone, RelayState::AwaitingSession { generation }) => match message {
                RelayMessage::Session {
                    host_id,
                    generation: found,
                    session_id,
                    ..
                } => {
                    if host_id != self.host_id {
                        return Err(self.fail(RelayError::HostMismatch));
                    }
                    if found != generation {
                        return Err(self.fail(RelayError::GenerationMismatch {
                            expected: generation,
                            found,
                        }));
                    }
                    self.state = RelayState::Routed {
                        session_id: session_id.clone(),
                    };
                    Ok(RelayEvent::SessionOpened { session_id })
                }
                RelayMessage::Error { code, .. } => Err(self.fail(code.into_error())),
                _ => Err(self.fail(RelayError::UnexpectedMessage)),
            },
            (RelayRole::Phone, RelayState::Routed { session_id }) => match message {
                RelayMessage::Frame {
                    session_id: found,
                    frame,
                    ..
                } => {
                    if found != session_id {
                        return Err(self.fail(RelayError::UnknownRoute));
                    }
                    Ok(RelayEvent::Frame { session_id, frame })
                }
                RelayMessage::Close {
                    session_id: found,
                    reason,
                    ..
                } => {
                    if found != session_id {
                        return Err(self.fail(RelayError::UnknownRoute));
                    }
                    self.state = RelayState::Closed;
                    Ok(RelayEvent::Closed {
                        session_id: Some(session_id),
                        reason,
                    })
                }
                RelayMessage::Error { code, .. } => Err(self.fail(code.into_error())),
                _ => Err(self.fail(RelayError::UnexpectedMessage)),
            },
            _ => Err(self.fail(RelayError::UnexpectedMessage)),
        }
    }

    /// Wraps one opaque pairing payload for the routed session.
    pub fn payload_frame(&self, payload: &[u8]) -> Result<RelayMessage, RelayError> {
        let RelayState::Routed { session_id } = &self.state else {
            return Err(RelayError::NotRouted);
        };
        Ok(RelayMessage::Frame {
            v: SIGNALLING_VERSION,
            session_id: session_id.clone(),
            frame: OpaqueFrame::new(payload)?,
        })
    }

    /// Closes the routed session. The driver then stops accepting frames.
    pub fn close_frame(&mut self, reason: CloseReason) -> Result<RelayMessage, RelayError> {
        let RelayState::Routed { session_id } = self.state.clone() else {
            return Err(RelayError::NotRouted);
        };
        self.state = RelayState::Closed;
        Ok(RelayMessage::Close {
            v: SIGNALLING_VERSION,
            session_id,
            reason,
        })
    }

    fn fail(&mut self, error: RelayError) -> RelayError {
        self.state = RelayState::Closed;
        error
    }
}

impl fmt::Debug for RelayDriver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RelayDriver")
            .field("role", &self.role)
            .field("host_id", &self.host_id)
            .field("generation", &self.generation)
            .field("state", &self.state)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct StubConnection {
        generation: u64,
    }

    impl StubConnection {
        fn new(generation: u64) -> Self {
            Self { generation }
        }
    }

    impl RelayConnection for StubConnection {
        type Error = RelayError;

        fn generation(&self) -> u64 {
            self.generation
        }

        fn send_frame(&mut self, _frame: &[u8]) -> Result<(), Self::Error> {
            Err(RelayError::Offline)
        }

        fn recv_frame(&mut self, _max_bytes: usize) -> Result<Vec<u8>, Self::Error> {
            Err(RelayError::Offline)
        }
    }

    fn host_id() -> HostId {
        HostId::new("host-1").unwrap()
    }

    fn other_host_id() -> HostId {
        HostId::new("host-2").unwrap()
    }

    fn session() -> SessionId {
        SessionId::parse("sess-0000000000000001").unwrap()
    }

    fn other_session() -> SessionId {
        SessionId::parse("sess-0000000000000002").unwrap()
    }

    fn started_host(generation: u64) -> RelayDriver {
        let mut host = RelayDriver::host(host_id());
        host.begin(&StubConnection::new(generation)).unwrap();
        host.handle(RelayMessage::Registered {
            v: SIGNALLING_VERSION,
            host_id: host_id(),
            generation,
        })
        .unwrap();
        host
    }

    fn routed_host(generation: u64) -> RelayDriver {
        let mut host = started_host(generation);
        host.handle(RelayMessage::Route {
            v: SIGNALLING_VERSION,
            generation,
            session_id: session(),
        })
        .unwrap();
        host
    }

    fn routed_phone(generation: u64) -> RelayDriver {
        let mut phone = RelayDriver::phone(host_id());
        phone.begin(&StubConnection::new(generation)).unwrap();
        phone
            .handle(RelayMessage::Session {
                v: SIGNALLING_VERSION,
                host_id: host_id(),
                generation,
                session_id: session(),
            })
            .unwrap();
        phone
    }

    #[test]
    fn envelope_round_trips_and_is_strictly_versioned() {
        let message = RelayMessage::Register {
            v: SIGNALLING_VERSION,
            host_id: host_id(),
            generation: 3,
        };
        let encoded = message.encode().unwrap();
        assert_eq!(RelayMessage::decode(&encoded).unwrap(), message);

        let mut value: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
        value["extra"] = serde_json::json!(true);
        assert_eq!(
            RelayMessage::decode(value.to_string().as_bytes()),
            Err(RelayError::Malformed)
        );

        let mut value: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
        value["body"]["extra"] = serde_json::json!(true);
        assert_eq!(
            RelayMessage::decode(value.to_string().as_bytes()),
            Err(RelayError::Malformed)
        );

        let mut trailing = encoded.clone();
        trailing.push(b'x');
        assert_eq!(RelayMessage::decode(&trailing), Err(RelayError::Malformed));

        // Adjacent tagging has a second array representation; v1 accepts only
        // the single JSON-object form.
        let array = r#"["register",{"v":1,"host_id":"host-1","generation":3}]"#;
        assert_eq!(
            RelayMessage::decode(array.as_bytes()),
            Err(RelayError::Malformed)
        );

        // Resume is not part of v1. A frame that tries to carry it is an
        // unknown field, not an accepted message the test double must reject.
        let resume = r#"{"type":"register","body":{"v":1,"host_id":"host-1","generation":1,"resume_session":"sess-0000000000000001"}}"#;
        assert_eq!(
            RelayMessage::decode(resume.as_bytes()),
            Err(RelayError::Malformed)
        );

        let mut value: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
        value["body"]["v"] = serde_json::json!(2);
        assert_eq!(
            RelayMessage::decode(value.to_string().as_bytes()),
            Err(RelayError::UnsupportedVersion { found: 2 })
        );
    }

    #[test]
    fn oversized_envelopes_and_payloads_are_rejected() {
        assert_eq!(
            RelayMessage::decode(&vec![b'{'; MAX_SIGNALLING_FRAME_BYTES + 1]),
            Err(RelayError::FrameTooLarge {
                max: MAX_SIGNALLING_FRAME_BYTES
            })
        );
        assert_eq!(
            OpaqueFrame::new(&vec![0_u8; MAX_SIGNALLING_PAYLOAD_BYTES + 1]),
            Err(RelayError::PayloadTooLarge {
                max: MAX_SIGNALLING_PAYLOAD_BYTES
            })
        );

        let raw = serde_json::json!({
            "type": "frame",
            "body": {
                "v": 1,
                "session_id": session().as_str(),
                "frame": URL_SAFE_NO_PAD
                    .encode(vec![0_u8; MAX_SIGNALLING_PAYLOAD_BYTES + 1]),
            }
        });
        assert_eq!(
            RelayMessage::decode(raw.to_string().as_bytes()),
            Err(RelayError::Malformed)
        );

        // The encoded length is checked before base64 decoding.
        let overlong = serde_json::json!({
            "type": "frame",
            "body": {
                "v": 1,
                "session_id": session().as_str(),
                "frame": "A".repeat(MAX_SIGNALLING_PAYLOAD_ENCODED_BYTES + 1),
            }
        });
        assert_eq!(
            RelayMessage::decode(overlong.to_string().as_bytes()),
            Err(RelayError::Malformed)
        );
    }

    #[test]
    fn rejects_noncanonical_payload_and_bad_session_ids() {
        for raw in [
            "short",
            "has spaces in this identifier",
            "sess-0000000000000001!",
        ] {
            assert_eq!(SessionId::parse(raw), Err(RelayError::InvalidSessionId));
        }
        assert!(SessionId::parse("sess-0000000000000001").is_ok());

        let encoded = URL_SAFE_NO_PAD.encode([1_u8; 8]);
        let raw = serde_json::json!({
            "type": "frame",
            "body": {
                "v": 1,
                "session_id": session().as_str(),
                "frame": format!("{encoded}="),
            }
        });
        assert_eq!(
            RelayMessage::decode(raw.to_string().as_bytes()),
            Err(RelayError::Malformed)
        );
    }

    #[test]
    fn host_driver_happy_path_routes_bounded_frames() {
        let mut host = RelayDriver::host(host_id());
        assert_eq!(
            host.begin(&StubConnection::new(1)).unwrap(),
            RelayMessage::Register {
                v: 1,
                host_id: host_id(),
                generation: 1,
            }
        );
        assert_eq!(
            host.handle(RelayMessage::Registered {
                v: 1,
                host_id: host_id(),
                generation: 1,
            })
            .unwrap(),
            RelayEvent::RegisterAccepted { generation: 1 }
        );
        assert_eq!(
            host.handle(RelayMessage::Route {
                v: 1,
                generation: 1,
                session_id: session(),
            })
            .unwrap(),
            RelayEvent::IncomingSession {
                session_id: session()
            }
        );

        let frame = host.payload_frame(b"pairing request").unwrap();
        let encoded = frame.encode().unwrap();
        let decoded = RelayMessage::decode(&encoded).unwrap();
        assert_eq!(
            host.handle(decoded).unwrap(),
            RelayEvent::Frame {
                session_id: session(),
                frame: OpaqueFrame::new(b"pairing request").unwrap(),
            }
        );
    }

    #[test]
    fn driver_fails_closed_on_out_of_order_and_stale_generation() {
        let mut host = RelayDriver::host(host_id());
        assert_eq!(
            host.begin(&StubConnection::new(0)),
            Err(RelayError::Offline)
        );
        assert_eq!(host.state(), &RelayState::Closed);

        let mut host = RelayDriver::host(host_id());
        host.begin(&StubConnection::new(2)).unwrap();
        assert_eq!(
            host.begin(&StubConnection::new(2)),
            Err(RelayError::AlreadyConnected)
        );
        assert_eq!(host.state(), &RelayState::Closed);

        // Lower generations are rejected once a higher one was accepted.
        let mut host = RelayDriver::host(host_id());
        host.begin(&StubConnection::new(2)).unwrap();
        assert_eq!(
            host.begin(&StubConnection::new(1)),
            Err(RelayError::GenerationMismatch {
                expected: 2,
                found: 1
            })
        );

        let mut phone = RelayDriver::phone(host_id());
        phone.begin(&StubConnection::new(4)).unwrap();
        assert_eq!(
            phone.handle(RelayMessage::Session {
                v: 1,
                host_id: host_id(),
                generation: 3,
                session_id: session(),
            }),
            Err(RelayError::GenerationMismatch {
                expected: 4,
                found: 3
            })
        );
        assert_eq!(phone.state(), &RelayState::Closed);
    }

    #[test]
    fn driver_rejects_wrong_host_second_route_wrong_close_and_not_routed() {
        // Wrong Host in the registration reply.
        let mut host = RelayDriver::host(host_id());
        host.begin(&StubConnection::new(1)).unwrap();
        assert_eq!(
            host.handle(RelayMessage::Registered {
                v: 1,
                host_id: other_host_id(),
                generation: 1,
            }),
            Err(RelayError::HostMismatch)
        );
        assert_eq!(host.state(), &RelayState::Closed);

        // A second route while already routed is a protocol error.
        let mut host = routed_host(1);
        assert_eq!(
            host.handle(RelayMessage::Route {
                v: 1,
                generation: 1,
                session_id: other_session(),
            }),
            Err(RelayError::UnexpectedMessage)
        );
        assert_eq!(host.state(), &RelayState::Closed);

        // Close for a different session id never touches the active route.
        let mut host = routed_host(1);
        assert_eq!(
            host.handle(RelayMessage::Close {
                v: 1,
                session_id: other_session(),
                reason: CloseReason::Revoked,
            }),
            Err(RelayError::UnknownRoute)
        );
        assert_eq!(host.state(), &RelayState::Closed);

        // Wrong Host on the phone session reply.
        let mut phone = RelayDriver::phone(host_id());
        phone.begin(&StubConnection::new(1)).unwrap();
        assert_eq!(
            phone.handle(RelayMessage::Session {
                v: 1,
                host_id: other_host_id(),
                generation: 1,
                session_id: session(),
            }),
            Err(RelayError::HostMismatch)
        );
        assert_eq!(phone.state(), &RelayState::Closed);

        // Wrong close id on the phone route.
        let mut phone = routed_phone(1);
        assert_eq!(
            phone.handle(RelayMessage::Close {
                v: 1,
                session_id: other_session(),
                reason: CloseReason::Offline,
            }),
            Err(RelayError::UnknownRoute)
        );
        assert_eq!(phone.state(), &RelayState::Closed);

        // Frames and closes require an active route.
        let mut host = RelayDriver::host(host_id());
        assert_eq!(
            host.payload_frame(b"before route"),
            Err(RelayError::NotRouted)
        );
        assert_eq!(
            host.close_frame(CloseReason::Revoked),
            Err(RelayError::NotRouted)
        );
    }

    #[test]
    fn malformed_frames_close_the_driver_and_relay_refusals_are_typed() {
        let mut host = RelayDriver::host(host_id());
        host.begin(&StubConnection::new(1)).unwrap();
        assert_eq!(host.handle_frame(b"{"), Err(RelayError::Malformed));
        assert_eq!(host.state(), &RelayState::Closed);

        let mut host = RelayDriver::host(host_id());
        host.begin(&StubConnection::new(1)).unwrap();
        assert_eq!(
            host.handle_frame(br#"["registered",{"v":1,"host_id":"host-1","generation":1}]"#),
            Err(RelayError::Malformed)
        );
        assert_eq!(host.state(), &RelayState::Closed);

        let mut host = RelayDriver::host(host_id());
        host.begin(&StubConnection::new(1)).unwrap();
        assert_eq!(
            host.handle(RelayMessage::Error {
                v: 1,
                code: RelayErrorCode::UnsupportedVersion,
            }),
            Err(RelayError::RelayRefused(RelayErrorCode::UnsupportedVersion))
        );
        assert_eq!(host.state(), &RelayState::Closed);
    }
}
