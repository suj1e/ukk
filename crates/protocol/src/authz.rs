//! Authorization decisions for remote and local peers.
//!
//! Two planes share typed envelopes but not trust:
//!
//! * [`Plane::LocalIpc`] is authenticated by the OS peer identity of the
//!   desktop process; the transport supplies a verified uid.
//! * [`Plane::Remote`] is authenticated by a registered device key and
//!   explicit scoped grants. A relay is never an authority.
//!
//! An envelope carries no plane or device id, so it cannot talk its way into a
//! different trust domain. The plane is fixed by the handshake, and durable
//! idempotency records remember the plane that first used a key.

use crate::device::{AuthenticatedDevice, TrustRegistry};
use crate::envelope::Command;
use crate::id::{DeviceId, DomainId, LeaseId, RequestId};
use crate::limits::Limits;
use crate::scope::{Scope, ScopeSet};
use crate::version::ProtocolGeneration;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};
use std::error::Error;
use std::fmt;

/// Transport trust domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Plane {
    /// Same-machine, user-scoped IPC. Never exposed to a network listener.
    LocalIpc,
    /// Paired-device protocol over relay or direct encrypted transport.
    Remote,
}

/// How a connection was accepted by the transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransportBinding {
    pub plane: Plane,
    /// Set only after the OS verified the local peer's user id.
    pub verified_local_uid: Option<u32>,
}

impl TransportBinding {
    #[must_use]
    pub fn local_ipc(verified_local_uid: u32) -> Self {
        Self {
            plane: Plane::LocalIpc,
            verified_local_uid: Some(verified_local_uid),
        }
    }

    #[must_use]
    pub fn remote() -> Self {
        Self {
            plane: Plane::Remote,
            verified_local_uid: None,
        }
    }

    /// A local binding without a verified uid means "local" was asserted but
    /// never authenticated, which is exactly the escalation this protocol
    /// refuses.
    pub fn validate(&self) -> Result<(), BindingError> {
        match (self.plane, self.verified_local_uid) {
            (Plane::LocalIpc, None) => Err(BindingError::LocalPeerMissing),
            (Plane::Remote, Some(_)) => Err(BindingError::UnexpectedLocalPeer),
            _ => Ok(()),
        }
    }
}

/// Typed transport-binding failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingError {
    LocalPeerMissing,
    UnexpectedLocalPeer,
}

impl fmt::Display for BindingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LocalPeerMissing => {
                formatter.write_str("local IPC binding has no verified peer identity")
            }
            Self::UnexpectedLocalPeer => {
                formatter.write_str("remote binding must not carry local peer identity")
            }
        }
    }
}

impl Error for BindingError {}

/// Typed authorization denial. Denials never reveal prior command results.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum Denial {
    UnknownDevice,
    DeviceRevoked,
    ForgedIdentity,
    StaleIdentity,
    MissingScope { scope: Scope },
    GrantExpired { scope: Scope },
    PlaneMismatch,
    ReplayForbidden,
    ControllerLeaseMissing,
    ControllerLeaseExpired,
    ControllerLeaseMismatch,
}

impl Denial {
    #[must_use]
    pub fn scope(&self) -> Option<Scope> {
        match self {
            Self::MissingScope { scope } | Self::GrantExpired { scope } => Some(*scope),
            _ => None,
        }
    }
}

impl fmt::Display for Denial {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownDevice => formatter.write_str("device is not registered"),
            Self::DeviceRevoked => formatter.write_str("device has been revoked"),
            Self::ForgedIdentity => formatter.write_str("device identity does not match"),
            Self::StaleIdentity => formatter.write_str("device identity generation is stale"),
            Self::MissingScope { scope } => write!(formatter, "missing scope {scope}"),
            Self::GrantExpired { scope } => write!(formatter, "grant for {scope} has expired"),
            Self::PlaneMismatch => formatter.write_str("peer and transport plane do not match"),
            Self::ReplayForbidden => {
                formatter.write_str("command must never be replayed after reconnect")
            }
            Self::ControllerLeaseMissing => {
                formatter.write_str("remote input requires a controller lease")
            }
            Self::ControllerLeaseExpired => formatter.write_str("controller lease has expired"),
            Self::ControllerLeaseMismatch => {
                formatter.write_str("controller lease does not belong to this device or session")
            }
        }
    }
}

impl Error for Denial {}

impl From<BindingError> for Denial {
    fn from(_: BindingError) -> Self {
        Self::PlaneMismatch
    }
}

/// Authenticated peer bound to one connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Peer {
    /// The local OS user that owns the Host process.
    LocalOwner { uid: u32 },
    /// A paired device verified against the trust registry.
    Device(AuthenticatedDevice),
}

/// Authenticated principal used to bind idempotency records.
///
/// Keys are scoped to a principal so one device cannot claim another device's
/// retry record, even if it guesses the key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Principal {
    LocalOwner { uid: u32 },
    Device(DeviceId),
}

/// Result of a successful handshake. Holds no transport object and no
/// ephemeral runtime sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthenticatedSession {
    pub request_id: RequestId,
    pub generation: ProtocolGeneration,
    pub plane: Plane,
    pub peer: Peer,
}

impl AuthenticatedSession {
    #[must_use]
    pub fn device(&self) -> Option<&AuthenticatedDevice> {
        match &self.peer {
            Peer::Device(device) => Some(device),
            Peer::LocalOwner { .. } => None,
        }
    }

    #[must_use]
    pub fn device_id(&self) -> Option<&DeviceId> {
        self.device().map(|device| &device.device_id)
    }

    #[must_use]
    pub fn principal(&self) -> Principal {
        match &self.peer {
            Peer::LocalOwner { uid } => Principal::LocalOwner { uid: *uid },
            Peer::Device(device) => Principal::Device(device.device_id.clone()),
        }
    }

    #[must_use]
    pub fn scopes(&self) -> ScopeSet {
        match &self.peer {
            Peer::Device(device) => device.scopes.clone(),
            Peer::LocalOwner { .. } => Scope::all().collect(),
        }
    }

    /// Stream delivery authorization, evaluated against the live registry.
    ///
    /// This is the only observe check exposed to transports. It must be called
    /// before every outbound event batch, not just when a subscription starts:
    /// revocation, key rotation and grant expiry must stop an already-open
    /// stream. The handshake scope snapshot is never authoritative here.
    pub fn authorize_observe(&self, trust: &TrustRegistry, now_ms: i64) -> Result<(), Denial> {
        match &self.peer {
            Peer::LocalOwner { .. } => {
                if self.plane != Plane::LocalIpc {
                    return Err(Denial::PlaneMismatch);
                }
                Ok(())
            }
            Peer::Device(device) => {
                if self.plane != Plane::Remote {
                    return Err(Denial::PlaneMismatch);
                }
                let (valid, expired) = trust.live_grants(device, now_ms)?;
                if !valid.contains(Scope::SessionObserve) {
                    return Err(if expired.contains(Scope::SessionObserve) {
                        Denial::GrantExpired {
                            scope: Scope::SessionObserve,
                        }
                    } else {
                        Denial::MissingScope {
                            scope: Scope::SessionObserve,
                        }
                    });
                }
                Ok(())
            }
        }
    }
}

/// Maximum lifetime of a remote input lease.
pub const MAX_CONTROLLER_LEASE_MS: i64 = 10 * 60 * 1000;
const MAX_ACTIVE_LEASES: usize = 64;

/// Time-limited remote input authority. Expiry is absolute wall-clock time so
/// a silently disconnected phone still loses control.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControllerLease {
    pub id: LeaseId,
    pub device_id: DeviceId,
    pub session_id: DomainId,
    pub expires_at_ms: i64,
}

impl ControllerLease {
    #[must_use]
    pub fn is_expired_at(&self, now_ms: i64) -> bool {
        now_ms >= self.expires_at_ms
    }
}

/// Typed lease failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaseError {
    InvalidTtl,
    Capacity,
}

impl fmt::Display for LeaseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTtl => formatter.write_str("controller lease lifetime is out of range"),
            Self::Capacity => formatter.write_str("too many active controller leases"),
        }
    }
}

impl Error for LeaseError {}

/// Active controller leases. Issuing a lease requires an explicit local action.
#[derive(Debug, Default)]
pub struct LeaseRegistry {
    leases: BTreeMap<LeaseId, ControllerLease>,
}

impl LeaseRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn issue(
        &mut self,
        device_id: DeviceId,
        session_id: DomainId,
        now_ms: i64,
        ttl_ms: i64,
    ) -> Result<ControllerLease, LeaseError> {
        if ttl_ms <= 0 || ttl_ms > MAX_CONTROLLER_LEASE_MS {
            return Err(LeaseError::InvalidTtl);
        }
        self.purge_expired(now_ms);
        if self.leases.len() >= MAX_ACTIVE_LEASES {
            return Err(LeaseError::Capacity);
        }
        let lease = ControllerLease {
            id: LeaseId::new_random(),
            device_id,
            session_id,
            expires_at_ms: now_ms.saturating_add(ttl_ms),
        };
        self.leases.insert(lease.id.clone(), lease.clone());
        Ok(lease)
    }

    pub fn verify(
        &self,
        lease_id: &LeaseId,
        device_id: &DeviceId,
        session_id: &DomainId,
        now_ms: i64,
    ) -> Result<(), Denial> {
        let lease = self
            .leases
            .get(lease_id)
            .ok_or(Denial::ControllerLeaseMismatch)?;
        if lease.is_expired_at(now_ms) {
            return Err(Denial::ControllerLeaseExpired);
        }
        if &lease.device_id != device_id || &lease.session_id != session_id {
            return Err(Denial::ControllerLeaseMismatch);
        }
        Ok(())
    }

    pub fn revoke(&mut self, lease_id: &LeaseId) -> bool {
        self.leases.remove(lease_id).is_some()
    }

    /// Revokes every lease held by a device. Called on device revocation.
    pub fn revoke_device(&mut self, device_id: &DeviceId) -> usize {
        let before = self.leases.len();
        self.leases.retain(|_, lease| &lease.device_id != device_id);
        before - self.leases.len()
    }

    fn purge_expired(&mut self, now_ms: i64) {
        self.leases.retain(|_, lease| !lease.is_expired_at(now_ms));
    }
}

/// Semantics of a command for retry and reconnect handling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandClass {
    /// Safe to retry with the same idempotency key.
    IdempotentMutation,
    /// A human decision. Retrying must never approve twice.
    HumanDecision,
    /// Live pointer or keyboard input. Meaningless after reconnect.
    EphemeralInput,
}

/// Reconnect replay policy derived from the command class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayPolicy {
    ReplayWithIdempotency,
    NeverReplay,
}

impl ReplayPolicy {
    #[must_use]
    pub fn is_replayable(self) -> bool {
        matches!(self, Self::ReplayWithIdempotency)
    }
}

impl CommandClass {
    #[must_use]
    pub fn replay_policy(self) -> ReplayPolicy {
        match self {
            Self::IdempotentMutation => ReplayPolicy::ReplayWithIdempotency,
            Self::HumanDecision | Self::EphemeralInput => ReplayPolicy::NeverReplay,
        }
    }
}

/// Checks a single command against the live trust registry and leases.
///
/// Authorization runs before idempotency lookup so an unauthorized retry can
/// never learn a previous command result.
pub fn authorize_command(
    session: &AuthenticatedSession,
    command: &Command,
    trust: &TrustRegistry,
    leases: &LeaseRegistry,
    now_ms: i64,
) -> Result<(), Denial> {
    let device = match &session.peer {
        Peer::LocalOwner { .. } => {
            if session.plane != Plane::LocalIpc {
                return Err(Denial::PlaneMismatch);
            }
            return Ok(());
        }
        Peer::Device(device) => device,
    };
    if session.plane != Plane::Remote {
        return Err(Denial::PlaneMismatch);
    }
    let scope = command.required_scope();
    let (valid, expired) = trust.live_grants(device, now_ms)?;
    if !valid.contains(scope) {
        return Err(if expired.contains(scope) {
            Denial::GrantExpired { scope }
        } else {
            Denial::MissingScope { scope }
        });
    }
    if let Command::PointerInput(input) = command {
        let lease_id = input
            .lease_id
            .as_ref()
            .ok_or(Denial::ControllerLeaseMissing)?;
        leases.verify(lease_id, &device.device_id, &input.session_id, now_ms)?;
    }
    Ok(())
}

/// Typed reconnect-queue failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueError {
    ReplayForbidden,
    /// The command is older than the idempotency retention window, or claims a
    /// timestamp far in the future. Replaying it could execute after the Host
    /// has expired the corresponding retry record.
    Stale,
    Full {
        max: usize,
    },
}

impl fmt::Display for QueueError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReplayForbidden => {
                formatter.write_str("command class is forbidden from reconnect replay")
            }
            Self::Stale => formatter.write_str("queued command is outside the replay window"),
            Self::Full { max } => write!(formatter, "reconnect queue is full ({max})"),
        }
    }
}

impl Error for QueueError {}

/// Bounded outbound queue used only for replay-safe commands.
///
/// Approvals and pointer input can never enter this queue, so reconnecting
/// cannot silently approve an action or inject input the user never repeated.
#[derive(Debug)]
pub struct ReplayQueue {
    limits: Limits,
    pending: VecDeque<crate::envelope::CommandEnvelope>,
}

impl ReplayQueue {
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            pending: VecDeque::new(),
        }
    }

    pub fn enqueue(
        &mut self,
        envelope: crate::envelope::CommandEnvelope,
        now_ms: i64,
    ) -> Result<(), QueueError> {
        if !envelope.replay_policy().is_replayable() {
            return Err(QueueError::ReplayForbidden);
        }
        if !Self::is_fresh(
            envelope.issued_at_ms,
            now_ms,
            self.limits.idempotency_retention_ms,
        ) {
            return Err(QueueError::Stale);
        }
        if self.pending.len() >= self.limits.max_queue_depth {
            return Err(QueueError::Full {
                max: self.limits.max_queue_depth,
            });
        }
        self.pending.push_back(envelope);
        Ok(())
    }

    /// Removes up to `max` replay-safe commands in original order.
    ///
    /// Entries that fell outside the idempotency retention window are dropped
    /// rather than replayed: the Host would refuse the expired key, and
    /// executing it would reintroduce duplicate side effects.
    pub fn drain(&mut self, max: usize, now_ms: i64) -> Vec<crate::envelope::CommandEnvelope> {
        let retention = self.limits.idempotency_retention_ms;
        self.pending
            .retain(|envelope| Self::is_fresh(envelope.issued_at_ms, now_ms, retention));
        let count = max.min(self.pending.len());
        self.pending.drain(..count).collect()
    }

    /// A command is replayable only inside the window during which its
    /// idempotency record still produces a duplicate classification. Far-future
    /// timestamps are rejected rather than trusted as "fresh forever".
    fn is_fresh(issued_at_ms: i64, now_ms: i64, retention_ms: i64) -> bool {
        let age = now_ms.saturating_sub(issued_at_ms);
        let future = issued_at_ms.saturating_sub(now_ms);
        age < retention_ms && future < retention_ms
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.pending.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }
}
