//! End-to-end, headless proof of the plugin broker: a real helper process is
//! enabled and disabled repeatedly **without restarting the host**, and its
//! Session action, tab and overlay contributions appear and disappear.
mod common;

use common::*;
use ukk_plugin_api::{ClientPlatform, ContributionSlot, OverlayAnchor};
use ukk_plugin_host::{
    AuditKind, EchoInProcessPlugin, HelperCommand, HelperLimits, HelperProcess, InProcessPlugin,
    InvokeRequest, PluginHost, PluginInstall, PluginState,
};

#[test]
fn sample_plugin_enable_disable_without_host_restart() {
    let fixture = test_host_default();
    let host = &fixture.host;
    let plugin_id = sample_id();

    host.install(PluginInstall::for_helper(sample_manifest(), sample_helper()).unwrap())
        .expect("install");

    // Installed but disabled: no contribution is visible and no process runs.
    assert!(host.session_snapshot(ClientPlatform::Desktop).is_empty());
    assert!(host.helper_pid(&plugin_id).is_none());
    assert_eq!(
        host.status(&plugin_id).unwrap().state,
        PluginState::Disabled
    );

    let status = host.enable(&plugin_id).expect("enable");
    assert_eq!(status.state, PluginState::Ready);
    assert_eq!(status.command_count, 1);
    assert_eq!(status.view_count, 3);
    let enabled_pid = host.helper_pid(&plugin_id).expect("helper pid");
    assert_process_alive(enabled_pid, "after enable");

    let snapshot = host.session_snapshot(ClientPlatform::Desktop);
    assert_eq!(snapshot.session_actions.len(), 1);
    assert_eq!(snapshot.session_tabs.len(), 1);
    assert_eq!(snapshot.session_overlays.len(), 1);
    assert_eq!(snapshot.commands.len(), 1);
    assert_eq!(
        snapshot.session_actions[0].plugin_id(),
        &plugin_id,
        "action must carry its owning plugin"
    );
    assert_eq!(
        snapshot.session_actions[0].view_id(),
        "sample.session.action"
    );
    assert_eq!(
        snapshot.session_actions[0].command(),
        Some("sample.session.ping")
    );
    let overlay = snapshot.session_overlays[0].overlay().expect("overlay");
    assert_eq!(overlay.anchor(), OverlayAnchor::TopRight);
    assert_eq!(overlay.max_width(), 320);
    assert_eq!(overlay.max_height(), 240);
    assert_eq!(
        snapshot.session_overlays[0].slot(),
        ContributionSlot::SessionOverlay
    );

    // The declared command needs no capability, so it runs immediately.
    let outcome = host
        .invoke(
            InvokeRequest::new(plugin_id.clone(), "sample.session.ping").in_session("session-1"),
        )
        .expect("invoke");
    assert_eq!(outcome.value["message"], "pong");
    assert_eq!(outcome.value["session_id"], "session-1");

    // Activating a tab view resolves to the same command.
    let via_view = host
        .invoke_view(&plugin_id, "sample.session.tab", Some("session-1"))
        .expect("invoke view");
    assert_eq!(via_view.command, "sample.session.ping");

    // Disable: helper is gone, contributions disappear, dispatch is refused.
    let status = host.disable(&plugin_id).expect("disable");
    assert_eq!(status.state, PluginState::Disabled);
    assert!(host.helper_pid(&plugin_id).is_none());
    assert_process_dead(enabled_pid, "disable");
    assert!(host.session_snapshot(ClientPlatform::Desktop).is_empty());
    let denied = host
        .invoke(InvokeRequest::new(plugin_id.clone(), "sample.session.ping").in_session("s"))
        .unwrap_err();
    assert_eq!(denied.code(), "not_enabled");

    // Re-enable in the same host instance: no host restart involved.
    let status = host.enable(&plugin_id).expect("re-enable");
    assert_eq!(status.state, PluginState::Ready);
    let reenabled_pid = host.helper_pid(&plugin_id).expect("new helper pid");
    assert_ne!(reenabled_pid, enabled_pid);
    assert_process_alive(reenabled_pid, "after re-enable");
    let outcome = host
        .invoke(InvokeRequest::new(plugin_id.clone(), "sample.session.ping").in_session("s2"))
        .expect("invoke after re-enable");
    assert_eq!(outcome.value["message"], "pong");
    assert_eq!(
        host.session_snapshot(ClientPlatform::Desktop)
            .session_tabs
            .len(),
        1
    );

    let kinds = fixture.audit.kinds();
    for expected in [
        AuditKind::PluginInstalled,
        AuditKind::PluginEnabled,
        AuditKind::CommandInvoked,
        AuditKind::PluginDisabled,
        AuditKind::PluginEnabled,
    ] {
        assert!(
            kinds.contains(&expected),
            "missing {expected:?} in {kinds:?}"
        );
    }
    // The only denial is the deliberate dispatch while disabled; the command
    // itself needs no capability.
    let invoked = kinds
        .iter()
        .filter(|kind| **kind == AuditKind::CommandInvoked)
        .count();
    assert_eq!(invoked, 3, "every allowed invocation is audited: {kinds:?}");
}

#[test]
fn platform_filter_hides_views_but_keeps_commands() {
    let fixture = test_host_default();
    let host = &fixture.host;
    let plugin_id = sample_id();
    host.install(PluginInstall::for_helper(sample_manifest(), sample_helper()).unwrap())
        .unwrap();
    host.enable(&plugin_id).unwrap();

    // The sample declares desktop-only views.
    let ios = host.session_snapshot(ClientPlatform::Ios);
    assert!(ios.session_actions.is_empty());
    assert!(ios.session_tabs.is_empty());
    assert!(ios.session_overlays.is_empty());
    assert_eq!(ios.commands.len(), 1);
}

#[test]
fn builtin_in_process_plugin_runs_but_is_still_checked() {
    let fixture = test_host_default();
    let host = &fixture.host;
    let plugin_id = ukk_plugin_api::PluginId::parse(BUILTIN_ID).unwrap();
    let plugin = std::sync::Arc::new(EchoInProcessPlugin::new(BUILTIN_ID))
        as std::sync::Arc<dyn InProcessPlugin>;

    host.install(PluginInstall::for_in_process(builtin_manifest(), plugin).unwrap())
        .expect("install builtin");
    assert_eq!(host.enable(&plugin_id).unwrap().state, PluginState::Ready);
    assert!(
        host.helper_pid(&plugin_id).is_none(),
        "no process for builtin"
    );

    let outcome = host
        .invoke(InvokeRequest::new(plugin_id.clone(), "builtin.echo").in_session("s"))
        .expect("invoke builtin");
    assert_eq!(outcome.value["in_process"], true);
    assert_eq!(outcome.value["command"], "builtin.echo");

    assert_eq!(
        host.session_snapshot(ClientPlatform::Desktop)
            .commands
            .len(),
        1
    );
    assert_eq!(
        host.disable(&plugin_id).unwrap().state,
        PluginState::Disabled
    );
}

#[test]
fn trust_and_runtime_must_match() {
    // First-party manifest cannot be installed in-process.
    let error = PluginInstall::for_in_process(
        sample_manifest(),
        std::sync::Arc::new(EchoInProcessPlugin::new(SAMPLE_ID)),
    )
    .unwrap_err();
    assert_eq!(error.code(), "trust_mismatch");

    // Builtin manifest cannot be installed as a helper.
    let error = PluginInstall::for_helper(builtin_manifest(), sample_helper()).unwrap_err();
    assert_eq!(error.code(), "trust_mismatch");

    // A relative helper path is never executed from PATH.
    let error = PluginInstall::for_helper(sample_manifest(), HelperCommand::new("sample-helper"))
        .unwrap_err();
    assert_eq!(error.code(), "invalid_helper");
}

#[test]
fn duplicate_plugin_and_command_registration_is_rejected() {
    let fixture = test_host_default();
    let host = &fixture.host;
    host.install(PluginInstall::for_helper(sample_manifest(), sample_helper()).unwrap())
        .unwrap();
    host.enable(&sample_id()).unwrap();

    // Same plugin id twice.
    let error = host
        .install(PluginInstall::for_helper(sample_manifest(), sample_helper()).unwrap())
        .unwrap_err();
    assert_eq!(error.code(), "duplicate_plugin");

    // A different plugin claiming an existing command id.
    let error = host
        .install(PluginInstall::for_helper(clash_manifest(), sample_helper()).unwrap())
        .unwrap_err();
    assert_eq!(error.code(), "duplicate_contribution");
    assert!(fixture
        .audit
        .kinds()
        .contains(&AuditKind::DuplicateContribution));

    // Nothing was half-installed.
    assert_eq!(host.installed(), vec![sample_id()]);
    assert_eq!(
        host.session_snapshot(ClientPlatform::Desktop)
            .commands
            .len(),
        1
    );
}

#[test]
fn uninstall_stops_helper_and_releases_command_ids() {
    let fixture = test_host_default();
    let host = &fixture.host;
    let plugin_id = sample_id();
    host.install(PluginInstall::for_helper(sample_manifest(), sample_helper()).unwrap())
        .unwrap();
    host.enable(&plugin_id).unwrap();
    let pid = host.helper_pid(&plugin_id).expect("helper pid");

    host.uninstall(&plugin_id).expect("uninstall");
    assert_process_dead(pid, "uninstall");
    assert!(host.status(&plugin_id).is_none());
    assert!(host.session_snapshot(ClientPlatform::Desktop).is_empty());
    assert_eq!(host.installed(), Vec::new());

    // The command id is free again, so a replacement plugin can install.
    host.install(PluginInstall::for_helper(clash_manifest(), sample_helper()).unwrap())
        .expect("command id must be released on uninstall");
}

#[test]
fn helper_shutdown_is_cooperative_or_killed() {
    warm_up(&sample_helper());
    warm_up(&fault_helper("default"));

    // Cooperative helper exits on `plugin.shutdown`.
    let process = HelperProcess::spawn(
        SAMPLE_ID,
        &sample_helper(),
        HelperLimits {
            request_timeout: std::time::Duration::from_secs(2),
            shutdown_grace: std::time::Duration::from_millis(500),
            max_frame_bytes: 64 * 1024,
            event_queue_capacity: 4,
        },
    )
    .expect("spawn");
    assert!(process.is_alive());
    assert!(process.health().is_ok());
    let cooperative_pid = process.pid();
    assert_process_alive(cooperative_pid, "helper running");
    let report = process.shutdown();
    assert!(!report.killed, "cooperative helper must not be killed");
    assert!(!process.is_alive());
    assert_process_dead(cooperative_pid, "cooperative shutdown");

    // A helper that stops reading cannot answer shutdown and is killed.
    let stalled = HelperProcess::spawn(
        FAULT_ID,
        &fault_helper("stall"),
        HelperLimits {
            request_timeout: std::time::Duration::from_secs(2),
            shutdown_grace: std::time::Duration::from_millis(200),
            max_frame_bytes: 64 * 1024,
            event_queue_capacity: 4,
        },
    )
    .expect("spawn stalled");
    assert!(stalled.health().is_ok());
    // `stall` stops answering only once a command arrives, so block it first.
    let error = stalled
        .invoke("sample.session.ping", None, serde_json::Value::Null)
        .unwrap_err();
    assert_eq!(error.code(), "timeout");
    let stalled_pid = stalled.pid();
    let report = stalled.shutdown();
    assert!(report.killed, "stalled helper must be killed after grace");
    assert!(!stalled.is_alive());
    assert_process_dead(stalled_pid, "stalled shutdown");
}

#[test]
fn max_plugins_limit_is_enforced() {
    let mut config = fast_config();
    config.max_plugins = 1;
    let fixture = test_host(config);
    fixture
        .host
        .install(PluginInstall::for_helper(sample_manifest(), sample_helper()).unwrap())
        .unwrap();
    let error = fixture
        .host
        .install(PluginInstall::for_helper(other_manifest(), other_helper()).unwrap())
        .unwrap_err();
    assert_eq!(error.code(), "too_many_plugins");
}

#[test]
fn unknown_plugin_operations_are_rejected() {
    let fixture = test_host_default();
    let host: &PluginHost = &fixture.host;
    let missing = other_id();
    assert_eq!(host.enable(&missing).unwrap_err().code(), "unknown_plugin");
    assert_eq!(host.disable(&missing).unwrap_err().code(), "unknown_plugin");
    assert_eq!(
        host.uninstall(&missing).unwrap_err().code(),
        "unknown_plugin"
    );
    assert_eq!(
        host.invoke(InvokeRequest::new(missing, "other.echo"))
            .unwrap_err()
            .code(),
        "unknown_plugin"
    );
}
