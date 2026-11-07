mod support;

use serde_json::{json, Value};
use support::{agent_send, envelope, limits, local_session, log, service, status_event, stream};
use ukk_protocol::{
    AckRequest, ClientHello, ClientMessage, CommandResult, CommandResultEnvelope, CursorCheckpoint,
    CursorError, DurableCursor, EnvelopeError, EventEnvelope, LimitError, ProtocolGeneration,
    RequestId, ServerError, ServerMessage, Snapshot, SubscribeRequest, CURRENT_GENERATION,
};

fn insert_top_level(raw: &str, field: &str, value: Value) -> String {
    let mut parsed: Value = serde_json::from_str(raw).expect("valid json");
    parsed
        .as_object_mut()
        .expect("object")
        .insert(field.to_owned(), value);
    serde_json::to_string(&parsed).expect("serializes")
}

#[test]
fn client_messages_round_trip_with_typed_envelopes() {
    let limits = limits();
    let hello = ClientMessage::Hello(ClientHello::local(
        RequestId::new_random(),
        vec![CURRENT_GENERATION],
    ));
    let command = ClientMessage::Command(envelope(agent_send("run the tests")));
    let subscribe = ClientMessage::Subscribe(SubscribeRequest::new(
        CURRENT_GENERATION,
        stream(),
        DurableCursor::ZERO,
        16,
    ));
    let ack = ClientMessage::Ack(AckRequest::new(
        CURRENT_GENERATION,
        stream(),
        DurableCursor::from_raw(2),
    ));
    for message in [hello, command, subscribe, ack] {
        let encoded = message.encode(&limits).expect("encodes");
        assert!(encoded.contains("\"type\":"));
        assert!(!encoded.contains("hc_seq"));
        assert!(!encoded.contains("host_epoch"));
        let decoded = ClientMessage::decode(&encoded, &limits).expect("decodes");
        assert_eq!(message, decoded);
    }
}

#[test]
fn server_messages_round_trip() {
    let limits = limits();
    let mut log = log();
    let event = log
        .append(status_event(3), 1_700_000_000_000)
        .expect("append");
    let result = CommandResultEnvelope {
        request_id: RequestId::new_random(),
        replayed: true,
        result: CommandResult::Completed {
            detail: Some("dispatched".to_owned()),
        },
    };
    let snapshot = Snapshot::new(
        stream(),
        DurableCursor::from_raw(1),
        4,
        json!({"sessions": []}),
    );
    let error = ServerError {
        protocol: CURRENT_GENERATION,
        request_id: Some(RequestId::new_random()),
        denial: Some(ukk_protocol::Denial::MissingScope {
            scope: ukk_protocol::Scope::AgentSend,
        }),
        message: "missing scope agent.send".to_owned(),
    };
    let hello = ServerMessage::Hello(ukk_protocol::ServerHello {
        protocol: CURRENT_GENERATION,
        request_id: RequestId::new_random(),
        features: ukk_protocol::Features::default_server(),
        scopes: ukk_protocol::ScopeSet::empty(),
        streams: vec![log.bounds()],
    });
    for message in [
        hello,
        ServerMessage::Result(result),
        ServerMessage::Event(event),
        ServerMessage::Snapshot(snapshot),
        ServerMessage::Error(error),
    ] {
        let encoded = message.encode(&limits).expect("encodes");
        let decoded = ServerMessage::decode(&encoded, &limits).expect("decodes");
        assert_eq!(message, decoded);
    }
}

#[test]
fn wire_shape_is_tagged_and_generation_qualified() {
    let encoded = ClientMessage::Command(envelope(agent_send("hello")))
        .encode(&limits())
        .expect("encodes");
    let parsed: Value = serde_json::from_str(&encoded).expect("json");
    assert_eq!(parsed["type"], "command");
    assert_eq!(parsed["protocol"], 1);
    assert_eq!(parsed["command"]["command"], "agent_send");
    assert!(parsed["request_id"].is_string());
    assert!(parsed["idempotency_key"].is_string());
}

#[test]
fn reserved_ephemeral_cursor_fields_are_rejected() {
    let limits = limits();
    let raw = ClientMessage::Command(envelope(agent_send("hello")))
        .encode(&limits)
        .expect("encodes");
    for field in ["hc_seq", "host_epoch"] {
        let mutated = insert_top_level(&raw, field, json!(42));
        let error = ClientMessage::decode(&mutated, &limits).expect_err("reserved field");
        assert_eq!(
            error,
            EnvelopeError::ReservedEphemeralField(if field == "hc_seq" {
                "hc_seq"
            } else {
                "host_epoch"
            })
        );
    }

    let event = EventEnvelope {
        protocol: CURRENT_GENERATION,
        event_id: ukk_protocol::EventId::new_random(),
        stream: stream(),
        cursor: DurableCursor::from_raw(1),
        emitted_at_ms: 0,
        event: status_event(1),
    };
    let raw = event.encode(&limits).expect("encodes");
    let mutated = insert_top_level(&raw, "hc_seq", json!(9));
    assert_eq!(
        EventEnvelope::decode(&mutated, &limits).expect_err("reserved"),
        EnvelopeError::ReservedEphemeralField("hc_seq")
    );

    let checkpoint = CursorCheckpoint::new(stream(), DurableCursor::from_raw(3));
    let raw = checkpoint.encode().expect("encodes");
    let mutated = insert_top_level(&raw, "host_epoch", json!("boot-1"));
    assert_eq!(
        CursorCheckpoint::decode(&mutated).expect_err("reserved"),
        CursorError::ReservedEphemeralField("host_epoch")
    );
}

#[test]
fn unknown_fields_are_rejected() {
    let limits = limits();
    let raw = ClientMessage::Ack(AckRequest::new(
        CURRENT_GENERATION,
        stream(),
        DurableCursor::from_raw(1),
    ))
    .encode(&limits)
    .expect("encodes");
    let mutated = insert_top_level(&raw, "plane", json!("remote"));
    assert!(matches!(
        ClientMessage::decode(&mutated, &limits),
        Err(EnvelopeError::Json(_))
    ));
}

#[test]
fn version_negotiation_mismatch_is_typed() {
    let policy = ukk_protocol::ServerPolicy::default();
    assert_eq!(
        policy.negotiate(&[ProtocolGeneration::from_raw(2)]),
        Err(ukk_protocol::NegotiationError::NoCommonGeneration {
            server: ukk_protocol::ProtocolRange::current()
        })
    );
    assert_eq!(
        policy.negotiate(&[]),
        Err(ukk_protocol::NegotiationError::EmptyClientOffer)
    );
    assert_eq!(
        policy.negotiate(&[ProtocolGeneration::from_raw(0)]),
        Err(ukk_protocol::NegotiationError::InvalidGeneration(
            ProtocolGeneration::from_raw(0)
        ))
    );
    assert_eq!(
        policy.negotiate(&[
            ProtocolGeneration::from_raw(1),
            ProtocolGeneration::from_raw(2)
        ]),
        Ok(ukk_protocol::Negotiated::new(CURRENT_GENERATION))
    );
}

#[test]
fn envelope_generation_must_match_the_negotiated_generation() {
    let mut service = service();
    let session = local_session(&service);
    let mut command = envelope(agent_send("hello"));
    command.protocol = ProtocolGeneration::from_raw(2);
    let error = service
        .admit_command(&session, &command, 1_700_000_000_000)
        .expect_err("mismatch");
    assert_eq!(
        error,
        ukk_protocol::CommandError::Envelope(EnvelopeError::GenerationMismatch {
            expected: CURRENT_GENERATION,
            actual: ProtocolGeneration::from_raw(2),
        })
    );
}

#[test]
fn oversized_command_payload_is_rejected_on_encode_and_decode() {
    let mut limits = limits();
    limits.max_payload_bytes = 64;
    limits.max_envelope_bytes = 512;
    let command = envelope(agent_send(&"x".repeat(200)));
    let error = command.encode(&limits).expect_err("too large");
    assert!(matches!(
        error,
        EnvelopeError::Limit(LimitError::PayloadTooLarge { .. })
    ));

    let raw = serde_json::to_string(&json!({
        "protocol": 1,
        "request_id": "req-1",
        "idempotency_key": "key-1",
        "issued_at_ms": 0,
        "command": {
            "command": "agent_send",
            "session_id": "session-1",
            "agent_run_id": "agent-run-1",
            "text": "y".repeat(200),
            "expected_revision": 3
        }
    }))
    .expect("json");
    let error = ukk_protocol::CommandEnvelope::decode(&raw, &limits).expect_err("too large");
    assert!(matches!(
        error,
        EnvelopeError::Limit(LimitError::PayloadTooLarge { .. })
    ));
}

#[test]
fn oversized_event_and_snapshot_are_rejected() {
    let mut limits = limits();
    limits.max_payload_bytes = 64;
    limits.max_envelope_bytes = 1024;
    limits.max_snapshot_bytes = 128;
    let mut log = support::log_with(limits);
    let event = ukk_protocol::Event::SessionCreated(ukk_protocol::SessionCreated {
        space_id: ukk_protocol::DomainId::new("space-1").expect("valid"),
        session_id: support::session_id(),
        title: "t".repeat(200),
        revision: 1,
    });
    let error = log.append(event, 0).expect_err("event too large");
    assert!(matches!(
        error,
        ukk_protocol::SyncError::Envelope(EnvelopeError::Limit(_))
            | ukk_protocol::SyncError::Envelope(EnvelopeError::Invalid(_))
    ));

    let snapshot = Snapshot::new(
        stream(),
        DurableCursor::from_raw(1),
        1,
        json!({"body": "s".repeat(500)}),
    );
    let error = snapshot.encode(&limits).expect_err("snapshot too large");
    assert!(matches!(
        error,
        EnvelopeError::Limit(LimitError::SnapshotTooLarge { .. })
    ));
}

#[test]
fn snapshot_body_must_be_an_object() {
    let limits = limits();
    let snapshot = Snapshot::new(
        stream(),
        DurableCursor::from_raw(1),
        1,
        json!(["not", "an", "object"]),
    );
    assert_eq!(
        snapshot.encode(&limits).expect_err("invalid body"),
        EnvelopeError::Invalid("snapshot body must be a JSON object")
    );
}

#[test]
fn approval_without_revision_is_rejected() {
    let mut command = ukk_protocol::Command::ApprovalRespond(ukk_protocol::ApprovalRespond {
        session_id: support::session_id(),
        approval_id: ukk_protocol::DomainId::new("approval-1").expect("valid"),
        decision: ukk_protocol::ApprovalDecision::Approve,
        revision: 0,
    });
    if let ukk_protocol::Command::ApprovalRespond(respond) = &mut command {
        respond.revision = 0;
    }
    let error = command.validate(&limits()).expect_err("revision required");
    assert_eq!(
        error,
        EnvelopeError::Invalid("approval requires a positive revision")
    );
}
