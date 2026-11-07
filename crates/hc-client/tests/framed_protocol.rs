//! Black-box tests against the compiled `mock_hc` fixture.
//!
//! These exercise the real child-process boundary: hello negotiation, framed
//! launch/attach/send/interrupt/stop/remove, sequence gaps, truncation,
//! oversized frames, reconnection snapshots and idempotency. The same tests
//! run on macOS, Linux and Windows because Cargo builds the fixture binary.

use serde_json::json;
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::time::{Duration, Instant};
use ukk_hc_client::{
    AttachRequest, EventFilter, FramedConnection, HcClient, HcError, InterruptRequest, RunRequest,
    SendRequest, StreamEvent, StreamNotice, TerminalAddress, TerminalState,
};

const SHORT: Duration = Duration::from_secs(3);

fn client() -> HcClient {
    HcClient::new(env!("CARGO_BIN_EXE_mock_hc"))
}

fn connection() -> ukk_hc_client::FramedConnection {
    client().framed().expect("mock hc negotiates framed mode")
}

fn address(raw: &str) -> TerminalAddress {
    TerminalAddress::parse(raw).expect("valid fixture address")
}

#[test]
fn launch_attach_send_interrupt_stop_and_remove_against_mock_core() {
    let connection = connection();
    assert_eq!(connection.core_id(), "mock-core");
    assert!(connection.hello().supports("terminal.attach"));

    let run = connection
        .terminal_run(&RunRequest {
            argv: vec!["sleep".into(), "60".into()],
            name: Some("probe-1".into()),
            cwd: Some("/mock".into()),
            keep: true,
            ..RunRequest::default()
        })
        .expect("launch");
    assert_eq!(run.terminal.address.as_str(), "t7");
    assert!(run.terminal.is_live());

    let attachment = connection
        .attach(&AttachRequest::observe(address("t7"), 80, 24))
        .expect("attach");
    assert!(attachment.automatic_input());
    assert!(attachment.resize_notifications());
    assert_eq!(
        attachment.next_event(SHORT).unwrap(),
        StreamEvent::Data(b"first paint".to_vec())
    );
    assert_eq!(
        attachment.next_event(SHORT).unwrap(),
        StreamEvent::Resize {
            cols: 100,
            rows: 40
        }
    );
    assert_eq!(
        attachment.next_event(SHORT).unwrap(),
        StreamEvent::Data(b"live output".to_vec())
    );
    attachment.detach().expect("detach stream");

    // Detach is local: the connection and the program remain usable.
    assert!(connection.server_status().is_ok());

    let send = connection
        .terminal_send(&SendRequest::new(address("t7"), "continue"))
        .expect("send");
    assert_eq!(send.turn, 5);

    let interrupt = connection
        .terminal_interrupt(&InterruptRequest {
            terminal: address("t7"),
            timeout: Some(1.0),
        })
        .expect("interrupt");
    assert_eq!(interrupt.outcome, ukk_hc_client::InterruptOutcome::Stopped);

    let stopped = connection
        .terminal_kill(&address("t7"))
        .expect("explicit stop");
    assert_eq!(stopped.state, TerminalState::Finished);
    assert_eq!(stopped.reason, Some(ukk_hc_client::TerminalReason::Killed));

    let removed = connection
        .terminal_remove(&address("t7"))
        .expect("remove finished record");
    assert_eq!(removed.removed, "t7");
}

#[test]
fn attachment_sends_human_input_as_data_and_refuses_observer_input() {
    let connection = connection();
    let observer = connection
        .attach(&AttachRequest::observe(address("t1"), 80, 24))
        .expect("observe attach");
    assert!(observer.is_observer());
    assert!(matches!(
        observer.send_input(b"x"),
        Err(HcError::Unsupported { .. })
    ));
    assert!(matches!(
        observer.resize(120, 40),
        Err(HcError::Unsupported { .. })
    ));

    let active = connection
        .attach(&AttachRequest {
            terminal: address("t2"),
            cols: 80,
            rows: 24,
            history: 0,
            observe: false,
            resize_notifications: true,
            resumed: false,
            caller: None,
            display: None,
        })
        .expect("active attach");
    // Drain the first paint and resize before sending input.
    let _ = active.next_event(SHORT).unwrap();
    let _ = active.next_event(SHORT).unwrap();
    let _ = active.next_event(SHORT).unwrap();
    active.send_input(b"hello").expect("send DATA");
    let echo = loop {
        match active.next_event(SHORT).unwrap() {
            StreamEvent::Data(bytes) => break bytes,
            _ => continue,
        }
    };
    assert_eq!(echo, b"echo:hello");
    active.detach().unwrap();
}

#[test]
fn launch_idempotency_reuses_matching_terminal_and_rejects_conflicts() {
    let connection = connection();
    let matching = connection
        .terminal_run_idempotent(&RunRequest {
            argv: vec!["codex".into()],
            name: Some("existing".into()),
            cwd: Some("/mock".into()),
            ..RunRequest::default()
        })
        .expect("reuse");
    assert!(matching.replayed);
    assert_eq!(matching.result.terminal.address.as_str(), "existing");

    let conflict = connection
        .terminal_run_idempotent(&RunRequest {
            argv: vec!["claude".into()],
            name: Some("existing".into()),
            cwd: Some("/mock".into()),
            ..RunRequest::default()
        })
        .unwrap_err();
    assert!(matches!(conflict, HcError::IdempotencyConflict(_)));

    let fresh = connection
        .terminal_run_idempotent(&RunRequest {
            argv: vec!["sleep".into(), "60".into()],
            name: Some("missing".into()),
            cwd: Some("/mock".into()),
            ..RunRequest::default()
        })
        .expect("launch new");
    assert!(!fresh.replayed);
    assert_eq!(fresh.result.terminal.address.as_str(), "t7");
}

#[test]
fn send_idempotency_does_not_repeat_user_input() {
    let connection = connection();
    let request = SendRequest::new(address("t7"), "once");
    let first = connection
        .terminal_send_once("request-1", &request)
        .expect("first send");
    assert!(!first.replayed);
    assert_eq!(first.result.turn, 5);

    let repeated = connection
        .terminal_send_once("request-1", &request)
        .expect("deduped send");
    assert!(repeated.replayed);
    assert_eq!(repeated.result.turn, 5, "mock increments if called twice");

    let changed = connection
        .terminal_send_once("request-1", &SendRequest::new(address("t7"), "twice"))
        .unwrap_err();
    assert!(matches!(changed, HcError::IdempotencyConflict(_)));
}

#[test]
fn truncated_frame_header_closes_the_pending_request_typed() {
    let connection = connection();
    let error = connection
        .attach(&AttachRequest::observe(address("t-partial"), 80, 24))
        .unwrap_err();
    match error {
        HcError::Closed(reason) => assert!(reason.contains("truncated"), "{reason}"),
        other => panic!("expected typed close, got {other:?}"),
    }
}

#[test]
fn oversized_frame_is_rejected_without_waiting_for_payload() {
    let connection = connection();
    let error = connection
        .attach(&AttachRequest::observe(address("t-oversized"), 80, 24))
        .unwrap_err();
    match error {
        HcError::Closed(reason) => assert!(reason.contains("exceeds limit"), "{reason}"),
        other => panic!("expected typed close, got {other:?}"),
    }
}

#[test]
fn unfiltered_event_stream_reports_a_sequence_gap() {
    let connection = connection();
    let subscription = connection
        .subscribe_events(&EventFilter::default())
        .expect("subscribe");
    assert_eq!(subscription.snapshot_seq(), 10);
    match subscription.next_notice(SHORT).unwrap() {
        StreamNotice::Event(event) => assert_eq!(event.seq, 11),
        other => panic!("expected event, got {other:?}"),
    }
    let gap = subscription.next_notice(SHORT).unwrap_err();
    assert!(matches!(
        gap,
        HcError::SequenceGap {
            expected: 12,
            got: 13
        }
    ));
    match subscription.next_notice(SHORT).unwrap() {
        StreamNotice::Missed(missed) => assert_eq!(missed.seq, 20),
        other => panic!("expected missed, got {other:?}"),
    }
}

#[test]
fn filtered_event_stream_accepts_skipped_sequence_numbers() {
    let connection = connection();
    let subscription = connection
        .subscribe_events(&EventFilter {
            terminal: Some(address("t1")),
            labels: BTreeMap::new(),
            children_of: None,
        })
        .expect("filtered subscribe");
    match subscription.next_notice(SHORT).unwrap() {
        StreamNotice::Event(event) => assert_eq!(event.seq, 11),
        other => panic!("expected event, got {other:?}"),
    }
    match subscription.next_notice(SHORT).unwrap() {
        StreamNotice::Event(event) => assert_eq!(event.seq, 13),
        other => panic!("expected event, got {other:?}"),
    }
}

#[test]
fn reconnection_uses_the_new_snapshot_instead_of_reporting_a_false_gap() {
    let first = connection();
    let first_subscription = first
        .subscribe_events(&EventFilter::default())
        .expect("first subscribe");
    match first_subscription.next_notice(SHORT).unwrap() {
        StreamNotice::Event(event) => assert_eq!(event.seq, 11),
        other => panic!("expected event, got {other:?}"),
    }
    drop(first_subscription);
    drop(first);

    let second = connection();
    let second_subscription = second
        .subscribe_events(&EventFilter {
            terminal: Some(address("t-lower")),
            labels: BTreeMap::new(),
            children_of: None,
        })
        .expect("second subscribe");
    assert_eq!(second_subscription.snapshot_seq(), 2);
    match second_subscription.next_notice(SHORT).unwrap() {
        StreamNotice::Event(event) => assert_eq!(event.seq, 3),
        other => panic!("expected event after restart, got {other:?}"),
    }
}

#[test]
fn framed_handshake_times_out_and_reaps_a_wedged_core() {
    let started = Instant::now();
    let error = FramedConnection::connect_with_timeout(
        OsStr::new(env!("CARGO_BIN_EXE_mock_hc_hang")),
        Duration::from_millis(200),
    )
    .expect_err("a core that never answers hello must not block");
    assert!(matches!(error, HcError::Timeout { .. }), "{error:?}");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "hello deadline must be bounded"
    );
}

#[test]
fn concurrent_send_once_calls_reach_the_core_exactly_once() {
    let connection = connection();
    let request = SendRequest::new(address("t7"), "same input");
    let (first, second) = std::thread::scope(|scope| {
        let first = scope.spawn(|| connection.terminal_send_once("concurrent", &request));
        let second = scope.spawn(|| connection.terminal_send_once("concurrent", &request));
        (first.join().unwrap(), second.join().unwrap())
    });
    let first = first.expect("first send");
    let second = second.expect("second send");
    assert_eq!(first.result.turn, 5);
    assert_eq!(
        second.result.turn, 5,
        "a second call must not reach the core"
    );
    assert!(
        first.replayed ^ second.replayed,
        "exactly one caller must be a replay"
    );
}

#[test]
fn read_chunk_returns_credit_and_the_core_dribbles_the_next_frame() {
    let connection = connection();
    let attachment = connection
        .attach(&AttachRequest {
            terminal: address("t-credit"),
            cols: 80,
            rows: 24,
            history: 0,
            observe: false,
            resize_notifications: false,
            resumed: false,
            caller: None,
            display: None,
        })
        .expect("attach");
    assert_eq!(attachment.read_chunk(SHORT).unwrap(), b"first paint");
    assert_eq!(attachment.read_chunk(SHORT).unwrap(), b"after credit");
    attachment.detach().unwrap();
}

#[test]
fn one_shot_call_times_out_and_reaps_a_hung_child() {
    let client = HcClient::with_timeout(
        env!("CARGO_BIN_EXE_mock_hc_hang"),
        Duration::from_millis(300),
    );
    let started = Instant::now();
    let error = client
        .call("terminal.list", json!({}))
        .expect_err("hung core must not block forever");
    assert!(matches!(error, HcError::Timeout { .. }), "{error:?}");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "timeout must be bounded"
    );
}
