//! Shared fixtures for the plugin-host integration tests.
//!
//! Every helper executable is a real process built from this crate's `[[bin]]`
//! targets, located through `CARGO_BIN_EXE_*`. Nothing is mocked at the process
//! boundary, so the tests exercise spawn, framing, timeouts and kill paths.
#![allow(dead_code)]
use std::sync::Arc;
use std::time::Duration;

use ukk_plugin_api::{PluginId, ValidatedManifest};
use ukk_plugin_host::{HostConfig, InMemoryGrantStore, MemoryAuditSink, PluginHost};

pub const SAMPLE_ID: &str = "dev.ukk.sample.session-action";
pub const FAULT_ID: &str = "dev.ukk.fault.plugin";
pub const OTHER_ID: &str = "dev.ukk.other.plugin";
pub const BUILTIN_ID: &str = "dev.ukk.builtin.echo";

pub fn sample_manifest() -> ValidatedManifest {
    ValidatedManifest::from_toml_str(include_str!(
        "../../../../plugins/sample-session-action/plugin.toml"
    ))
    .expect("sample manifest must validate")
}

pub fn sample_id() -> PluginId {
    PluginId::parse(SAMPLE_ID).unwrap()
}

pub fn fault_id() -> PluginId {
    PluginId::parse(FAULT_ID).unwrap()
}

pub fn other_id() -> PluginId {
    PluginId::parse(OTHER_ID).unwrap()
}

pub const FAULT_MANIFEST: &str = r#"
manifest_version = 1
id = "dev.ukk.fault.plugin"
name = "Fault injection plugin"
version = "0.1.0"
api = ">=0.1,<0.2"
trust = "first-party"
entrypoint = "ukk-plugin-fault-helper"

[capabilities]
required = []
optional = ["session.observe", "agent.approve", "screen.view"]

[[contributions.commands]]
id = "fault.ping"
title = "Fault: ping"
scope = "session"
requires = []

[[contributions.commands]]
id = "fault.approve"
title = "Fault: approve an agent action"
scope = "session"
requires = ["agent.approve"]

[[contributions.commands]]
id = "fault.screen"
title = "Fault: view screen"
scope = "session"
requires = ["screen.view"]

[[contributions.events]]
id = "fault.events"
kinds = ["session.updated"]
"#;

pub fn fault_manifest() -> ValidatedManifest {
    ValidatedManifest::from_toml_str(FAULT_MANIFEST).expect("fault manifest must validate")
}

pub const OTHER_MANIFEST: &str = r#"
manifest_version = 1
id = "dev.ukk.other.plugin"
name = "Other plugin"
version = "0.1.0"
api = ">=0.1,<0.2"
trust = "first-party"
entrypoint = "ukk-plugin-fault-helper"

[[contributions.commands]]
id = "fault.ping"
title = "Other: ping"
scope = "session"
requires = []

[[contributions.commands]]
id = "other.echo"
title = "Other: echo"
scope = "session"
requires = []
"#;

pub fn other_manifest() -> ValidatedManifest {
    ValidatedManifest::from_toml_str(OTHER_MANIFEST).unwrap()
}

/// A second plugin that tries to claim the sample's command id.
pub const CLASH_MANIFEST: &str = r#"
manifest_version = 1
id = "dev.ukk.clash.plugin"
name = "Clashing plugin"
version = "0.1.0"
api = ">=0.1,<0.2"
trust = "first-party"
entrypoint = "ukk-sample-session-helper"

[[contributions.commands]]
id = "sample.session.ping"
title = "Clash: ping"
scope = "session"
requires = []
"#;

pub fn clash_manifest() -> ValidatedManifest {
    ValidatedManifest::from_toml_str(CLASH_MANIFEST).unwrap()
}

pub const BUILTIN_MANIFEST: &str = r#"
manifest_version = 1
id = "dev.ukk.builtin.echo"
name = "Builtin echo"
version = "0.1.0"
api = ">=0.1,<0.2"
trust = "builtin"

[[contributions.commands]]
id = "builtin.echo"
title = "Builtin: echo"
scope = "session"
requires = []
"#;

pub fn builtin_manifest() -> ValidatedManifest {
    ValidatedManifest::from_toml_str(BUILTIN_MANIFEST).unwrap()
}

pub fn sample_helper() -> ukk_plugin_host::HelperCommand {
    ukk_plugin_host::HelperCommand::new(env!("CARGO_BIN_EXE_ukk-sample-session-helper"))
}

pub fn fault_helper(mode: &str) -> ukk_plugin_host::HelperCommand {
    fault_helper_as(FAULT_ID, mode)
}

pub fn fault_helper_as(plugin_id: &str, mode: &str) -> ukk_plugin_host::HelperCommand {
    ukk_plugin_host::HelperCommand::new(env!("CARGO_BIN_EXE_ukk-plugin-fault-helper"))
        .with_arg("--id")
        .with_arg(plugin_id)
        .with_arg("--mode")
        .with_arg(mode)
}

/// The "other" plugin runs the fault helper under its own identity.
pub fn other_helper() -> ukk_plugin_host::HelperCommand {
    fault_helper_as(OTHER_ID, "default")
}

/// Shared limits. The request deadline is generous because the *first*
/// execution of a freshly linked helper binary costs a few hundred
/// milliseconds on macOS; later spawns are milliseconds. Timeout tests call
/// [`warm_up`] and then shrink the deadline explicitly.
pub fn fast_config() -> HostConfig {
    HostConfig {
        request_timeout: Duration::from_secs(2),
        shutdown_grace: Duration::from_millis(500),
        max_frame_bytes: 64 * 1024,
        event_queue_capacity: 8,
        max_plugins: 8,
    }
}

/// Same as [`fast_config`] with a short request deadline, for timeout tests.
pub fn short_timeout_config(timeout_ms: u64) -> HostConfig {
    HostConfig {
        request_timeout: Duration::from_millis(timeout_ms),
        ..fast_config()
    }
}

/// Execute one throwaway round trip so the OS has the helper image cached.
/// Needed before asserting on a short request deadline.
pub fn warm_up(helper: &ukk_plugin_host::HelperCommand) {
    let limits = ukk_plugin_host::HelperLimits {
        request_timeout: Duration::from_secs(10),
        shutdown_grace: Duration::from_millis(200),
        max_frame_bytes: 64 * 1024,
        event_queue_capacity: 1,
    };
    if let Ok(process) = ukk_plugin_host::HelperProcess::spawn("dev.ukk.warmup", helper, limits) {
        let _ = process.health();
        process.shutdown();
    }
}

pub struct TestHost {
    pub host: PluginHost,
    pub audit: Arc<MemoryAuditSink>,
    pub grants: Arc<InMemoryGrantStore>,
}

pub fn test_host(config: HostConfig) -> TestHost {
    let audit = Arc::new(MemoryAuditSink::new());
    let grants = Arc::new(InMemoryGrantStore::new());
    TestHost {
        host: PluginHost::new(config, grants.clone(), audit.clone()),
        audit,
        grants,
    }
}

pub fn test_host_default() -> TestHost {
    test_host(fast_config())
}

/// OS-level liveness probe for a pid captured earlier.
///
/// This is deliberately *not* `helper_pid().is_none()`: reading the slot only
/// proves the host forgot the handle, which is exactly the failure mode a
/// containment bug produces. `kill -0` / `tasklist` asks the OS.
#[cfg(unix)]
pub fn process_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(windows)]
pub fn process_alive(pid: u32) -> bool {
    let filter = format!("PID eq {pid}");
    std::process::Command::new("tasklist")
        .args(["/FI", &filter, "/NH"])
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).contains(&pid.to_string()))
        .unwrap_or(false)
}

/// Wait briefly for the OS to report the process gone, then assert it.
///
/// Reaping is synchronous in the host, so this normally succeeds on the first
/// probe; the retry only absorbs CI scheduler noise.
pub fn assert_process_dead(pid: u32, context: &str) {
    for _ in 0..50 {
        if !process_alive(pid) {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("process {pid} is still alive after {context}");
}

pub fn assert_process_alive(pid: u32, context: &str) {
    assert!(
        process_alive(pid),
        "process {pid} should be alive ({context})"
    );
}
