//! Manual acceptance against a real herdr-core.
//!
//! Ignored by default: CI uses `mock_hc`. Run on a machine with a verified
//! hc 0.1.18 binary:
//!
//! ```text
//! UKK_HC_BIN=/path/to/hc cargo test -p ukk-hc-client --test real_daemon \
//!   -- --ignored --nocapture
//! ```
//!
//! The test creates exactly one terminal with a unique name, touches only
//! that address, and always stops/removes it. It never inspects or signals an
//! existing terminal.

use std::env;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use ukk_hc_client::{
    AttachRequest, EventFilter, HcClient, RunRequest, StreamEvent, TerminalAddress, TerminalState,
};

fn binary() -> std::ffi::OsString {
    env::var_os("UKK_HC_BIN").unwrap_or_else(|| "hc".into())
}

fn unique_name() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default();
    format!("ukk-r1-{}-{millis}", std::process::id())
}

#[test]
#[ignore = "requires a real herdr-core; see docs/subsystems/host-runtime.md"]
fn real_core_launch_attach_reconnect_interrupt_stop_remove() {
    let client = HcClient::new(binary());
    let name = unique_name();
    let connection = client.framed().expect("real hc framed handshake");
    assert_eq!(connection.hello().generation, 1);
    assert!(connection.hello().supports("terminal.attach"));

    let run = connection
        .terminal_run(&RunRequest {
            argv: vec!["sleep".into(), "30".into()],
            name: Some(name.clone()),
            cwd: Some(std::env::temp_dir().to_string_lossy().into_owned()),
            ..RunRequest::default()
        })
        .expect("launch a terminal we own");
    let address = run.terminal.address.clone();
    assert_ne!(address.as_str(), "t1", "never touch pre-existing terminals");

    let cleanup = || {
        let fresh = client.framed().expect("cleanup connection");
        let _ = fresh.terminal_kill(&address);
        let _ = fresh.terminal_remove(&address);
    };

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // First attachment: receive the first paint and detach locally.
        let attachment = connection
            .attach(&AttachRequest::observe(address.clone(), 80, 24))
            .expect("attach to our terminal");
        let first = attachment
            .next_event(Duration::from_secs(10))
            .expect("first paint");
        assert!(
            matches!(first, StreamEvent::Data(_) | StreamEvent::Resize { .. }),
            "unexpected first event: {first:?}"
        );
        attachment.detach().expect("detach view");

        // Closing the client connection must not stop the program. A new
        // connection reattaches to the same address and receives a fresh paint.
        connection.close().expect("close client");
        let second = client.framed().expect("reconnect");
        let reattached = second
            .attach(&AttachRequest::observe(address.clone(), 80, 24))
            .expect("reattach after client reconnect");
        let paint = reattached
            .next_event(Duration::from_secs(10))
            .expect("first paint after reconnect");
        assert!(
            matches!(paint, StreamEvent::Data(_) | StreamEvent::Resize { .. }),
            "unexpected reattach event: {paint:?}"
        );
        reattached.detach().expect("detach reattached view");

        // Event subscription is optional but proves the typed channel path.
        let events = second
            .subscribe_events(&EventFilter::default())
            .expect("event subscription");
        drop(events);

        // Interrupt is non-escalating: sleep has no OSC 7501 status, so the
        // core presses ctrl+c. Do not assert the observed outcome.
        let interrupt = second
            .terminal_interrupt(&ukk_hc_client::InterruptRequest {
                terminal: address.clone(),
                timeout: Some(5.0),
            })
            .expect("interrupt");
        assert!(!interrupt.key.is_empty());

        // Explicit stop is the only program-end call.
        let stopped = second
            .terminal_kill(&address)
            .expect("explicit stop of our terminal");
        assert_eq!(stopped.state, TerminalState::Finished);

        let removed = second
            .terminal_remove(&address)
            .expect("remove our finished record");
        assert_eq!(removed.removed, address.as_str());
    }));

    if result.is_err() {
        cleanup();
    }
    result.unwrap();
}

#[test]
#[ignore = "requires a real herdr-core; see docs/subsystems/host-runtime.md"]
fn real_core_reports_the_expected_schema_identity() {
    let client = HcClient::new(binary());
    let connection = client.framed().expect("real hc framed handshake");
    let hello = connection.hello();
    assert_eq!(hello.version, "0.1.18");
    assert!(hello.supports("terminal.run"));
    assert!(hello.supports("terminal.attach"));
    assert!(hello.supports("terminal.send"));
    assert!(hello.supports("terminal.interrupt"));
    assert!(hello.supports("terminal.kill"));
    assert!(hello.supports("terminal.rm"));
    assert!(hello.supports("events.subscribe"));
    assert!(hello.supports_feature("attach"));
    assert!(hello.supports_feature("automatic_input"));
    let _ = TerminalAddress::parse("t7").unwrap();
}
