//! Host-owned adapter that binds the merged `ukk_protocol::ProtocolService` to
//! the durable SQLite state in `crates/store`.
//!
//! The protocol crate owns authorization, cursor, replay and idempotency
//! *semantics*. This adapter owns the state that must survive a Host restart:
//!
//! * event log and per-stream cursors (`Store::append_event`/`read_events`),
//! * device identity and scoped grants (`Store::load_trust_registry`),
//! * idempotency records (`Store::begin_idempotent_command`/finish).
//!
//! Admission keeps the mandatory check order from
//! `ukk_protocol::ProtocolService::admit_command`: generation, semantic
//! validation, live authorization/lease, then idempotency. The in-memory
//! `IdempotencyStore` inside [`ukk_protocol::ProtocolService`] is deliberately
//! bypassed: SQLite is the single durable authority, so a retry after a Host
//! restart is classified by the same record that admitted the first attempt.
//! Maintaining both stores would create two sources of truth.
//!
//! There is no production pairing UI here. [`ProtocolHost::register_test_device`]
//! and [`ProtocolHost::revoke_test_device`] exist so transport tests can
//! exercise real trust rows; the S2 pairing flow is the only future caller of
//! the same store APIs.

use std::fmt;
use std::sync::{Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use ukk_protocol::{
    authorize_command, AgentRunStatusChanged, AgentStatus, AuthenticatedSession, ClientHello,
    Command, CommandEnvelope, CommandError, CommandResult, CommandResultEnvelope, Denial, DomainId,
    DurableCursor, EnvelopeError, Event, EventEnvelope, EventId, IdError, IdempotencyAdmission,
    IdempotencyError, LimitError, Limits, Plane, ProtocolService, RequestFingerprint, RequestId,
    ServerHello, SessionCreated, SessionError, Snapshot, StreamBounds, StreamId, SubscribeRequest,
    SyncError, TransportBinding,
};
use ukk_store::{Store, StoreError};

/// Durable stream served by the local protocol endpoint.
///
/// Terminal byte streams and screen media never enter this stream; herdr-core
/// `hc_seq` and Host `host_epoch` are process-local and are never cursors.
pub(crate) const WORKSPACE_STREAM: &str = "workspace";

/// Wall-clock milliseconds for persisted timestamps and grant expiry.
pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

/// Typed failure of the Host protocol adapter.
#[derive(Debug)]
pub(crate) enum ProtocolHostError {
    Store(StoreError),
    Sync(SyncError),
    Envelope(EnvelopeError),
    Limit(LimitError),
    Id(IdError),
    Json(serde_json::Error),
    Session(SessionError),
    Command(CommandError),
    Denial(Denial),
    Idempotency(IdempotencyError),
    Unsupported(&'static str),
}

impl ProtocolHostError {
    /// Denial carried by this failure, when it is an authorization decision.
    /// Transports put this on the wire as a typed reason; other failures are
    /// diagnostics only.
    pub(crate) fn denial(&self) -> Option<&Denial> {
        match self {
            Self::Denial(denial) => Some(denial),
            Self::Session(SessionError::Denial(denial)) => Some(denial),
            Self::Command(CommandError::Denial(denial)) => Some(denial),
            _ => None,
        }
    }
}

impl fmt::Display for ProtocolHostError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Store(error) => error.fmt(formatter),
            Self::Sync(error) => error.fmt(formatter),
            Self::Envelope(error) => error.fmt(formatter),
            Self::Limit(error) => error.fmt(formatter),
            Self::Id(error) => error.fmt(formatter),
            Self::Json(error) => write!(formatter, "protocol JSON: {error}"),
            Self::Session(error) => error.fmt(formatter),
            Self::Command(error) => error.fmt(formatter),
            Self::Denial(error) => error.fmt(formatter),
            Self::Idempotency(error) => error.fmt(formatter),
            Self::Unsupported(reason) => formatter.write_str(reason),
        }
    }
}

impl std::error::Error for ProtocolHostError {}

impl From<StoreError> for ProtocolHostError {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::Protocol(sync) => Self::Sync(sync),
            other => Self::Store(other),
        }
    }
}

impl From<SyncError> for ProtocolHostError {
    fn from(error: SyncError) -> Self {
        Self::Sync(error)
    }
}

impl From<EnvelopeError> for ProtocolHostError {
    fn from(error: EnvelopeError) -> Self {
        Self::Envelope(error)
    }
}

impl From<LimitError> for ProtocolHostError {
    fn from(error: LimitError) -> Self {
        Self::Limit(error)
    }
}

impl From<IdError> for ProtocolHostError {
    fn from(error: IdError) -> Self {
        Self::Id(error)
    }
}

impl From<serde_json::Error> for ProtocolHostError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

impl From<SessionError> for ProtocolHostError {
    fn from(error: SessionError) -> Self {
        Self::Session(error)
    }
}

impl From<IdempotencyError> for ProtocolHostError {
    fn from(error: IdempotencyError) -> Self {
        Self::Idempotency(error)
    }
}

impl From<CommandError> for ProtocolHostError {
    fn from(error: CommandError) -> Self {
        Self::Command(error)
    }
}

impl From<Denial> for ProtocolHostError {
    fn from(denial: Denial) -> Self {
        Self::Denial(denial)
    }
}

/// Outcome of admitting and executing one command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CommandOutcome {
    /// Final classification, including an exact retry of an earlier attempt.
    Completed(CommandResultEnvelope),
    /// The first attempt is still in flight. The client must not assume it
    /// failed, and the transport must not execute anything.
    InFlight { request_id: RequestId },
}

/// Result of one replay or live delivery step.
#[derive(Debug)]
pub(crate) enum Delivery {
    /// The durable head has not advanced past the client cursor.
    Idle,
    /// Ordered events. `next` is the cursor the client has been brought to.
    Events {
        events: Vec<EventEnvelope>,
        next: DurableCursor,
    },
    /// History was pruned. The client applies this snapshot and resumes at
    /// `next`; the cursor binding is owned by the event log, not the transport.
    Snapshot {
        snapshot: Snapshot,
        next: DurableCursor,
    },
}

/// The Host's sole in-process owner of durable protocol state.
pub(crate) struct ProtocolHost {
    inner: Mutex<Inner>,
}

struct Inner {
    store: Store,
    service: ProtocolService,
    stream: StreamId,
    head: DurableCursor,
    floor: DurableCursor,
}

impl ProtocolHost {
    /// Hydrates the protocol service from an already opened store.
    pub(crate) fn open(mut store: Store) -> Result<Self, ProtocolHostError> {
        let limits = *store.limits();
        let mut service = ProtocolService::new(limits)?;
        service.trust = store.load_trust_registry()?;
        let stream = StreamId::new(WORKSPACE_STREAM).expect("static stream id is valid");
        let bounds = match store.event_stream_bounds(&stream)? {
            Some(bounds) => bounds,
            None => store.ensure_event_stream(&stream)?,
        };
        Ok(Self {
            inner: Mutex::new(Inner {
                store,
                service,
                stream,
                head: bounds.head,
                floor: bounds.floor,
            }),
        })
    }

    /// Opens the default local database and hydrates from it.
    pub(crate) fn open_default() -> Result<Self, ProtocolHostError> {
        Self::open(Store::open_default()?)
    }

    #[must_use]
    pub(crate) fn limits(&self) -> Limits {
        // Limits are immutable for a Host lifetime; the store and protocol
        // service were constructed from the same value.
        let inner = self.lock();
        inner.service.limits
    }

    /// Authenticates the transport and negotiates the generation.
    ///
    /// The stream bounds advertised here come from the durable log, so a
    /// reconnecting client can replay or be told a snapshot is required.
    pub(crate) fn accept_hello(
        &self,
        binding: &TransportBinding,
        hello: &ClientHello,
        now_ms: i64,
    ) -> Result<(AuthenticatedSession, ServerHello), ProtocolHostError> {
        let mut inner = self.lock();
        // A handshake must also observe an externally committed revocation or
        // rotation. Never trust a registry snapshot from Host startup.
        inner.reload_trust()?;
        let session = inner.service.accept_hello(binding, hello, now_ms)?;
        let hello = inner.service.server_hello(
            &session,
            vec![StreamBounds {
                stream: inner.stream.clone(),
                floor: inner.floor,
                head: inner.head,
            }],
        );
        Ok((session, hello))
    }

    /// Authorizes a subscription and returns the first ordered batch (or the
    /// snapshot that replaces pruned history).
    pub(crate) fn subscribe(
        &self,
        session: &AuthenticatedSession,
        request: &SubscribeRequest,
        now_ms: i64,
    ) -> Result<Delivery, ProtocolHostError> {
        if request.stream.as_str() != WORKSPACE_STREAM {
            return Err(ProtocolHostError::Unsupported(
                "only the workspace stream is served",
            ));
        }
        let mut inner = self.lock();
        inner.reload_trust()?;
        inner.service.authorize_subscribe(session, now_ms)?;
        let batch = limited_batch(request.max_batch, &inner.service.limits);
        inner.resume(request.cursor, batch)
    }

    /// Re-checks live authorization and returns the next live batch.
    ///
    /// `authorize_delivery` runs on every outbound batch. A denial is returned
    /// to the transport, which must close the open subscription: cancellation
    /// of a device grant or key rotation must stop an already-running stream.
    pub(crate) fn deliver(
        &self,
        session: &AuthenticatedSession,
        cursor: DurableCursor,
        max_batch: usize,
        now_ms: i64,
    ) -> Result<Delivery, ProtocolHostError> {
        let mut inner = self.lock();
        // Re-read the durable registry before every batch: an external
        // writer's revocation, rotation or grant change must stop delivery.
        inner.reload_trust()?;
        inner.service.authorize_delivery(session, now_ms)?;
        if cursor >= inner.head {
            return Ok(Delivery::Idle);
        }
        let batch = limited_batch(max_batch, &inner.service.limits);
        inner.resume(cursor, batch)
    }

    /// Admits and executes one command with durable idempotency.
    pub(crate) fn command(
        &self,
        session: &AuthenticatedSession,
        envelope: &CommandEnvelope,
        now_ms: i64,
    ) -> Result<CommandOutcome, ProtocolHostError> {
        let mut inner = self.lock();
        if envelope.protocol != session.generation {
            return Err(EnvelopeError::GenerationMismatch {
                expected: session.generation,
                actual: envelope.protocol,
            }
            .into());
        }
        if session.plane == Plane::LocalIpc {
            // The merged local transport has no capability field. The existing
            // JSON-RPC mutations require the rotated per-Host LocalAuth token,
            // so this socket must not expose a same-user mutation path that is
            // stronger than the RPC path. Local mutations stay on RPC until a
            // protocol-level local capability is designed.
            return Err(ProtocolHostError::Unsupported(
                "local protocol commands are disabled; local mutations use the \
                 rotated workspace capability on the JSON-RPC endpoint",
            ));
        }
        envelope.command.validate(&inner.service.limits)?;
        inner.reload_trust()?;
        authorize_command(
            session,
            &envelope.command,
            &inner.service.trust,
            &inner.service.leases,
            now_ms,
        )?;
        // Reject clearly unexecutable input before admission. An in-flight
        // record must not be left behind for input this Host can never
        // execute; such a record never expires by design.
        if let Command::SessionCreate(create) = &envelope.command {
            if uuid::Uuid::parse_str(envelope.request_id.as_str()).is_err() {
                return Err(
                    EnvelopeError::Invalid("session_create request id must be a UUID").into(),
                );
            }
            if !inner.store.space_exists(create.space_id.as_str())? {
                return Err(StoreError::NotFound(format!("space {}", create.space_id)).into());
            }
        }
        if let Command::DeviceRevoke(revoke) = &envelope.command {
            if inner.store.device_generation(&revoke.device_id)?.is_none() {
                return Err(StoreError::NotFound(format!("device {}", revoke.device_id)).into());
            }
        }
        if !is_executable(&envelope.command) {
            // Refuse before admission: recording an in-flight record for a
            // command this Host cannot execute would leave a permanent
            // duplicate that never completes.
            return Err(ProtocolHostError::Unsupported(
                "command is not executable on the local protocol endpoint",
            ));
        }
        // Same ordering as `ProtocolService::admit_command`: generation,
        // semantic validation, live authorization/lease, then idempotency.
        // The durable store replaces the protocol service's in-memory
        // `IdempotencyStore` so a retry after a Host restart sees the same
        // record; there is no second authority.
        let fingerprint = RequestFingerprint::of(&envelope.command)?;
        let principal = session.principal();
        match inner.store.begin_idempotent_command(
            session.plane,
            &principal,
            &envelope.idempotency_key,
            &fingerprint,
            now_ms,
        )? {
            IdempotencyAdmission::Execute => {
                let result = match inner.execute(envelope, now_ms) {
                    Ok(result) => result,
                    Err(error) => {
                        // Execution failed before a classification. An
                        // in-flight record never expires by design, so it must
                        // be aborted instead of burning capacity forever.
                        if let Err(abort) = inner
                            .store
                            .abort_idempotent_command(&envelope.idempotency_key)
                        {
                            eprintln!(
                                "ukk protocol service could not abort an in-flight command: \
                                 {abort}"
                            );
                        }
                        return Err(error);
                    }
                };
                inner.store.finish_idempotent_command(
                    &envelope.idempotency_key,
                    result.clone(),
                    now_ms,
                )?;
                Ok(CommandOutcome::Completed(CommandResultEnvelope {
                    request_id: envelope.request_id.clone(),
                    replayed: false,
                    result,
                }))
            }
            IdempotencyAdmission::Duplicate {
                result: Some(result),
            } => Ok(CommandOutcome::Completed(CommandResultEnvelope {
                request_id: envelope.request_id.clone(),
                replayed: true,
                result,
            })),
            IdempotencyAdmission::Duplicate { result: None } => Ok(CommandOutcome::InFlight {
                request_id: envelope.request_id.clone(),
            }),
        }
    }

    /// Publishes one structured herdr-core status change as a durable typed
    /// event, but only when the terminal is bound to a persisted AgentRun.
    ///
    /// An unbound terminal is not silently assigned an Agent identity, and
    /// `hc_seq`/`host_epoch` are never used as cursors. The store assigns the
    /// durable cursor and fans the event out to open subscriptions.
    pub(crate) fn publish_broker_status(
        &self,
        core_id: Option<&str>,
        terminal_address: &str,
        status: AgentStatus,
        now_ms: i64,
    ) -> Result<bool, ProtocolHostError> {
        let mut inner = self.lock();
        let Some((session_id, agent_run_id)) = inner
            .store
            .agent_run_for_terminal(core_id, terminal_address)?
        else {
            return Ok(false);
        };
        let revision = u64::try_from(inner.store.workspace_revision()?).unwrap_or(0);
        inner.append_event(
            Event::AgentRunStatusChanged(AgentRunStatusChanged {
                session_id: DomainId::new(session_id)?,
                agent_run_id: DomainId::new(agent_run_id)?,
                status,
                revision,
            }),
            now_ms,
        )?;
        Ok(true)
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // A poisoned lock means another connection panicked while holding the
        // store. Keeping the durable state usable is safer than aborting every
        // other local client.
        self.inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
}

impl Inner {
    fn resume(
        &mut self,
        cursor: DurableCursor,
        max_batch: usize,
    ) -> Result<Delivery, ProtocolHostError> {
        match self.store.read_events(&self.stream, cursor, max_batch)? {
            ukk_protocol::Resume::Events { events, head, .. } => {
                self.head = head;
                let next = events.last().map_or(cursor, |event| event.cursor);
                Ok(Delivery::Events { events, next })
            }
            ukk_protocol::Resume::SnapshotRequired { at_cursor, bounds } => {
                self.head = bounds.head;
                self.floor = bounds.floor;
                let snapshot = self.snapshot_at_head()?;
                // The store binds a snapshot to the current head; the protocol
                // outcome's `at_cursor` is the same value.
                debug_assert_eq!(snapshot.at_cursor, at_cursor);
                Ok(Delivery::Snapshot {
                    snapshot,
                    next: at_cursor,
                })
            }
        }
    }

    fn snapshot_at_head(&self) -> Result<Snapshot, ProtocolHostError> {
        let revision = u64::try_from(self.store.workspace_revision()?).unwrap_or(0);
        let body = serde_json::to_value(self.store.snapshot()?)?;
        Ok(self.store.snapshot_at_head(&self.stream, revision, body)?)
    }

    fn execute(
        &mut self,
        envelope: &CommandEnvelope,
        now_ms: i64,
    ) -> Result<CommandResult, ProtocolHostError> {
        match &envelope.command {
            Command::SessionCreate(create) => {
                // The request id is the client-chosen idempotent Session id,
                // matching the existing workspace.session.create RPC.
                let session = self.store.create_session_with_id(
                    create.space_id.as_str(),
                    &create.title,
                    envelope.request_id.as_str(),
                )?;
                let revision = u64::try_from(self.store.workspace_revision()?).unwrap_or(0);
                self.append_event(
                    Event::SessionCreated(SessionCreated {
                        space_id: create.space_id.clone(),
                        session_id: ukk_protocol::DomainId::new(session.id.clone())?,
                        title: create.title.clone(),
                        revision,
                    }),
                    now_ms,
                )?;
                Ok(CommandResult::Completed {
                    detail: Some(session.id),
                })
            }
            Command::DeviceRevoke(revoke) => {
                let generation = self
                    .store
                    .device_generation(&revoke.device_id)?
                    .ok_or_else(|| StoreError::NotFound(format!("device {}", revoke.device_id)))?;
                self.store
                    .revoke_device(&revoke.device_id, generation, now_ms)?;
                self.reload_trust()?;
                self.service.leases.revoke_device(&revoke.device_id);
                Ok(CommandResult::Completed {
                    detail: Some(revoke.device_id.to_string()),
                })
            }
            _ => Err(ProtocolHostError::Unsupported(
                "command is not executable on the local protocol endpoint",
            )),
        }
    }

    /// Appends through the durable log so the cursor is assigned inside one
    /// SQLite transaction, then advances the cached head.
    fn append_event(
        &mut self,
        event: Event,
        now_ms: i64,
    ) -> Result<EventEnvelope, ProtocolHostError> {
        let envelope =
            self.store
                .append_event(&self.stream, EventId::new_random(), event, now_ms)?;
        self.head = envelope.cursor;
        Ok(envelope)
    }

    fn reload_trust(&mut self) -> Result<(), ProtocolHostError> {
        self.service.trust = self.store.load_trust_registry()?;
        Ok(())
    }
}

/// Commands the local protocol endpoint can execute end to end.
///
/// `agent_send`, `approval_respond` and `pointer_input` belong to the runtime
/// control capability and controller-lease surfaces, which are separate from
/// this slice. Refusing them is explicit; they are never reported as accepted.
fn is_executable(command: &Command) -> bool {
    matches!(
        command,
        Command::SessionCreate(_) | Command::DeviceRevoke(_)
    )
}

fn limited_batch(requested: usize, limits: &Limits) -> usize {
    requested.clamp(1, limits.max_replay_batch)
}

/// Test-only hooks that exercise real durable rows.
///
/// There is no production pairing surface in this slice: S2 pairing will call
/// the same `Store` APIs for a real device, and tests use these helpers to
/// register or revoke a device through the same path.
#[cfg(test)]
impl ProtocolHost {
    pub(crate) fn register_test_device(
        &self,
        device: &str,
        fingerprint: &str,
        scopes: &[ukk_protocol::Scope],
        expires_at_ms: i64,
    ) -> Result<ukk_protocol::DeviceId, ProtocolHostError> {
        use ukk_protocol::{DeviceId, IdentityGeneration, KeyFingerprint};

        let device_id = DeviceId::new(device)?;
        let fingerprint = KeyFingerprint::new(fingerprint)?;
        let mut inner = self.lock();
        let now = now_ms();
        inner.store.register_device(&device_id, &fingerprint, now)?;
        for scope in scopes {
            inner.store.grant_device_scope(
                &device_id,
                *scope,
                IdentityGeneration::FIRST,
                expires_at_ms,
                now,
            )?;
        }
        inner.reload_trust()?;
        Ok(device_id)
    }

    pub(crate) fn revoke_test_device(&self, device: &str) -> Result<(), ProtocolHostError> {
        use ukk_protocol::DeviceId;

        let device_id = DeviceId::new(device)?;
        let mut inner = self.lock();
        let now = now_ms();
        let generation = inner
            .store
            .device_generation(&device_id)?
            .ok_or_else(|| StoreError::NotFound(format!("device {device}")))?;
        inner.store.revoke_device(&device_id, generation, now)?;
        inner.reload_trust()?;
        inner.service.leases.revoke_device(&device_id);
        Ok(())
    }

    pub(crate) fn create_test_space(&self, title: &str) -> Result<String, ProtocolHostError> {
        Ok(self.lock().store.create_space(title, "/src/ukk")?.id)
    }

    pub(crate) fn idempotency_count(&self) -> usize {
        self.lock()
            .store
            .idempotency_record_count()
            .unwrap_or_default()
    }

    /// Revokes through the store only, without updating the adapter's cached
    /// registry. Used to prove that authorization reloads durable state on the
    /// next handshake, command or delivery batch.
    pub(crate) fn revoke_device_in_store_only(
        &self,
        device: &str,
    ) -> Result<(), ProtocolHostError> {
        use ukk_protocol::DeviceId;

        let device_id = DeviceId::new(device)?;
        let mut inner = self.lock();
        let now = now_ms();
        let generation = inner
            .store
            .device_generation(&device_id)?
            .ok_or_else(|| StoreError::NotFound(format!("device {device}")))?;
        inner.store.revoke_device(&device_id, generation, now)?;
        Ok(())
    }

    pub(crate) fn session_count(&self) -> usize {
        self.lock()
            .store
            .snapshot()
            .map(|snapshot| snapshot.sessions.len())
            .unwrap_or(0)
    }

    /// Test-only retention advance, mirroring a Host policy that pruned old
    /// history. Production pruning happens inside `Store::append_event`.
    pub(crate) fn prune_test_events(&self, through: u64) -> Result<(), ProtocolHostError> {
        let mut inner = self.lock();
        let stream = inner.stream.clone();
        let bounds = inner
            .store
            .prune_events_through(&stream, DurableCursor::from_raw(through))?;
        inner.head = bounds.head;
        inner.floor = bounds.floor;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ukk_domain::new_id;
    use ukk_protocol::{
        ClientHello, DeviceClaim, DeviceId, IdempotencyKey, IdentityGeneration, KeyFingerprint,
        ProtocolGeneration, Scope, SessionCreate as WireSessionCreate, CURRENT_GENERATION,
    };

    fn temporary_database() -> (std::path::PathBuf, std::path::PathBuf) {
        let directory = std::env::temp_dir().join(format!(
            "ukk-protocol-service-{}-{}",
            std::process::id(),
            new_id()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        (directory.clone(), directory.join("workspace.sqlite3"))
    }

    fn session_create_envelope(space_id: &str, request_id: &str, key: &str) -> CommandEnvelope {
        CommandEnvelope {
            protocol: CURRENT_GENERATION,
            request_id: RequestId::new(request_id).unwrap(),
            idempotency_key: IdempotencyKey::new(key).unwrap(),
            issued_at_ms: now_ms(),
            command: Command::SessionCreate(WireSessionCreate {
                space_id: ukk_protocol::DomainId::new(space_id).unwrap(),
                title: "Host protocol Session".into(),
                expected_revision: None,
            }),
        }
    }

    #[test]
    fn hydrates_device_trust_and_grants_from_sqlite() {
        let (directory, path) = temporary_database();
        let device = DeviceId::new("test-device-1").unwrap();
        let fingerprint = KeyFingerprint::new("sha256:test-fingerprint").unwrap();
        {
            let mut store = Store::open(&path).unwrap();
            store
                .register_device(&device, &fingerprint, now_ms())
                .unwrap();
            store
                .grant_device_scope(
                    &device,
                    Scope::SessionObserve,
                    IdentityGeneration::FIRST,
                    now_ms() + 60_000,
                    now_ms(),
                )
                .unwrap();
        }

        let host = ProtocolHost::open(Store::open(&path).unwrap()).unwrap();
        let claim = DeviceClaim {
            device_id: device.clone(),
            key_fingerprint: fingerprint,
            generation: IdentityGeneration::FIRST,
        };
        let hello = ClientHello::remote(RequestId::new_random(), vec![CURRENT_GENERATION], claim);
        let (session, server_hello) = host
            .accept_hello(&TransportBinding::remote(), &hello, now_ms())
            .unwrap();
        assert!(server_hello.scopes.contains(Scope::SessionObserve));
        assert_eq!(session.device_id(), Some(&device));

        std::fs::remove_dir_all(directory).ok();
    }

    fn remote_session(host: &ProtocolHost, scopes: &[Scope]) -> AuthenticatedSession {
        let device = new_id();
        let fingerprint = format!("sha256:{device}");
        host.register_test_device(&device, &fingerprint, scopes, now_ms() + 60_000)
            .unwrap();
        let hello = ClientHello::remote(
            RequestId::new_random(),
            vec![CURRENT_GENERATION],
            DeviceClaim {
                device_id: DeviceId::new(device).unwrap(),
                key_fingerprint: KeyFingerprint::new(fingerprint).unwrap(),
                generation: IdentityGeneration::FIRST,
            },
        );
        host.accept_hello(&TransportBinding::remote(), &hello, now_ms())
            .unwrap()
            .0
    }

    #[test]
    fn remote_command_is_admitted_once_and_replayed_from_the_durable_record() {
        let (directory, path) = temporary_database();
        let store = Store::open(&path).unwrap();
        let host = ProtocolHost::open(store).unwrap();
        let space_id = host
            .lock()
            .store
            .create_space("ukk", "/src/ukk")
            .unwrap()
            .id;
        let session = remote_session(&host, &[Scope::SessionManage]);
        let request_id = new_id();
        let envelope = session_create_envelope(&space_id, &request_id, "retry-key-1");

        match host.command(&session, &envelope, now_ms()).unwrap() {
            CommandOutcome::Completed(result) => {
                assert!(!result.replayed);
                assert_eq!(
                    result.result,
                    CommandResult::Completed {
                        detail: Some(request_id.clone())
                    }
                );
            }
            other => panic!("first attempt must execute: {other:?}"),
        }
        match host.command(&session, &envelope, now_ms()).unwrap() {
            CommandOutcome::Completed(result) => {
                assert!(result.replayed, "retry must be classified as a replay");
                assert_eq!(
                    result.result,
                    CommandResult::Completed {
                        detail: Some(request_id)
                    }
                );
            }
            other => panic!("retry must not execute again: {other:?}"),
        }
        assert_eq!(
            host.lock().store.snapshot().unwrap().sessions.len(),
            1,
            "idempotent retry must create exactly one Session"
        );
        assert_eq!(host.idempotency_count(), 1);

        std::fs::remove_dir_all(directory).ok();
    }

    #[test]
    fn local_commands_are_refused_without_a_local_capability() {
        let (directory, path) = temporary_database();
        let store = Store::open(&path).unwrap();
        let host = ProtocolHost::open(store).unwrap();
        let space_id = host
            .lock()
            .store
            .create_space("ukk", "/src/ukk")
            .unwrap()
            .id;
        let hello = ClientHello::local(RequestId::new_random(), vec![CURRENT_GENERATION]);
        let (session, _) = host
            .accept_hello(&TransportBinding::local_ipc(1000), &hello, now_ms())
            .unwrap();

        let envelope = session_create_envelope(&space_id, &new_id(), "local-refused");
        let error = host.command(&session, &envelope, now_ms()).unwrap_err();
        assert!(
            matches!(error, ProtocolHostError::Unsupported(_)),
            "{error}"
        );
        assert_eq!(host.session_count(), 0);
        assert_eq!(
            host.idempotency_count(),
            0,
            "a refused local command must not occupy an idempotency record"
        );

        std::fs::remove_dir_all(directory).ok();
    }

    #[test]
    fn execute_failure_aborts_the_in_flight_record() {
        let (directory, path) = temporary_database();
        let mut store = Store::open(&path).unwrap();
        let space = store.create_space("ukk", "/src/ukk").unwrap();
        let host = ProtocolHost::open(store).unwrap();
        let session = remote_session(&host, &[Scope::SessionManage]);

        // The request id already names a Session with a different title, so
        // execution fails after admission.
        let colliding_id = new_id();
        host.lock()
            .store
            .create_session_with_id(&space.id, "Existing title", &colliding_id)
            .unwrap();
        let colliding = session_create_envelope(&space.id, &colliding_id, "collision-key");
        let error = host.command(&session, &colliding, now_ms()).unwrap_err();
        assert!(matches!(error, ProtocolHostError::Store(_)), "{error}");
        assert_eq!(
            host.idempotency_count(),
            0,
            "an execute failure must abort the in-flight record"
        );

        // An unknown Space is rejected before admission.
        let unknown = session_create_envelope(&new_id(), &new_id(), "unknown-space-key");
        let error = host.command(&session, &unknown, now_ms()).unwrap_err();
        assert!(
            matches!(error, ProtocolHostError::Store(StoreError::NotFound(_))),
            "{error}"
        );
        assert_eq!(host.idempotency_count(), 0);

        std::fs::remove_dir_all(directory).ok();
    }

    #[test]
    fn delivery_and_handshake_reload_external_revocation_from_sqlite() {
        let (directory, path) = temporary_database();
        let store = Store::open(&path).unwrap();
        let host = ProtocolHost::open(store).unwrap();
        let device = new_id();
        let fingerprint = format!("sha256:{device}");
        host.register_test_device(
            &device,
            &fingerprint,
            &[Scope::SessionObserve],
            now_ms() + 60_000,
        )
        .unwrap();
        let claim = DeviceClaim {
            device_id: DeviceId::new(device.clone()).unwrap(),
            key_fingerprint: KeyFingerprint::new(fingerprint.clone()).unwrap(),
            generation: IdentityGeneration::FIRST,
        };
        let hello = ClientHello::remote(
            RequestId::new_random(),
            vec![CURRENT_GENERATION],
            claim.clone(),
        );
        let (session, _) = host
            .accept_hello(&TransportBinding::remote(), &hello, now_ms())
            .unwrap();
        assert!(host
            .deliver(&session, DurableCursor::ZERO, 16, now_ms())
            .is_ok());

        // Bypass the adapter's cache; the next delivery must re-read SQLite.
        host.revoke_device_in_store_only(&device).unwrap();
        let error = host
            .deliver(&session, DurableCursor::ZERO, 16, now_ms())
            .unwrap_err();
        assert_eq!(error.denial(), Some(&Denial::DeviceRevoked));

        // A new handshake must fail too.
        let hello = ClientHello::remote(RequestId::new_random(), vec![CURRENT_GENERATION], claim);
        let error = host
            .accept_hello(&TransportBinding::remote(), &hello, now_ms())
            .unwrap_err();
        assert_eq!(
            error.denial(),
            Some(&Denial::DeviceRevoked),
            "handshake must reload the durable registry"
        );

        std::fs::remove_dir_all(directory).ok();
    }

    #[test]
    fn broker_status_is_published_only_for_bound_agent_runs() {
        let (directory, path) = temporary_database();
        let mut store = Store::open(&path).unwrap();
        let space = store.create_space("ukk", "/src/ukk").unwrap();
        let session = store.create_session(&space.id, "Bound agent").unwrap();
        let run = store
            .bind_agent(&session.id, "core-1", "t3", "codex", None)
            .unwrap();
        let host = ProtocolHost::open(store).unwrap();

        assert!(
            !host
                .publish_broker_status(
                    Some("core-1"),
                    "unbound-terminal",
                    AgentStatus::Running,
                    now_ms()
                )
                .unwrap(),
            "an unbound terminal must not gain an Agent identity"
        );
        // Two cores sharing one terminal address must never be guessed at when
        // the reporting core is unknown.
        host.lock()
            .store
            .bind_agent(&session.id, "core-2", "t3", "claude", None)
            .unwrap();
        assert!(
            !host
                .publish_broker_status(None, "t3", AgentStatus::Running, now_ms())
                .unwrap(),
            "an ambiguous terminal binding must not be attributed"
        );
        assert!(host
            .publish_broker_status(Some("core-1"), "t3", AgentStatus::Running, now_ms())
            .unwrap());
        assert!(
            !host
                .publish_broker_status(Some("core-3"), "t3", AgentStatus::Running, now_ms())
                .unwrap(),
            "a different core must not claim another core's terminal"
        );

        let hello = ClientHello::local(RequestId::new_random(), vec![CURRENT_GENERATION]);
        let (auth, _) = host
            .accept_hello(&TransportBinding::local_ipc(1000), &hello, now_ms())
            .unwrap();
        let delivery = host
            .deliver(&auth, DurableCursor::ZERO, 16, now_ms())
            .unwrap();
        match delivery {
            Delivery::Events { events, next } => {
                assert_eq!(next, DurableCursor::from_raw(1));
                assert_eq!(events.len(), 1);
                match &events[0].event {
                    Event::AgentRunStatusChanged(changed) => {
                        assert_eq!(changed.session_id.as_str(), session.id);
                        assert_eq!(changed.agent_run_id.as_str(), run.id);
                        assert_eq!(changed.status, AgentStatus::Running);
                    }
                    other => panic!("unexpected event: {other:?}"),
                }
            }
            other => panic!("expected one durable event, got {other:?}"),
        }

        std::fs::remove_dir_all(directory).ok();
    }

    #[test]
    fn generation_mismatch_is_refused_before_execution() {
        let (directory, path) = temporary_database();
        let store = Store::open(&path).unwrap();
        let host = ProtocolHost::open(store).unwrap();
        let space_id = host
            .lock()
            .store
            .create_space("ukk", "/src/ukk")
            .unwrap()
            .id;
        let hello = ClientHello::local(RequestId::new_random(), vec![CURRENT_GENERATION]);
        let (session, _) = host
            .accept_hello(&TransportBinding::local_ipc(1), &hello, now_ms())
            .unwrap();
        let mut envelope = session_create_envelope(&space_id, &new_id(), "generation-key");
        envelope.protocol = ProtocolGeneration::from_raw(2);
        let error = host.command(&session, &envelope, now_ms()).unwrap_err();
        assert!(matches!(error, ProtocolHostError::Envelope(_)), "{error}");
        assert_eq!(host.lock().store.snapshot().unwrap().sessions.len(), 0);

        std::fs::remove_dir_all(directory).ok();
    }
}
