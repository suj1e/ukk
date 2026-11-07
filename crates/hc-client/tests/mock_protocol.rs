//! A black-box test of the hc connect JSON-line transport without requiring
//! the real daemon in CI.
//!
//! The mock is a compiled fixture binary (`CARGO_BIN_EXE_mock_hc`), not a
//! script written at runtime. Writing an executable and exec'ing it from
//! parallel test threads is racy on Linux: another thread's `fork` can
//! inherit the still-open write descriptor and the kernel reports
//! `ETXTBSY` (ExecutableFileBusy). Cargo builds this fixture once, so the
//! test observes only transport behaviour.
//!
//! Real herdr-core integration remains a manual platform acceptance test.

use serde_json::{json, Value};
use ukk_hc_client::{HcClient, HcError};

fn mock_client() -> HcClient {
    HcClient::new(env!("CARGO_BIN_EXE_mock_hc"))
}

#[test]
fn lists_multiple_terminals_through_hc_connect() {
    let result = mock_client()
        .call("terminal.list", json!({}))
        .expect("real JSON-RPC round-trip to mock hc");
    assert_eq!(result["terminals"].as_array().unwrap().len(), 2);
    assert_eq!(result["terminals"][1]["name"], "claude");
}

#[test]
fn watches_snapshot_event_and_missed() {
    let mut received: Vec<Value> = Vec::new();
    let final_result = mock_client().watch(|message| {
        received.push(message);
        Ok(())
    });
    // A finite mock closes stdout. A live core should stream indefinitely.
    assert!(
        matches!(final_result, Err(HcError::Protocol(_))),
        "unexpected notification-stream termination: {final_result:?}; received: {received:?}"
    );
    assert_eq!(received.len(), 3);
    assert_eq!(received[0]["kind"], "snapshot");
    assert_eq!(received[0]["data"]["seq"], 10);
    assert_eq!(received[1]["data"]["method"], "event");
    assert_eq!(received[2]["data"]["method"], "missed");
}

/// Repeated subscriptions used to be the flaky path. Keep several sequential
/// and concurrent streams in the suite so a future fixture regression is
/// caught on every platform rather than only on Linux.
#[test]
fn repeated_watch_streams_terminate_deterministically() {
    for _ in 0..8 {
        let mut watched = 0_usize;
        let result = mock_client().watch(|_| {
            watched += 1;
            Ok(())
        });
        assert!(matches!(result, Err(HcError::Protocol(_))));
        assert_eq!(watched, 3);
    }
}
