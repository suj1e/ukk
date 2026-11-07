use ukk_agent_adapters::capability::{CapabilityDescriptor, DetectionError, VersionStatus};
use ukk_agent_adapters::contract::{
    ApprovalDecision, ApprovalEvidence, EventPayload, FallbackReason, ToolInput, ToolStatus,
    TurnStatus, UnsupportedScope,
};
use ukk_agent_adapters::fixtures;
use ukk_agent_adapters::{
    adapter_for, detect_adapter, AdapterConfig, AdapterError, Limits, MergeEngine, MergeOutcome,
    ProviderAdapter, ProviderHandshake, ProviderKind, RuleRedactor, SessionId, StreamId,
    CLAUDE_REQUIRED_METHODS, CODEX_REQUIRED_METHODS,
};

fn codex_adapter(config: AdapterConfig) -> Box<dyn ProviderAdapter> {
    adapter_for(ProviderKind::CodexAppServer, config)
}

fn claude_adapter(config: AdapterConfig) -> Box<dyn ProviderAdapter> {
    adapter_for(ProviderKind::ClaudeStreamJson, config)
}

fn run_fixture(
    adapter: &mut dyn ProviderAdapter,
    merge: &mut MergeEngine,
    stream_name: &str,
    fixture: &str,
) -> Vec<ukk_agent_adapters::ParseOutcome> {
    let stream = StreamId::from(stream_name);
    let mut outcomes = Vec::new();
    for (index, line) in fixtures::lines(fixture).enumerate() {
        let sequence = u64::try_from(index).unwrap() + 1;
        let outcome = adapter.parse_line(&stream, sequence, line);
        assert!(
            outcome.error.is_none(),
            "fixture parse failed: {:?}",
            outcome.error
        );
        let ingest = merge.ingest_batch(outcome.events.clone());
        assert!(
            ingest.fallback.is_none(),
            "merge fallback: {:?}",
            ingest.fallback
        );
        outcomes.push(outcome);
    }
    outcomes
}

#[test]
fn codex_fixture_parses_and_merges_turn() {
    let mut adapter = codex_adapter(AdapterConfig::default());
    let mut merge = MergeEngine::new(ProviderKind::CodexAppServer, Limits::default());
    run_fixture(
        adapter.as_mut(),
        &mut merge,
        "codex-turn",
        fixtures::CODEX_V2_TURN,
    );

    let session = merge.session();
    assert_eq!(
        session.id.as_ref().map(SessionId::as_str),
        Some("thr_fixture")
    );
    assert_eq!(session.messages["user_1"].text(), "Run the portfolio tests");
    assert_eq!(session.messages["msg_1"].text(), "I will run the tests.");
    assert_eq!(
        session.messages["msg_1"].phase.as_deref(),
        Some("final_answer")
    );
    assert!(session.messages["msg_1"].complete);

    let tool = &session.tools["cmd_1"];
    assert_eq!(tool.kind, ukk_agent_adapters::ToolKind::Command);
    assert_eq!(tool.status, ToolStatus::Completed);
    assert_eq!(tool.exit_code, Some(0));
    let output = tool.output.as_ref().expect("command output");
    assert!(output.text.contains("ok"));

    assert_eq!(
        session.turns["turn_fixture"].info.status,
        TurnStatus::Completed
    );
    assert!(session
        .progress
        .iter()
        .any(|progress| progress.kind == ukk_agent_adapters::ProgressKind::ToolOutput));
    assert!(session.unsupported.is_empty());
}

#[test]
fn codex_duplicate_item_id_is_idempotent() {
    let mut adapter = codex_adapter(AdapterConfig::default());
    let mut merge = MergeEngine::new(ProviderKind::CodexAppServer, Limits::default());
    let completed = fixtures::lines(fixtures::CODEX_V2_TURN)
        .nth(7)
        .expect("agent message completion line");
    let stream = StreamId::from("duplicates");

    let first = adapter.parse_line(&stream, 1, completed);
    assert_eq!(first.events.len(), 1);
    assert!(matches!(
        merge.ingest(first.events[0].clone()).outcomes.as_slice(),
        [MergeOutcome::Applied { .. }]
    ));

    let second = adapter.parse_line(&stream, 2, completed);
    assert_eq!(second.events.len(), 1);
    assert!(matches!(
        merge.ingest(second.events[0].clone()).outcomes.as_slice(),
        [MergeOutcome::Duplicate { .. }]
    ));
    assert_eq!(merge.session().messages.len(), 1);
    assert_eq!(
        merge.session().messages["msg_1"].text(),
        "I will run the tests."
    );
}

#[test]
fn codex_out_of_order_stream_is_buffered_until_the_gap_closes() {
    let mut adapter = codex_adapter(AdapterConfig::default());
    let mut merge = MergeEngine::new(ProviderKind::CodexAppServer, Limits::default());
    let lines: Vec<&str> = fixtures::lines(fixtures::CODEX_V2_TURN).take(8).collect();
    let stream = StreamId::from("out-of-order");

    let late = adapter.parse_line(&stream, 3, lines[7]);
    let late_ingest = merge.ingest(late.events[0].clone());
    assert!(matches!(
        late_ingest.outcomes.as_slice(),
        [MergeOutcome::Buffered {
            awaiting_sequence: 1
        }]
    ));
    assert_eq!(late_ingest.buffered, 1);
    assert!(merge.session().messages.is_empty());

    let first = adapter.parse_line(&stream, 1, lines[0]);
    let first_ingest = merge.ingest(first.events[0].clone());
    assert!(matches!(
        first_ingest.outcomes.as_slice(),
        [MergeOutcome::Applied { .. }]
    ));
    assert_eq!(first_ingest.buffered, 1);

    let second = adapter.parse_line(&stream, 2, lines[1]);
    let second_ingest = merge.ingest(second.events[0].clone());
    assert_eq!(second_ingest.outcomes.len(), 2);
    assert_eq!(
        merge.session().messages["msg_1"].text(),
        "I will run the tests."
    );
}

#[test]
fn codex_resume_preserves_history() {
    let mut adapter = codex_adapter(AdapterConfig::default());
    let mut merge = MergeEngine::new(ProviderKind::CodexAppServer, Limits::default());
    run_fixture(
        adapter.as_mut(),
        &mut merge,
        "initial",
        fixtures::CODEX_V2_TURN,
    );
    let message_count = merge.session().messages.len();
    assert!(message_count >= 2);

    let mut resumed =
        codex_adapter(AdapterConfig::default().with_resume_session(SessionId::from("thr_fixture")));
    let outcomes = run_fixture(
        resumed.as_mut(),
        &mut merge,
        "resumed",
        fixtures::CODEX_V2_RESUME,
    );

    let first = &outcomes[0].events[0];
    assert!(matches!(first.payload, EventPayload::SessionResumed { .. }));
    let session = merge.session();
    assert!(session.resumed);
    assert_eq!(session.messages.len(), message_count + 1);
    assert_eq!(
        session.messages["msg_resumed"].text(),
        "Resumed context is available."
    );
    assert!(session.messages.contains_key("msg_1"));
}

#[test]
fn codex_unknown_actions_are_unsupported_and_not_fabricated() {
    let mut adapter = codex_adapter(AdapterConfig::default());
    let mut merge = MergeEngine::new(ProviderKind::CodexAppServer, Limits::default());
    let outcomes = run_fixture(
        adapter.as_mut(),
        &mut merge,
        "unsupported",
        fixtures::CODEX_V2_UNSUPPORTED,
    );

    assert_eq!(outcomes.len(), 2);
    for outcome in &outcomes {
        assert!(matches!(
            outcome.events[0].payload,
            EventPayload::Unsupported { .. }
        ));
    }
    let session = merge.session();
    assert_eq!(session.unsupported.len(), 2);
    assert_eq!(session.unsupported[0].scope, UnsupportedScope::Action);
    assert!(session.messages.is_empty());
    assert!(session.tools.is_empty());
    assert!(session.turns.is_empty());
}

#[test]
fn codex_approval_provenance_is_retained_without_an_invented_decision() {
    let mut adapter = codex_adapter(AdapterConfig::default());
    let mut merge = MergeEngine::new(ProviderKind::CodexAppServer, Limits::default());
    let stream = StreamId::from("approval");
    let lines: Vec<&str> = fixtures::lines(fixtures::CODEX_V2_APPROVAL).collect();

    let mut parsed_request = None;
    for (index, line) in lines.iter().enumerate() {
        let sequence = u64::try_from(index).unwrap() + 1;
        let outcome = adapter.parse_line(&stream, sequence, line);
        assert!(outcome.error.is_none(), "{:?}", outcome.error);
        if index == 2 {
            parsed_request = Some(outcome.events[0].clone());
        }
        for event in &outcome.events {
            merge.ingest(event.clone());
        }
    }

    let request_event = parsed_request.expect("approval request parsed");
    assert_eq!(
        request_event.provenance.native_type,
        "item/commandExecution/requestApproval"
    );
    assert_eq!(request_event.provenance.native_id.as_deref(), Some("42"));
    assert_eq!(
        request_event
            .provenance
            .session_id
            .as_ref()
            .map(SessionId::as_str),
        Some("thr_fixture")
    );
    assert_eq!(
        request_event
            .provenance
            .turn_id
            .as_ref()
            .map(|id| id.as_str()),
        Some("turn_fixture")
    );
    assert_eq!(
        request_event
            .provenance
            .item_id
            .as_ref()
            .map(|id| id.as_str()),
        Some("cmd_approval")
    );

    let approval = &merge.session().approvals["42"];
    assert_eq!(approval.request.id.as_str(), "42");
    assert_eq!(approval.request.request_id.as_str(), "42");
    assert_eq!(
        approval.request.item_id.as_ref().unwrap().as_str(),
        "cmd_approval"
    );
    assert!(matches!(
        approval.request.subject,
        ukk_agent_adapters::ApprovalSubject::Command { .. }
    ));
    assert_eq!(approval.request.options.len(), 5);
    let clearance = approval.clearance.as_ref().expect("clearance");
    assert_eq!(
        clearance.evidence,
        ApprovalEvidence::ItemStatus(ToolStatus::Declined)
    );
    assert_eq!(clearance.decision, Some(ApprovalDecision::Decline));
}

#[test]
fn codex_redaction_applies_to_messages_tools_and_approvals() {
    let config =
        AdapterConfig::default().with_redactor(RuleRedactor::new([("sk-secret", "[REDACTED]")]));
    let mut adapter = codex_adapter(config);
    let mut merge = MergeEngine::new(ProviderKind::CodexAppServer, Limits::default());
    let stream = StreamId::from("redaction");

    let message_line = r#"{"method":"item/completed","params":{"threadId":"thr_r","turnId":"turn_r","item":{"id":"msg_r","type":"agentMessage","text":"token sk-secret","phase":null}}}"#;
    let tool_line = r#"{"method":"item/completed","params":{"threadId":"thr_r","turnId":"turn_r","item":{"id":"cmd_r","type":"commandExecution","command":"echo sk-secret","cwd":"/repo","status":"completed","aggregatedOutput":"sk-secret","exitCode":0}}}"#;
    let approval_line = r#"{"id":"7","method":"item/commandExecution/requestApproval","params":{"threadId":"thr_r","turnId":"turn_r","itemId":"cmd_r","reason":"needs sk-secret","command":"echo sk-secret","cwd":"/repo"}}"#;

    for (index, line) in [message_line, tool_line, approval_line].iter().enumerate() {
        let outcome = adapter.parse_line(&stream, u64::try_from(index).unwrap() + 1, line);
        assert!(outcome.error.is_none(), "{:?}", outcome.error);
        for event in &outcome.events {
            merge.ingest(event.clone());
        }
    }

    let session = merge.session();
    assert_eq!(session.messages["msg_r"].text(), "token [REDACTED]");
    let tool = &session.tools["cmd_r"];
    let output = tool.output.as_ref().expect("tool output");
    assert_eq!(output.text, "[REDACTED]");
    if let Some(ToolInput::Command { command, .. }) = &tool.input {
        assert_eq!(command.text, "echo [REDACTED]");
    } else {
        panic!("expected command input");
    }
    let approval = &session.approvals["7"];
    if let ukk_agent_adapters::ApprovalSubject::Command {
        reason, command, ..
    } = &approval.request.subject
    {
        assert_eq!(reason.as_ref().unwrap().text, "needs [REDACTED]");
        assert_eq!(command.as_ref().unwrap().text, "echo [REDACTED]");
    } else {
        panic!("expected command approval subject");
    }
    assert!(!format!("{session:?}").contains("sk-secret"));
}

#[test]
fn codex_malformed_input_degrades_explicitly_without_events() {
    let mut adapter = codex_adapter(AdapterConfig::default());
    let stream = StreamId::from("malformed");
    let outcome = adapter.parse_line(&stream, 1, "{not json");
    assert!(outcome.events.is_empty());
    assert!(matches!(
        outcome.error,
        Some(AdapterError::Malformed { .. })
    ));
    let fallback = outcome.fallback.expect("fallback");
    assert_eq!(fallback.reason, FallbackReason::MalformedEvent);
    assert!(!fallback.structured_events_available);

    let outcome = adapter.parse_line(&stream, 2, r#"{"foo":1}"#);
    assert!(outcome.events.is_empty());
    assert!(matches!(
        outcome.error,
        Some(AdapterError::Malformed { .. })
    ));
}

#[test]
fn codex_bounded_payloads_truncate_and_report() {
    let limits = Limits {
        max_line_bytes: 64 * 1024,
        max_text_bytes: 8,
        max_input_bytes: 8,
        max_output_bytes: 8,
        ..Limits::default()
    };
    let mut adapter = codex_adapter(AdapterConfig::default().with_limits(limits));
    let mut merge = MergeEngine::new(ProviderKind::CodexAppServer, Limits::default());
    let stream = StreamId::from("bounded");

    let message_line = r#"{"method":"item/completed","params":{"threadId":"thr_b","turnId":"turn_b","item":{"id":"msg_b","type":"agentMessage","text":"0123456789","phase":null}}}"#;
    let tool_line = r#"{"method":"item/completed","params":{"threadId":"thr_b","turnId":"turn_b","item":{"id":"cmd_b","type":"commandExecution","command":"0123456789","cwd":"/repo","status":"completed","aggregatedOutput":"0123456789","exitCode":0}}}"#;
    for (index, line) in [message_line, tool_line].iter().enumerate() {
        let outcome = adapter.parse_line(&stream, u64::try_from(index).unwrap() + 1, line);
        assert!(outcome.error.is_none(), "{:?}", outcome.error);
        for event in &outcome.events {
            merge.ingest(event.clone());
        }
    }

    let message = &merge.session().messages["msg_b"];
    assert!(message.truncated);
    assert_eq!(message.text(), "01234567");
    let output = merge.session().tools["cmd_b"].output.as_ref().unwrap();
    assert!(output.truncated);
    assert_eq!(output.text, "01234567");
}

#[test]
fn oversized_line_is_rejected_with_fallback() {
    let limits = Limits {
        max_line_bytes: 16,
        ..Limits::default()
    };
    let mut adapter = codex_adapter(AdapterConfig::default().with_limits(limits));
    let stream = StreamId::from("oversized");
    let outcome = adapter.parse_line(&stream, 1, &"x".repeat(64));
    assert!(outcome.events.is_empty());
    assert!(matches!(
        outcome.error,
        Some(AdapterError::PayloadTooLarge { .. })
    ));
    assert_eq!(
        outcome.fallback.expect("fallback").reason,
        FallbackReason::PayloadTooLarge
    );
}

#[test]
fn claude_fixture_parses_and_merges_turn() {
    let mut adapter = claude_adapter(AdapterConfig::default());
    let mut merge = MergeEngine::new(ProviderKind::ClaudeStreamJson, Limits::default());
    run_fixture(
        adapter.as_mut(),
        &mut merge,
        "claude-turn",
        fixtures::CLAUDE_STREAM_JSON_TURN,
    );

    let session = merge.session();
    assert_eq!(
        session.id.as_ref().map(SessionId::as_str),
        Some("sess_fixture")
    );
    assert_eq!(session.messages["msg_1"].text(), "I will check the tests.");
    let tool = &session.tools["toolu_1"];
    assert_eq!(tool.name.as_deref(), Some("Bash"));
    assert_eq!(tool.kind, ukk_agent_adapters::ToolKind::ProviderNamed);
    assert_eq!(tool.status, ToolStatus::Completed);
    assert!(tool
        .output
        .as_ref()
        .expect("tool output")
        .text
        .contains("42 passed"));
    if let Some(ToolInput::Named { arguments, .. }) = &tool.input {
        assert!(arguments.text.contains("cargo test --workspace"));
    } else {
        panic!("expected named tool input");
    }

    let turn = session.turns.values().next().expect("turn");
    assert_eq!(turn.info.status, TurnStatus::Completed);
    assert!(session.unsupported.is_empty());
    assert!(session.approvals.is_empty());
}

#[test]
fn claude_partial_stream_is_replaced_by_the_authoritative_message() {
    let mut adapter = claude_adapter(AdapterConfig::default());
    let mut merge = MergeEngine::new(ProviderKind::ClaudeStreamJson, Limits::default());
    run_fixture(
        adapter.as_mut(),
        &mut merge,
        "claude-partial",
        fixtures::CLAUDE_STREAM_JSON_PARTIAL,
    );

    let message = &merge.session().messages["msg_stream"];
    assert!(message.complete);
    assert_eq!(message.text(), "Hello world");
}

#[test]
fn claude_progress_and_permission_denial_are_typed() {
    let mut adapter = claude_adapter(AdapterConfig::default());
    let mut merge = MergeEngine::new(ProviderKind::ClaudeStreamJson, Limits::default());
    run_fixture(
        adapter.as_mut(),
        &mut merge,
        "claude-progress",
        fixtures::CLAUDE_STREAM_JSON_PROGRESS,
    );

    let session = merge.session();
    let retry = session
        .progress
        .iter()
        .find(|progress| progress.kind == ukk_agent_adapters::ProgressKind::Retry)
        .expect("retry progress");
    assert_eq!(retry.attempt, Some(1));
    assert_eq!(retry.max_attempts, Some(3));
    assert_eq!(retry.retry_delay_ms, Some(1000));

    let tool_progress = session
        .progress
        .iter()
        .find(|progress| progress.kind == ukk_agent_adapters::ProgressKind::ToolProgress)
        .expect("tool progress");
    assert_eq!(tool_progress.elapsed_seconds, Some(30));
    assert_eq!(tool_progress.heartbeat, Some(true));

    let denial = &session.permission_denials[0];
    assert_eq!(denial.tool_call_id.as_str(), "toolu_denied");
    assert_eq!(denial.tool_name, "Bash");
    assert_eq!(denial.reason.as_deref(), Some("denied by settings"));
    assert!(session.approvals.is_empty());

    assert!(matches!(
        adapter.descriptor().capabilities.approvals,
        ukk_agent_adapters::Capability::Unsupported { .. }
    ));
}

#[test]
fn claude_unknown_actions_and_control_request_are_unsupported() {
    let mut adapter = claude_adapter(AdapterConfig::default());
    let mut merge = MergeEngine::new(ProviderKind::ClaudeStreamJson, Limits::default());
    let outcomes = run_fixture(
        adapter.as_mut(),
        &mut merge,
        "claude-unsupported",
        fixtures::CLAUDE_STREAM_JSON_UNSUPPORTED,
    );

    assert_eq!(outcomes.len(), 3);
    for outcome in &outcomes {
        assert!(matches!(
            outcome.events[0].payload,
            EventPayload::Unsupported { .. }
        ));
    }
    let session = merge.session();
    assert_eq!(session.unsupported.len(), 3);
    assert!(session.approvals.is_empty());
    assert!(session.messages.is_empty());
    assert!(session.tools.is_empty());
}

#[test]
fn detection_rejects_unknown_provider_and_unsupported_versions() {
    let unknown = match detect_adapter(
        "mystery-agent",
        &ProviderHandshake::default(),
        AdapterConfig::default(),
    ) {
        Ok(_) => panic!("unknown provider must not be accepted"),
        Err(failure) => failure,
    };
    assert!(matches!(
        unknown.error,
        DetectionError::UnknownProvider { .. }
    ));
    assert_eq!(unknown.fallback.reason, FallbackReason::UnknownProvider);
    assert!(!unknown.fallback.structured_events_available);

    let mut codex = ProviderHandshake {
        wire_version: Some("v1".to_owned()),
        ..ProviderHandshake::default()
    };
    codex.observed_methods = CODEX_REQUIRED_METHODS
        .iter()
        .map(|m| (*m).to_owned())
        .collect();
    let codex_error = match detect_adapter("codex", &codex, AdapterConfig::default()) {
        Ok(_) => panic!("codex v1 must not be accepted"),
        Err(failure) => failure,
    };
    assert!(matches!(
        codex_error.error,
        DetectionError::UnsupportedVersion { .. }
    ));

    let mut claude = ProviderHandshake {
        declared_version: Some("1.0.0".to_owned()),
        ..ProviderHandshake::default()
    };
    claude.observed_methods = CLAUDE_REQUIRED_METHODS
        .iter()
        .map(|m| (*m).to_owned())
        .collect();
    let claude_error = match detect_adapter("claude", &claude, AdapterConfig::default()) {
        Ok(_) => panic!("claude v1 must not be accepted"),
        Err(failure) => failure,
    };
    assert!(matches!(
        claude_error.error,
        DetectionError::UnsupportedVersion { .. }
    ));
    assert_eq!(
        claude_error.fallback.reason,
        FallbackReason::UnsupportedVersion
    );
}

#[test]
fn detection_accepts_documented_handshakes() {
    let mut codex = ProviderHandshake {
        wire_version: Some("v2".to_owned()),
        declared_version: Some("0.60.0".to_owned()),
        ..ProviderHandshake::default()
    };
    codex.observed_methods = CODEX_REQUIRED_METHODS
        .iter()
        .map(|m| (*m).to_owned())
        .collect();
    let adapter = detect_adapter("codex", &codex, AdapterConfig::default()).expect("codex v2");
    assert_eq!(adapter.provider(), ProviderKind::CodexAppServer);
    assert!(matches!(
        adapter.descriptor().version_status,
        VersionStatus::Detected { .. }
    ));
    assert!(adapter.descriptor().capabilities.approvals.is_supported());

    let mut claude = ProviderHandshake {
        declared_version: Some("2.1.191".to_owned()),
        ..ProviderHandshake::default()
    };
    claude.observed_methods = CLAUDE_REQUIRED_METHODS
        .iter()
        .map(|m| (*m).to_owned())
        .collect();
    let adapter = detect_adapter("claude", &claude, AdapterConfig::default()).expect("claude 2.x");
    assert_eq!(adapter.provider(), ProviderKind::ClaudeStreamJson);
    assert!(!adapter.descriptor().capabilities.plans.is_supported());
    assert!(!adapter.descriptor().capabilities.approvals.is_supported());
}

#[test]
fn contract_version_mismatch_degrades_without_applying() {
    let mut adapter = codex_adapter(AdapterConfig::default());
    let stream = StreamId::from("contract");
    let line = fixtures::lines(fixtures::CODEX_V2_TURN).next().unwrap();
    let outcome = adapter.parse_line(&stream, 1, line);
    let mut event = outcome.events[0].clone();
    event.contract_version = 99;
    let mut merge = MergeEngine::new(ProviderKind::CodexAppServer, Limits::default());
    let ingest = merge.ingest(event);
    assert!(merge.session().id.is_none());
    assert_eq!(
        ingest.fallback.expect("fallback").reason,
        FallbackReason::ContractVersionMismatch
    );
}

#[test]
fn fixture_manifests_are_versioned_and_cite_sources() {
    for (manifest, provider) in [
        (fixtures::CODEX_V2_MANIFEST, "codex_app_server"),
        (fixtures::CLAUDE_STREAM_JSON_MANIFEST, "claude_stream_json"),
    ] {
        let value: serde_json::Value = serde_json::from_str(manifest).expect("manifest JSON");
        assert_eq!(value["fixture_version"], 1);
        assert_eq!(value["provider"], provider);
        assert!(value["sources"].as_array().unwrap().len() >= 2);
        assert!(value["capture_method"]
            .as_str()
            .unwrap()
            .contains("not a live capture"));
    }
}

#[test]
fn capability_descriptors_report_contract_version_and_unsupported_semantics() {
    let codex =
        CapabilityDescriptor::codex_app_server_v2(VersionStatus::AssumedDocumentedBaseline {
            note: "test".to_owned(),
        });
    assert_eq!(codex.contract_version, ukk_agent_adapters::CONTRACT_VERSION);
    assert!(codex.capabilities.terminal_fallback.is_supported());

    let claude =
        CapabilityDescriptor::claude_stream_json(VersionStatus::AssumedDocumentedBaseline {
            note: "test".to_owned(),
        });
    assert!(matches!(
        claude.capabilities.artifacts,
        ukk_agent_adapters::Capability::Unsupported { .. }
    ));
    assert!(matches!(
        claude.capabilities.plans,
        ukk_agent_adapters::Capability::Unsupported { .. }
    ));
}
