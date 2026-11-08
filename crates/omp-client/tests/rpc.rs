//! Deterministic integration tests against the mock RPC child.

use std::time::{Duration, Instant};
use ukk_omp_client::{
    Event, EventKind, EventReceiver, OmpClient, OmpConfig, OmpError, OmpState, OmpTimelineKind,
};

fn mock_config(scenario: &str) -> OmpConfig {
    OmpConfig::default()
        .with_binary(env!("CARGO_BIN_EXE_ukk-omp-mock-rpc"))
        .with_args(["--scenario", scenario])
        .with_startup_timeout(Duration::from_secs(5))
        .with_request_timeout(Duration::from_secs(5))
}

fn spawn(scenario: &str) -> (OmpClient, EventReceiver) {
    let client = OmpClient::spawn(mock_config(scenario)).expect("mock child should start");
    let events = client.subscribe();
    (client, events)
}

fn collect_until_terminal(events: &EventReceiver, timeout: Duration) -> Vec<Event> {
    let deadline = Instant::now() + timeout;
    let mut collected: Vec<Event> = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(
            !remaining.is_zero(),
            "timed out waiting for a terminal turn; got {:?}",
            collected
                .iter()
                .map(|event| &event.kind)
                .collect::<Vec<_>>()
        );
        let event = events
            .recv_timeout(remaining)
            .expect("mock child should keep its stdout open");
        let terminal = event.is_turn_terminal();
        collected.push(event);
        if terminal {
            return collected;
        }
    }
}

fn text_deltas(events: &[Event]) -> String {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::MessageUpdate {
                delta_kind, delta, ..
            } if delta_kind == "text_delta" => Some(delta.as_str()),
            _ => None,
        })
        .collect()
}

fn is_kind(events: &[Event], predicate: impl Fn(&EventKind) -> bool) -> bool {
    events.iter().any(|event| predicate(&event.kind))
}

#[test]
fn ready_handshake_and_protocol_limits_are_recorded() {
    let (client, _events) = spawn("text");
    let ready = client.ready().expect("ready must be retained");
    assert_eq!(ready.protocol_version, 1);
    assert!(ready.supported_protocol_versions.contains(&1));
    assert_eq!(ready.max_frame_bytes, 1_048_576);
    assert!(client.state().is_running());
    client.shutdown();
    assert!(matches!(client.state(), OmpState::Exited { .. }));
}

#[test]
fn prompt_streams_text_and_completes_on_terminal_agent_end() {
    let (client, events) = spawn("text");
    let ack = client.prompt("Reply with exactly: OMP-OK").unwrap();
    assert_eq!(ack.agent_invoked, Some(true));

    let collected = collect_until_terminal(&events, Duration::from_secs(5));
    assert_eq!(text_deltas(&collected), "OMP-MOCK");
    assert!(is_kind(&collected, |kind| matches!(
        kind,
        EventKind::AgentStart
    )));
    assert!(is_kind(&collected, |kind| matches!(
        kind,
        EventKind::TurnStart
    )));
    assert!(is_kind(&collected, |kind| matches!(
        kind,
        EventKind::MessageStart(message) if message.role == "assistant"
    )));
    assert!(is_kind(&collected, |kind| matches!(
        kind,
        EventKind::MessageEnd(message) if message.text.as_deref() == Some("OMP-MOCK")
    )));
    let terminal = collected.last().unwrap();
    assert!(matches!(
        terminal.kind,
        EventKind::AgentEnd { is_terminal: true }
    ));
    // Widget updates are fire-and-forget and are recorded, never answered.
    let widget: Vec<_> = collected
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::ExtensionUi {
                method,
                requires_response,
                ..
            } => Some((method.as_str(), *requires_response)),
            _ => None,
        })
        .collect();
    assert!(widget.iter().any(|(method, _)| *method == "setWidget"));
    assert!(widget.iter().all(|(_, requires)| !requires));
    client.shutdown();
}

#[test]
fn tool_execution_events_are_typed_and_ordered() {
    let (client, events) = spawn("tool");
    client.prompt("Run a harmless bash command").unwrap();
    let collected = collect_until_terminal(&events, Duration::from_secs(5));
    assert_eq!(text_deltas(&collected), "OMP-TOOL-OKMOCK");

    let tool_events: Vec<&EventKind> = collected
        .iter()
        .map(|event| &event.kind)
        .filter(|kind| {
            matches!(
                kind,
                EventKind::ToolExecutionStart(_)
                    | EventKind::ToolExecutionUpdate(_)
                    | EventKind::ToolExecutionEnd(_)
            )
        })
        .collect();
    assert_eq!(tool_events.len(), 3, "{tool_events:?}");
    match tool_events[0] {
        EventKind::ToolExecutionStart(tool) => {
            assert_eq!(tool.tool_name.as_deref(), Some("bash"));
            assert_eq!(tool.tool_call_id.as_deref(), Some("tool-1"));
            assert!(tool.text.as_deref().unwrap().contains("echo OMP-TOOL"));
        }
        other => panic!("expected tool start, got {other:?}"),
    }
    match tool_events[1] {
        EventKind::ToolExecutionUpdate(tool) => {
            assert_eq!(tool.text.as_deref(), Some("OMP-TOOL\n"));
        }
        other => panic!("expected tool update, got {other:?}"),
    }
    match tool_events[2] {
        EventKind::ToolExecutionEnd(tool) => {
            assert_eq!(tool.text.as_deref(), Some("OMP-TOOL\n"));
            assert_eq!(tool.is_error, Some(false));
        }
        other => panic!("expected tool end, got {other:?}"),
    }
    client.shutdown();
}

#[test]
fn local_command_completion_does_not_require_agent_end() {
    let (client, events) = spawn("local");
    let ack = client.prompt("/status").unwrap();
    assert_eq!(ack.agent_invoked, Some(false));
    let collected = collect_until_terminal(&events, Duration::from_secs(5));
    assert!(collected.iter().any(|event| matches!(
        &event.kind,
        EventKind::PromptResult {
            agent_invoked: false,
            ..
        }
    )));
    assert!(collected.iter().any(
        |event| matches!(&event.kind, EventKind::CommandOutput { text } if text == "local handled")
    ));
    assert!(!collected
        .iter()
        .any(|event| matches!(event.kind, EventKind::AgentEnd { .. })));
    client.shutdown();
}

#[test]
fn rejection_is_a_typed_command_error() {
    let (client, _events) = spawn("reject");
    let error = client.prompt("hi").expect_err("prompt must fail");
    match error {
        OmpError::CommandRejected {
            ref command,
            ref message,
            ref code,
        } => {
            assert_eq!(command, "prompt");
            assert!(message.contains("Model not found"));
            assert_eq!(code.as_deref(), Some("model_not_found"));
        }
        other => panic!("unexpected error: {other:?}"),
    }
    client.shutdown();
}

#[test]
fn malformed_json_frame_is_reported_and_the_stream_continues() {
    let (client, events) = spawn("malformed");
    client.prompt("hi").unwrap();
    let collected = collect_until_terminal(&events, Duration::from_secs(5));
    assert!(collected
        .iter()
        .any(|event| matches!(&event.kind, EventKind::MalformedFrame { .. })));
    assert!(matches!(
        collected.last().unwrap().kind,
        EventKind::AgentEnd { is_terminal: true }
    ));
    client.shutdown();
}

#[test]
fn oversized_frame_is_rejected_honestly_and_does_not_break_the_stream() {
    let (client, events) = spawn("oversized");
    client.prompt("hi").unwrap();
    let collected = collect_until_terminal(&events, Duration::from_secs(5));
    let oversized: Vec<_> = collected
        .iter()
        .filter_map(|event| match event.kind {
            EventKind::OversizedFrame { limit } => Some(limit),
            _ => None,
        })
        .collect();
    assert_eq!(oversized, vec![65_536]);
    assert!(matches!(
        collected.last().unwrap().kind,
        EventKind::AgentEnd { is_terminal: true }
    ));
    client.shutdown();
}

#[test]
fn abort_is_correlated_even_when_responses_arrive_out_of_order() {
    let (client, _events) = spawn("out-of-order");
    let client = std::sync::Arc::new(client);
    let prompt_client = std::sync::Arc::clone(&client);
    let prompt = std::thread::spawn(move || prompt_client.prompt("slow prompt"));
    std::thread::sleep(Duration::from_millis(50));
    let started = Instant::now();
    let abort = client.abort().unwrap();
    assert_eq!(abort.command, "abort");
    assert!(
        started.elapsed() < Duration::from_millis(300),
        "abort must not wait for the slower prompt response"
    );
    let ack = prompt.join().unwrap().unwrap();
    assert_eq!(ack.agent_invoked, Some(true));
    client.shutdown();
}

#[test]
fn request_timeout_is_typed_and_does_not_kill_a_healthy_child() {
    let config = mock_config("no-response").with_request_timeout(Duration::from_millis(250));
    let client = OmpClient::spawn(config).unwrap();
    client.subscribe();
    let error = client.prompt("no answer").expect_err("must time out");
    assert!(error.is_timeout(), "{error:?}");
    assert!(client.state().is_running());
    client.kill().unwrap();
    assert!(matches!(client.state(), OmpState::Exited { .. }));
}

#[test]
fn unexpected_child_exit_fails_the_pending_prompt() {
    let (client, _events) = spawn("exit-before-ack");
    let error = client.prompt("exit now").expect_err("child exits");
    assert!(
        matches!(
            error,
            OmpError::Transport(_) | OmpError::NotRunning(_) | OmpError::Io(_)
        ),
        "{error:?}"
    );
    let deadline = Instant::now() + Duration::from_secs(3);
    while client.state().is_running() {
        assert!(Instant::now() < deadline, "state never observed exit");
        std::thread::sleep(Duration::from_millis(10));
    }
    match client.state() {
        OmpState::Exited { code, .. } => assert_eq!(code, Some(3)),
        other => panic!("unexpected state {other:?}"),
    }
}

#[test]
fn startup_timeout_kills_and_reaps_the_child() {
    let config = mock_config("no-ready").with_startup_timeout(Duration::from_secs(2));
    let error = OmpClient::spawn(config).expect_err("ready never arrives");
    let stderr_tail = match error {
        OmpError::StartupTimeout { stderr_tail, .. } => stderr_tail,
        other => panic!("unexpected error: {other:?}"),
    };
    let pid_line = stderr_tail
        .iter()
        .find(|line| line.starts_with("MOCK-PID "))
        .expect("mock reports its pid on stderr");
    let pid = pid_line.trim_start_matches("MOCK-PID ").trim().to_owned();
    #[cfg(unix)]
    {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let alive = std::process::Command::new("/bin/kill")
                .args(["-0", &pid])
                .status()
                .map(|status| status.success())
                .unwrap_or(false);
            if !alive {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "timed-out child {pid} was not reaped"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

#[test]
fn reader_failure_kills_and_reaps_the_child() {
    let client = OmpClient::spawn(mock_config("flood")).expect("flood child becomes ready");
    let _events = client.subscribe();
    let deadline = Instant::now() + Duration::from_secs(10);
    while client.state().is_running() {
        assert!(
            Instant::now() < deadline,
            "reader failure never stopped the child"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(matches!(client.state(), OmpState::Exited { .. }));
    let pid_line = client
        .stderr_tail()
        .into_iter()
        .find(|line| line.starts_with("MOCK-PID "))
        .expect("mock reports its pid");
    let pid = pid_line.trim_start_matches("MOCK-PID ").trim().to_owned();
    #[cfg(unix)]
    {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let alive = std::process::Command::new("/bin/kill")
                .args(["-0", &pid])
                .status()
                .map(|status| status.success())
                .unwrap_or(false);
            if !alive {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "reader failure left child {pid} unreaped"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

#[test]
fn stderr_is_separate_redacted_and_never_becomes_an_event() {
    let (client, events) = spawn("stderr");
    client.prompt("hi").unwrap();
    let collected = collect_until_terminal(&events, Duration::from_secs(5));
    for event in &collected {
        let rendered = format!("{:?}", event.kind);
        assert!(
            !rendered.contains("mock stderr"),
            "stderr leaked into the protocol events: {rendered}"
        );
    }
    let tail = client.stderr_tail();
    assert!(tail
        .iter()
        .any(|line| line.contains("mock stderr line one")));
    assert!(
        tail.iter()
            .any(|line| line.contains("mock stderr line two") && line.contains("sk-REDACTED")),
        "stderr tail must redact provider tokens: {tail:?}"
    );
    client.shutdown();
}

#[test]
fn ui_requests_that_expect_a_value_are_cancelled_without_deadlock() {
    let (client, events) = spawn("ui-confirm");
    client.prompt("confirm something").unwrap();
    let collected = collect_until_terminal(&events, Duration::from_secs(5));
    assert!(collected.iter().any(|event| matches!(
        &event.kind,
        EventKind::ExtensionUi {
            method,
            requires_response: true,
            ..
        } if method == "confirm"
    )));
    assert!(collected.iter().any(|event| matches!(
        &event.kind,
        EventKind::Notice { message, .. } if message == "ui request cancelled"
    )));
    client.shutdown();
}

#[test]
fn steer_and_follow_up_carry_their_typed_payloads() {
    let (client, _events) = spawn("commands");
    let steer = client.steer("insert this").unwrap();
    assert_eq!(steer.command, "steer");
    assert_eq!(
        steer.data.as_ref().unwrap()["message"],
        serde_json::json!("insert this")
    );
    let follow_up = client.follow_up("after this").unwrap();
    assert_eq!(follow_up.command, "follow_up");
    assert_eq!(
        follow_up.data.as_ref().unwrap()["message"],
        serde_json::json!("after this")
    );
    client.shutdown();
}

#[test]
fn events_before_the_ack_still_complete_the_turn() {
    let (client, events) = spawn("delayed-ack");
    let ack = client.prompt("early event").unwrap();
    assert_eq!(ack.agent_invoked, Some(true));
    let collected = collect_until_terminal(&events, Duration::from_secs(5));
    assert_eq!(text_deltas(&collected), "EARLYOMP-MOCK");
    client.shutdown();
}

#[test]
fn shutdown_is_graceful_and_idempotent() {
    let (client, _events) = spawn("text");
    let state = client.state();
    assert!(state.is_running());
    client.shutdown();
    client.shutdown();
    assert!(matches!(client.state(), OmpState::Exited { .. }));
}

#[test]
fn missing_binary_is_a_spawn_error() {
    let config = OmpConfig::default()
        .with_binary("/nonexistent/ukk-omp-binary")
        .with_args(["--mode", "rpc"]);
    let error = OmpClient::spawn(config).expect_err("binary does not exist");
    assert!(matches!(error, OmpError::Spawn { .. }), "{error:?}");
}

/// Timeline kinds are part of the public Host/Desktop contract; this keeps
/// the enum's wire spelling stable.
#[test]
fn timeline_kinds_have_stable_wire_names() {
    assert_eq!(
        serde_json::to_string(&OmpTimelineKind::Assistant).unwrap(),
        "\"assistant\""
    );
    assert_eq!(
        serde_json::to_string(&OmpTimelineKind::Tool).unwrap(),
        "\"tool\""
    );
}
