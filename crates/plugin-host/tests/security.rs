//! Security-negative tests: default-deny grants, forged and unknown commands,
//! manifest/hello forgery, crash containment, deadlines, frame bounds and
//! bounded event queues.
mod common;

use std::time::{Duration, Instant};

use common::*;
use serde_json::json;
use ukk_plugin_api::{Capability, ClientPlatform, EventKind, PluginId};
use ukk_plugin_host::{
    AuditKind, GrantError, GrantRequest, GrantScope, HostError, HostEvent, InvokeRequest,
    PluginInstall, PluginState, MAX_SENSITIVE_GRANT_TTL_MS,
};

fn install_fault(fixture: &TestHost, mode: &str) -> PluginId {
    fixture
        .host
        .install(PluginInstall::for_helper(fault_manifest(), fault_helper(mode)).unwrap())
        .expect("install fault plugin");
    fault_id()
}

fn grant_request(
    plugin_id: &PluginId,
    capability: Capability,
    scope: GrantScope,
    ttl_ms: Option<u64>,
) -> GrantRequest {
    GrantRequest {
        plugin_id: plugin_id.clone(),
        capability,
        scope,
        granted_by: "test-user".into(),
        ttl_ms,
    }
}

fn has_audit(fixture: &TestHost, kind: AuditKind) -> bool {
    fixture.audit.kinds().contains(&kind)
}

fn assert_enable_failure(error: HostError, expected: &str) {
    match error {
        HostError::PluginFailed { code, message, .. } => {
            assert_eq!(code, expected, "unexpected enable failure: {message}")
        }
        other => panic!("expected enable failure `{expected}`, got {other:?}"),
    }
}

#[test]
fn host_grants_nothing_by_default_and_privileged_commands_are_denied() {
    let fixture = test_host_default();
    let host = &fixture.host;
    let plugin_id = install_fault(&fixture, "default");
    host.enable(&plugin_id).unwrap();

    assert!(
        host.grants(&plugin_id).is_empty(),
        "installing and enabling must never create a grant"
    );

    for command in ["fault.approve", "fault.screen"] {
        let error = host
            .invoke(InvokeRequest::new(plugin_id.clone(), command).in_session("s1"))
            .unwrap_err();
        match error {
            HostError::CapabilityDenied { capability, .. } => {
                assert!(matches!(
                    capability.as_str(),
                    "agent.approve" | "screen.view"
                ));
            }
            other => panic!("expected capability denial for {command}, got {other:?}"),
        }
    }
    // The UI can still show the declared commands, flagged as requiring grants.
    let snapshot = host.session_snapshot(ClientPlatform::Desktop);
    let approve = snapshot
        .commands
        .iter()
        .find(|command| command.command_id() == "fault.approve")
        .expect("declared command is visible");
    assert_eq!(approve.requires(), ["agent.approve"]);

    let denials = fixture
        .audit
        .records()
        .into_iter()
        .filter(|record| record.kind == AuditKind::CommandDenied)
        .count();
    assert_eq!(denials, 2, "each denial is audited");
}

#[test]
fn a_grant_authorizes_only_its_scope_and_revocation_is_immediate() {
    let fixture = test_host_default();
    let host = &fixture.host;
    let plugin_id = install_fault(&fixture, "default");
    host.enable(&plugin_id).unwrap();

    host.grant(grant_request(
        &plugin_id,
        Capability::AgentApprove,
        GrantScope::Session("session-a".into()),
        Some(60_000),
    ))
    .expect("grant session-a");

    assert!(host
        .invoke(InvokeRequest::new(plugin_id.clone(), "fault.approve").in_session("session-a"))
        .is_ok());
    let error = host
        .invoke(InvokeRequest::new(plugin_id.clone(), "fault.approve").in_session("session-b"))
        .unwrap_err();
    assert_eq!(error.code(), "capability_denied");

    assert!(host.revoke(
        &plugin_id,
        &Capability::AgentApprove,
        &GrantScope::Session("session-a".into())
    ));
    let error = host
        .invoke(InvokeRequest::new(plugin_id.clone(), "fault.approve").in_session("session-a"))
        .unwrap_err();
    assert_eq!(error.code(), "capability_denied");

    // Revoking a grant that does not exist is reported, not silently allowed.
    assert!(!host.revoke(
        &plugin_id,
        &Capability::AgentApprove,
        &GrantScope::Session("session-a".into())
    ));

    assert!(has_audit(&fixture, AuditKind::GrantGranted));
    assert!(has_audit(&fixture, AuditKind::GrantRevoked));
}

#[test]
fn sensitive_capabilities_require_a_bounded_expiry() {
    let fixture = test_host_default();
    let plugin_id = install_fault(&fixture, "default");

    let error = fixture
        .host
        .grant(grant_request(
            &plugin_id,
            Capability::ScreenView,
            GrantScope::Host,
            None,
        ))
        .unwrap_err();
    assert!(
        matches!(error, HostError::Grant(GrantError::ExpiryRequired { .. })),
        "screen grants must be leases, got {error:?}"
    );

    let error = fixture
        .host
        .grant(grant_request(
            &plugin_id,
            Capability::ScreenView,
            GrantScope::Host,
            Some(MAX_SENSITIVE_GRANT_TTL_MS + 1),
        ))
        .unwrap_err();
    assert!(matches!(
        error,
        HostError::Grant(GrantError::ExpiryOutOfRange { .. })
    ));

    let lease = fixture
        .host
        .grant(grant_request(
            &plugin_id,
            Capability::ScreenView,
            GrantScope::Host,
            Some(MAX_SENSITIVE_GRANT_TTL_MS),
        ))
        .expect("bounded lease is accepted");
    assert!(lease.expires_at_ms().is_some());
    assert!(lease.is_active(lease.granted_at_ms()));
}

#[test]
fn undeclared_capability_cannot_be_granted() {
    let fixture = test_host_default();
    let plugin_id = install_fault(&fixture, "default");
    let error = fixture
        .host
        .grant(grant_request(
            &plugin_id,
            Capability::WorkspaceRead,
            GrantScope::Host,
            None,
        ))
        .unwrap_err();
    assert_eq!(error.code(), "undeclared_capability");
    assert!(fixture.host.grants(&plugin_id).is_empty());
}

#[test]
fn forged_and_unknown_commands_are_denied() {
    let fixture = test_host_default();
    let host = &fixture.host;
    host.install(PluginInstall::for_helper(sample_manifest(), sample_helper()).unwrap())
        .unwrap();
    host.install(PluginInstall::for_helper(other_manifest(), other_helper()).unwrap())
        .unwrap();
    host.enable(&sample_id()).unwrap();
    host.enable(&other_id()).unwrap();

    // Plugin `other` tries to run the sample's command.
    let error = host
        .invoke(InvokeRequest::new(other_id(), "sample.session.ping").in_session("s"))
        .unwrap_err();
    match error {
        HostError::ForgedCommand { owner, .. } => assert_eq!(owner, SAMPLE_ID),
        other => panic!("expected forged command, got {other:?}"),
    }
    assert!(has_audit(&fixture, AuditKind::ForgedRequest));

    // A command nobody declares, and a view nobody declares.
    let error = host
        .invoke(InvokeRequest::new(other_id(), "other.nope").in_session("s"))
        .unwrap_err();
    assert_eq!(error.code(), "unknown_command");
    let error = host
        .invoke_view(&other_id(), "other.missing-view", Some("s"))
        .unwrap_err();
    assert_eq!(error.code(), "unknown_view");
    assert!(has_audit(&fixture, AuditKind::UnknownCommand));

    // The two legitimate commands still work.
    assert!(host
        .invoke(InvokeRequest::new(sample_id(), "sample.session.ping").in_session("s"))
        .is_ok());
    assert!(host
        .invoke(InvokeRequest::new(other_id(), "other.echo").in_session("s"))
        .is_ok());
}

#[test]
fn session_scoped_commands_require_a_session() {
    let fixture = test_host_default();
    let host = &fixture.host;
    host.install(PluginInstall::for_helper(sample_manifest(), sample_helper()).unwrap())
        .unwrap();
    host.enable(&sample_id()).unwrap();

    let error = host
        .invoke(InvokeRequest::new(sample_id(), "sample.session.ping"))
        .unwrap_err();
    assert_eq!(error.code(), "missing_session");
}

#[test]
fn helper_crash_is_contained_and_other_plugins_keep_working() {
    let fixture = test_host_default();
    let host = &fixture.host;
    let crashing = install_fault(&fixture, "crash");
    host.install(PluginInstall::for_helper(sample_manifest(), sample_helper()).unwrap())
        .unwrap();
    host.enable(&crashing).unwrap();
    host.enable(&sample_id()).unwrap();

    let error = host
        .invoke(InvokeRequest::new(crashing.clone(), "fault.ping").in_session("s"))
        .unwrap_err();
    assert_eq!(error.code(), "crashed");
    assert_eq!(host.status(&crashing).unwrap().state.as_str(), "failed");
    assert!(host.helper_pid(&crashing).is_none());
    assert!(has_audit(&fixture, AuditKind::HelperCrashed));

    // The host and every other plugin are unaffected.
    assert_eq!(host.status(&sample_id()).unwrap().state, PluginState::Ready);
    assert!(host
        .invoke(InvokeRequest::new(sample_id(), "sample.session.ping").in_session("s"))
        .is_ok());
    assert!(
        host.invoke(InvokeRequest::new(crashing.clone(), "fault.ping").in_session("s"))
            .unwrap_err()
            .code()
            == "not_enabled"
    );

    // Recovery is explicit, and works without restarting the host.
    assert_eq!(host.restart(&crashing).unwrap().state, PluginState::Ready);
}

#[test]
fn helper_timeout_is_bounded_and_marks_the_plugin_failed() {
    warm_up(&fault_helper("default"));
    let fixture = test_host(short_timeout_config(500));
    let host = &fixture.host;
    let plugin_id = install_fault(&fixture, "stall");
    host.enable(&plugin_id).unwrap();

    let started = Instant::now();
    let error = host
        .invoke(InvokeRequest::new(plugin_id.clone(), "fault.ping").in_session("s"))
        .unwrap_err();
    assert_eq!(error.code(), "timeout");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "a hung helper must not block dispatch"
    );
    assert_eq!(host.status(&plugin_id).unwrap().state.as_str(), "failed");
    assert!(has_audit(&fixture, AuditKind::HelperTimeout));
}

#[test]
fn oversize_helper_output_is_cut_off() {
    warm_up(&fault_helper("default"));
    let mut config = short_timeout_config(500);
    config.max_frame_bytes = 4096;
    let fixture = test_host(config);
    let plugin_id = install_fault(&fixture, "oversize");
    fixture.host.enable(&plugin_id).unwrap();

    let error = fixture
        .host
        .invoke(InvokeRequest::new(plugin_id.clone(), "fault.ping").in_session("s"))
        .unwrap_err();
    assert_eq!(error.code(), "frame_too_large");
    assert_eq!(
        fixture.host.status(&plugin_id).unwrap().state.as_str(),
        "failed"
    );
    assert!(has_audit(&fixture, AuditKind::ProtocolViolation));
}

#[test]
fn non_json_and_unsolicited_frames_are_protocol_violations() {
    for mode in ["garbage", "noisy"] {
        warm_up(&fault_helper("default"));
        let fixture = test_host(short_timeout_config(500));
        let plugin_id = install_fault(&fixture, mode);
        fixture.host.enable(&plugin_id).unwrap();
        let error = fixture
            .host
            .invoke(InvokeRequest::new(plugin_id.clone(), "fault.ping").in_session("s"))
            .unwrap_err();
        assert_eq!(
            error.code(),
            "protocol_violation",
            "mode {mode} must be a protocol violation, got {error:?}"
        );
        assert_eq!(
            fixture.host.status(&plugin_id).unwrap().state.as_str(),
            "failed"
        );
        assert!(has_audit(&fixture, AuditKind::ProtocolViolation));
    }
}

#[test]
fn hello_forgery_identity_mismatch_and_handshake_timeout_are_rejected() {
    // Extra contributions the manifest never declared.
    let forged = test_host_default();
    let plugin_id = install_fault(&forged, "forged");
    let error = forged.host.enable(&plugin_id).unwrap_err();
    assert_enable_failure(error, "protocol_violation");
    assert_eq!(
        forged.host.status(&plugin_id).unwrap().state.as_str(),
        "failed"
    );
    assert!(forged.host.helper_pid(&plugin_id).is_none());
    assert!(has_audit(&forged, AuditKind::ProtocolViolation));

    // Wrong identity.
    let wrong = test_host_default();
    let plugin_id = install_fault(&wrong, "bad-identity");
    let error = wrong.host.enable(&plugin_id).unwrap_err();
    assert_enable_failure(error, "protocol_violation");
    assert!(has_audit(&wrong, AuditKind::ProtocolViolation));

    // No answer at all.
    warm_up(&fault_helper("default"));
    let silent = test_host(short_timeout_config(500));
    let plugin_id = install_fault(&silent, "hello-stall");
    let error = silent.host.enable(&plugin_id).unwrap_err();
    assert_enable_failure(error, "helper_timeout");
    assert_eq!(
        silent.host.status(&plugin_id).unwrap().state.as_str(),
        "failed"
    );
    // A failed plugin contributes nothing.
    assert!(silent
        .host
        .session_snapshot(ClientPlatform::Desktop)
        .is_empty());
}

#[test]
fn event_delivery_needs_a_grant_and_a_bounded_queue() {
    warm_up(&fault_helper("default"));
    let mut config = fast_config();
    config.event_queue_capacity = 4;
    let fixture = test_host(config);
    let host = &fixture.host;
    let plugin_id = install_fault(&fixture, "mute");
    host.enable(&plugin_id).unwrap();

    let event = || {
        HostEvent::new(
            EventKind::SessionUpdated,
            json!({ "blob": "x".repeat(8192) }),
        )
    };

    // Subscribed, ready, but no `session.observe` grant: nothing is delivered.
    let report = host.publish_event(event());
    assert_eq!(report.delivered, 0);
    assert_eq!(report.dropped, 0);
    assert_eq!(report.skipped, 1);

    host.grant(grant_request(
        &plugin_id,
        Capability::SessionObserve,
        GrantScope::Host,
        None,
    ))
    .expect("observe grant");

    let started = Instant::now();
    let mut delivered = 0;
    let mut dropped = 0;
    for _ in 0..60 {
        let report = host.publish_event(event());
        delivered += report.delivered;
        dropped += report.dropped;
        assert_eq!(
            report.skipped, 0,
            "granted events must be queued or dropped"
        );
    }
    let elapsed = started.elapsed();
    assert_eq!(
        delivered + dropped,
        60,
        "every event is accepted or dropped"
    );
    assert!(
        dropped > 0,
        "a stalled helper must overflow its bounded queue"
    );
    assert!(
        elapsed < Duration::from_secs(2),
        "publishing must never block on a slow plugin (took {elapsed:?})"
    );
    assert!(has_audit(&fixture, AuditKind::EventDropped));
    let status = host.status(&plugin_id).unwrap();
    assert!(status.dropped_events > 0);
    assert!(status.queued_events <= 4);
}

#[test]
fn disable_keeps_grants_while_uninstall_revokes_them() {
    let fixture = test_host_default();
    let host = &fixture.host;
    let plugin_id = install_fault(&fixture, "default");
    host.enable(&plugin_id).unwrap();
    host.grant(grant_request(
        &plugin_id,
        Capability::SessionObserve,
        GrantScope::Host,
        None,
    ))
    .unwrap();
    assert_eq!(host.grants(&plugin_id).len(), 1);

    host.disable(&plugin_id).unwrap();
    assert_eq!(
        host.grants(&plugin_id).len(),
        1,
        "disabling is not revocation"
    );

    host.uninstall(&plugin_id).unwrap();
    assert!(host.grants(&plugin_id).is_empty());
}

#[test]
fn audit_records_never_contain_command_arguments() {
    let fixture = test_host_default();
    let host = &fixture.host;
    host.install(PluginInstall::for_helper(sample_manifest(), sample_helper()).unwrap())
        .unwrap();
    host.enable(&sample_id()).unwrap();

    host.invoke(
        InvokeRequest::new(sample_id(), "sample.session.ping")
            .in_session("session-1")
            .with_args(json!({ "token": "super-secret-value", "prompt": "private text" })),
    )
    .expect("invoke");

    for record in fixture.audit.records() {
        assert!(!record.detail.contains("super-secret-value"));
        assert!(!record.detail.contains("private text"));
    }
}
