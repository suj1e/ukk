//! Versioned remote Host protocol.
//!
//! This crate is the contract between the ukk Agent Host and remote clients
//! (iOS today, other native clients later). It is deliberately transport
//! agnostic and contains no I/O:
//!
//! * [`wire`] holds the framed message shapes and the handshake payloads.
//! * [`envelope`] holds typed commands and durable events.
//! * [`cursor`] owns durable per-stream cursors, snapshots, replay and
//!   client-side deduplication.
//! * [`authz`] and [`device`] own trust: planes, device keys, scoped grants,
//!   controller leases and replay policy.
//! * [`idempotency`] classifies command results so retries never execute twice.
//! * [`limits`] bounds every payload, queue and retained history.
//!
//! Local IPC is a distinct trust plane. It is not a loopback instance of the
//! remote protocol, and a remote device claim is rejected on a local binding.

pub mod authz;
pub mod cursor;
pub mod device;
pub mod envelope;
pub mod id;
pub mod idempotency;
pub mod limits;
pub mod reserved;
pub mod scope;
pub mod service;
pub mod version;
pub mod wire;

pub use authz::{
    authorize_command, AuthenticatedSession, BindingError, CommandClass, ControllerLease, Denial,
    LeaseError, LeaseRegistry, Peer, Plane, QueueError, ReplayPolicy, ReplayQueue,
    TransportBinding, MAX_CONTROLLER_LEASE_MS,
};
pub use cursor::{
    ApplyOutcome, CursorCheckpoint, CursorError, DurableCursor, EventInbox, EventLog, InboxError,
    Resume, StreamBounds, SyncError,
};
pub use device::{
    AuthenticatedDevice, DeviceClaim, DeviceStatus, IdentityGeneration, RegistryError, ScopedGrant,
    TrustRegistry,
};
pub use envelope::{
    AgentRunStatusChanged, AgentSend, AgentStatus, ApprovalDecision, ApprovalRequested,
    ApprovalResolved, ApprovalRespond, Command, CommandEnvelope, DeviceRevoke, EnvelopeError,
    Event, EventEnvelope, PointerButton, PointerEvent, PointerInput, SessionArchived,
    SessionCreate, SessionCreated, Snapshot,
};
pub use id::{
    DeviceId, DomainId, EventId, IdError, IdempotencyKey, KeyFingerprint, LeaseId, RequestId,
    StreamId, MAX_ID_BYTES,
};
pub use idempotency::{
    CommandResult, CommandResultEnvelope, IdempotencyAdmission, IdempotencyError, IdempotencyStore,
    RequestFingerprint,
};
pub use limits::{LimitError, Limits};
pub use scope::{scopes, Scope, ScopeSet};
pub use service::{AdmissionDecision, CommandError, ProtocolService, SessionError};
pub use version::{
    Features, Negotiated, NegotiationError, ProtocolGeneration, ProtocolRange, ServerPolicy,
    CURRENT_GENERATION, MIN_SUPPORTED_GENERATION,
};
pub use wire::{
    AckRequest, ClientHello, ClientMessage, ServerError, ServerHello, ServerMessage,
    SubscribeRequest, MAX_ERROR_MESSAGE_BYTES,
};
