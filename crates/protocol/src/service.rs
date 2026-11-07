//! Protocol service: handshake, command admission and subscription checks.
//!
//! This is the adapter surface a Host transport calls. It owns no sockets and
//! no SQL; it owns the order of security checks:
//!
//! 1. transport binding is internally consistent,
//! 2. protocol generation negotiates,
//! 3. device identity verifies,
//! 4. every command re-checks live grants and leases,
//! 5. only then does idempotency lookup run.
//!
//! Reordering these steps would leak previous results or let revoked devices
//! keep acting on cached permissions.

use crate::authz::{
    authorize_command, AuthenticatedSession, BindingError, Peer, Plane, ReplayPolicy,
    TransportBinding,
};
use crate::cursor::StreamBounds;
use crate::device::{RegistryError, TrustRegistry};
use crate::envelope::{CommandEnvelope, EnvelopeError};
use crate::id::{DeviceId, IdempotencyKey, RequestId};
use crate::idempotency::{
    CommandResult, IdempotencyAdmission, IdempotencyError, IdempotencyStore, RequestFingerprint,
};
use crate::limits::{LimitError, Limits};
use crate::version::{NegotiationError, ServerPolicy};
use crate::wire::{ClientHello, ServerHello};
use std::error::Error;
use std::fmt;

/// Outcome of admitting one command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdmissionDecision {
    /// First attempt; the adapter may execute.
    Execute {
        key: IdempotencyKey,
        replay_policy: ReplayPolicy,
    },
    /// Retry of an earlier attempt. `result` is `None` while the first attempt
    /// is still in flight.
    Duplicate {
        request_id: RequestId,
        result: Option<CommandResult>,
    },
}

/// Typed handshake failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionError {
    Binding(BindingError),
    Negotiation(NegotiationError),
    Denial(crate::authz::Denial),
}

impl fmt::Display for SessionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Binding(error) => error.fmt(formatter),
            Self::Negotiation(error) => error.fmt(formatter),
            Self::Denial(denial) => denial.fmt(formatter),
        }
    }
}

impl Error for SessionError {}

impl From<BindingError> for SessionError {
    fn from(error: BindingError) -> Self {
        Self::Binding(error)
    }
}

impl From<NegotiationError> for SessionError {
    fn from(error: NegotiationError) -> Self {
        Self::Negotiation(error)
    }
}

impl From<crate::authz::Denial> for SessionError {
    fn from(denial: crate::authz::Denial) -> Self {
        Self::Denial(denial)
    }
}

/// Typed command admission failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandError {
    Envelope(EnvelopeError),
    Denial(crate::authz::Denial),
    Idempotency(IdempotencyError),
}

impl fmt::Display for CommandError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Envelope(error) => error.fmt(formatter),
            Self::Denial(denial) => denial.fmt(formatter),
            Self::Idempotency(error) => error.fmt(formatter),
        }
    }
}

impl Error for CommandError {}

impl From<EnvelopeError> for CommandError {
    fn from(error: EnvelopeError) -> Self {
        Self::Envelope(error)
    }
}

impl From<crate::authz::Denial> for CommandError {
    fn from(denial: crate::authz::Denial) -> Self {
        Self::Denial(denial)
    }
}

impl From<IdempotencyError> for CommandError {
    fn from(error: IdempotencyError) -> Self {
        Self::Idempotency(error)
    }
}

/// Protocol authority for one Host.
#[derive(Debug)]
pub struct ProtocolService {
    pub limits: Limits,
    pub policy: ServerPolicy,
    pub trust: TrustRegistry,
    pub idempotency: IdempotencyStore,
    pub leases: crate::authz::LeaseRegistry,
}

impl ProtocolService {
    pub fn new(limits: Limits) -> Result<Self, LimitError> {
        limits.validate()?;
        let idempotency = IdempotencyStore::new(limits).map_err(|error| match error {
            IdempotencyError::Limit(limit) => limit,
            _ => LimitError::InvalidPolicy("idempotency store rejected the limits"),
        })?;
        Ok(Self {
            limits,
            policy: ServerPolicy::default(),
            trust: TrustRegistry::new(),
            idempotency,
            leases: crate::authz::LeaseRegistry::new(),
        })
    }

    /// Authenticates a transport connection and negotiates the generation.
    pub fn accept_hello(
        &self,
        binding: &TransportBinding,
        hello: &ClientHello,
        now_ms: i64,
    ) -> Result<AuthenticatedSession, SessionError> {
        binding.validate()?;
        let negotiated = self.policy.negotiate(&hello.supported_generations)?;
        let peer = match binding.plane {
            Plane::LocalIpc => {
                if hello.device.is_some() {
                    return Err(SessionError::Denial(crate::authz::Denial::PlaneMismatch));
                }
                let uid = binding
                    .verified_local_uid
                    .ok_or(BindingError::LocalPeerMissing)?;
                Peer::LocalOwner { uid }
            }
            Plane::Remote => {
                let claim = hello
                    .device
                    .as_ref()
                    .ok_or(crate::authz::Denial::PlaneMismatch)?;
                Peer::Device(self.trust.authenticate(claim, now_ms)?)
            }
        };
        Ok(AuthenticatedSession {
            request_id: hello.request_id.clone(),
            generation: negotiated.generation,
            plane: binding.plane,
            peer,
        })
    }

    #[must_use]
    pub fn server_hello(
        &self,
        session: &AuthenticatedSession,
        streams: Vec<StreamBounds>,
    ) -> ServerHello {
        ServerHello {
            protocol: session.generation,
            request_id: session.request_id.clone(),
            features: self.policy.features.clone(),
            scopes: session.scopes(),
            streams,
        }
    }

    /// Admits a command after generation, live authorization and idempotency
    /// checks. `Duplicate` must not be executed again.
    pub fn admit_command(
        &mut self,
        session: &AuthenticatedSession,
        envelope: &CommandEnvelope,
        now_ms: i64,
    ) -> Result<AdmissionDecision, CommandError> {
        if envelope.protocol != session.generation {
            return Err(EnvelopeError::GenerationMismatch {
                expected: session.generation,
                actual: envelope.protocol,
            }
            .into());
        }
        envelope.command.validate(&self.limits)?;
        authorize_command(
            session,
            &envelope.command,
            &self.trust,
            &self.leases,
            now_ms,
        )?;
        let fingerprint = RequestFingerprint::of(&envelope.command)?;
        let principal = session.principal();
        match self.idempotency.begin(
            session.plane,
            &principal,
            &envelope.idempotency_key,
            &fingerprint,
            now_ms,
        )? {
            IdempotencyAdmission::Execute => Ok(AdmissionDecision::Execute {
                key: envelope.idempotency_key.clone(),
                replay_policy: envelope.replay_policy(),
            }),
            IdempotencyAdmission::Duplicate { result } => Ok(AdmissionDecision::Duplicate {
                request_id: envelope.request_id.clone(),
                result,
            }),
        }
    }

    /// Records the final classification for a previously admitted command.
    pub fn complete_command(
        &mut self,
        key: &IdempotencyKey,
        result: CommandResult,
        now_ms: i64,
    ) -> Result<(), CommandError> {
        self.idempotency
            .complete(key, result, now_ms)
            .map_err(CommandError::from)
    }

    /// Revokes a device and every controller lease it holds.
    pub fn revoke_device(&mut self, device_id: &DeviceId) -> Result<(), RegistryError> {
        self.trust.revoke(device_id)?;
        self.leases.revoke_device(device_id);
        Ok(())
    }

    /// Re-checks delivery rights against live grants before every outbound
    /// event batch.
    ///
    /// A subscription is not a durable capability. Revocation, key rotation
    /// and grant expiry must terminate an already-open stream, so transports
    /// call this immediately before sending each batch. A denial closes the
    /// subscription; it is not a retryable command error.
    pub fn authorize_delivery(
        &self,
        session: &AuthenticatedSession,
        now_ms: i64,
    ) -> Result<(), crate::authz::Denial> {
        session.authorize_observe(&self.trust, now_ms)
    }

    /// Subscription-request authorization. Same live check as delivery; the
    /// delivery check must still run for every later batch.
    pub fn authorize_subscribe(
        &self,
        session: &AuthenticatedSession,
        now_ms: i64,
    ) -> Result<(), crate::authz::Denial> {
        self.authorize_delivery(session, now_ms)
    }
}
