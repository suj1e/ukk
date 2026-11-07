#![allow(dead_code)]
//! Shared fixtures for protocol contract tests.

use ukk_protocol::{
    AgentRunStatusChanged, AgentSend, AgentStatus, ApprovalDecision, ApprovalRespond, ClientHello,
    Command, CommandEnvelope, DeviceClaim, DeviceId, DomainId, Event, EventId, EventLog,
    IdempotencyKey, IdentityGeneration, KeyFingerprint, LeaseId, Limits, PointerEvent,
    PointerInput, ProtocolService, RequestId, Scope, StreamId, TransportBinding,
    CURRENT_GENERATION,
};

pub const NOW: i64 = 1_700_000_000_000;
pub const DAY_MS: i64 = 86_400_000;

pub fn limits() -> Limits {
    Limits::default()
}

pub fn service() -> ProtocolService {
    ProtocolService::new(limits()).expect("default limits are valid")
}

pub fn stream() -> StreamId {
    StreamId::new("workspace").expect("valid stream id")
}

pub fn session_id() -> DomainId {
    DomainId::new("session-1").expect("valid session id")
}

pub fn agent_run_id() -> DomainId {
    DomainId::new("agent-run-1").expect("valid agent run id")
}

pub fn log() -> EventLog {
    EventLog::new(stream(), limits()).expect("valid log")
}

pub fn log_with(limits: Limits) -> EventLog {
    EventLog::new(stream(), limits).expect("valid log")
}

pub struct DeviceFixture {
    pub device_id: DeviceId,
    pub fingerprint: KeyFingerprint,
    pub generation: IdentityGeneration,
    pub claim: DeviceClaim,
}

pub fn register_device(service: &mut ProtocolService, scopes: &[Scope]) -> DeviceFixture {
    register_device_with_ttl(service, scopes, NOW + DAY_MS)
}

pub fn register_device_with_ttl(
    service: &mut ProtocolService,
    scopes: &[Scope],
    expires_at_ms: i64,
) -> DeviceFixture {
    let device_id = DeviceId::new_random();
    let fingerprint =
        KeyFingerprint::new(format!("fingerprint-{}", device_id.as_str())).expect("valid");
    let generation = service
        .trust
        .register(device_id.clone(), fingerprint.clone())
        .expect("new device");
    for scope in scopes {
        service
            .trust
            .grant(&device_id, *scope, generation, expires_at_ms)
            .expect("valid grant");
    }
    let claim = DeviceClaim {
        device_id: device_id.clone(),
        key_fingerprint: fingerprint.clone(),
        generation,
    };
    DeviceFixture {
        device_id,
        fingerprint,
        generation,
        claim,
    }
}

pub fn remote_session(
    service: &ProtocolService,
    fixture: &DeviceFixture,
) -> ukk_protocol::AuthenticatedSession {
    let hello = ClientHello::remote(
        RequestId::new_random(),
        vec![CURRENT_GENERATION],
        fixture.claim.clone(),
    );
    service
        .accept_hello(&TransportBinding::remote(), &hello, NOW)
        .expect("remote handshake succeeds")
}

pub fn local_session(service: &ProtocolService) -> ukk_protocol::AuthenticatedSession {
    let hello = ClientHello::local(RequestId::new_random(), vec![CURRENT_GENERATION]);
    service
        .accept_hello(&TransportBinding::local_ipc(501), &hello, NOW)
        .expect("local handshake succeeds")
}

pub fn agent_send(text: &str) -> Command {
    Command::AgentSend(AgentSend {
        session_id: session_id(),
        agent_run_id: agent_run_id(),
        text: text.to_owned(),
        expected_revision: Some(7),
    })
}

pub fn approval(decision: ApprovalDecision) -> Command {
    Command::ApprovalRespond(ApprovalRespond {
        session_id: session_id(),
        approval_id: DomainId::new("approval-1").expect("valid"),
        decision,
        revision: 7,
    })
}

pub fn pointer(lease_id: Option<LeaseId>) -> Command {
    Command::PointerInput(PointerInput {
        session_id: session_id(),
        lease_id,
        events: vec![PointerEvent::Move { x: 12.0, y: 24.0 }],
    })
}

pub fn envelope(command: Command) -> CommandEnvelope {
    CommandEnvelope::new(command, NOW)
}

pub fn envelope_with_key(command: Command, key: IdempotencyKey) -> CommandEnvelope {
    let mut envelope = CommandEnvelope::new(command, NOW);
    envelope.idempotency_key = key;
    envelope
}

pub fn status_event(revision: u64) -> Event {
    Event::AgentRunStatusChanged(AgentRunStatusChanged {
        session_id: session_id(),
        agent_run_id: agent_run_id(),
        status: AgentStatus::Running,
        revision,
    })
}

pub fn event_with_cursor(event_id: EventId, cursor: u64) -> ukk_protocol::EventEnvelope {
    ukk_protocol::EventEnvelope {
        protocol: CURRENT_GENERATION,
        event_id,
        stream: stream(),
        cursor: ukk_protocol::DurableCursor::from_raw(cursor),
        emitted_at_ms: NOW,
        event: status_event(cursor),
    }
}
