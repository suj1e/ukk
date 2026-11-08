//! Real omp smoke tests. Ignored by default: they need a logged-in omp binary
//! and model access. Run explicitly on a developer machine:
//!
//! ```text
//! cargo test -p ukk-omp-client --test real_omp -- --ignored --nocapture
//! ```
//!
//! The tests use an ephemeral `--no-session` child in a temporary directory,
//! print the structured frames they observed, and always shut the child down.

use std::time::{Duration, Instant};
use ukk_omp_client::{Event, EventKind, EventReceiver, OmpClient, OmpConfig, OmpState};

fn real_config(cwd: &std::path::Path) -> OmpConfig {
    OmpConfig::default()
        .with_cwd(cwd)
        .with_startup_timeout(Duration::from_secs(90))
        .with_request_timeout(Duration::from_secs(60))
}

fn scratch_dir(name: &str) -> std::path::PathBuf {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "ukk-omp-smoke-{name}-{}-{unique}",
        std::process::id()
    ));
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn collect_until_terminal(
    events: &EventReceiver,
    timeout: Duration,
    mut on_event: impl FnMut(&Event),
) -> Vec<Event> {
    let deadline = Instant::now() + timeout;
    let mut collected: Vec<Event> = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(
            !remaining.is_zero(),
            "timed out waiting for a terminal agent_end; saw {:?}",
            collected
                .iter()
                .map(|event| &event.kind)
                .collect::<Vec<_>>()
        );
        let event = events.recv_timeout(remaining).expect("omp stdout");
        on_event(&event);
        let terminal = event.is_turn_terminal();
        collected.push(event);
        if terminal {
            return collected;
        }
    }
}

fn assistant_text(events: &[Event]) -> String {
    let mut text = String::new();
    for event in events {
        if let EventKind::MessageUpdate {
            delta_kind, delta, ..
        } = &event.kind
        {
            if delta_kind == "text_delta" {
                text.push_str(delta);
            }
        }
    }
    text
}

#[test]
#[ignore = "requires a logged-in real omp binary and model access"]
fn real_omp_text_turn_round_trips_through_the_typed_client() {
    let cwd = scratch_dir("text");
    let client = OmpClient::spawn(real_config(&cwd)).expect("real omp should start");
    let ready = client.ready().expect("ready");
    eprintln!(
        "ready: protocol={} supported={:?} maxFrame={} maxReassembled={}",
        ready.protocol_version,
        ready.supported_protocol_versions,
        ready.max_frame_bytes,
        ready.max_reassembled_frame_bytes
    );
    let events = client.subscribe();
    let ack = client
        .prompt("Reply with exactly: OMP-OK")
        .expect("prompt acknowledged");
    eprintln!("prompt ack: {ack:?}");
    let collected = collect_until_terminal(&events, Duration::from_secs(240), |event| {
        eprintln!("event[{}]: {:?}", event.sequence, event.kind);
    });
    let text = assistant_text(&collected);
    eprintln!("assistant text: {text:?}");
    assert!(
        text.contains("OMP-OK"),
        "unexpected assistant text: {text:?}"
    );
    assert!(matches!(
        collected.last().unwrap().kind,
        EventKind::AgentEnd { is_terminal: true }
    ));
    client.shutdown();
    assert!(matches!(client.state(), OmpState::Exited { .. }));
    let _ = std::fs::remove_dir_all(&cwd);
}

#[test]
#[ignore = "requires a logged-in real omp binary and model access"]
fn real_omp_bash_tool_turn_emits_typed_tool_events() {
    let cwd = scratch_dir("tool");
    let client = OmpClient::spawn(real_config(&cwd)).expect("real omp should start");
    let events = client.subscribe();
    client
        .prompt(
            "Use the bash tool to run `printf OMP-TOOL-OK` in the current directory, \
             then reply with exactly: TOOL-DONE",
        )
        .expect("prompt acknowledged");
    let collected = collect_until_terminal(&events, Duration::from_secs(300), |event| {
        eprintln!("event[{}]: {:?}", event.sequence, event.kind);
    });
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
    eprintln!("tool events: {tool_events:#?}");
    assert!(
        collected.iter().any(|event| matches!(
            &event.kind,
            EventKind::ToolExecutionStart(tool) if tool.tool_name.as_deref() == Some("bash")
        )),
        "no typed bash tool_execution_start observed; events: {:?}",
        collected
            .iter()
            .map(|event| &event.kind)
            .collect::<Vec<_>>()
    );
    assert!(collected
        .iter()
        .any(|event| matches!(&event.kind, EventKind::ToolExecutionEnd(_))));
    assert!(matches!(
        collected.last().unwrap().kind,
        EventKind::AgentEnd { is_terminal: true }
    ));
    client.shutdown();
    assert!(matches!(client.state(), OmpState::Exited { .. }));
    let _ = std::fs::remove_dir_all(&cwd);
}
