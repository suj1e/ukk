//! End-to-end Host plugin wiring: one explicit registry, the real sample
//! helper, the separate `plugin.control` token and the read-only snapshot.
//!
//! These tests spawn the actual `ukk-host` binary and speak the local RPC, so
//! they exercise transport, capability checks, supervisor life cycle and the
//! helper process together. They never call herdr-core: the mock absence of
//! `UKK_HC_BIN` is deliberate.

use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use ukk_host_client::HostClient;

#[cfg(windows)]
static HOST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(windows)]
fn serial_guard() -> std::sync::MutexGuard<'static, ()> {
    HOST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn unique_root(_tag: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    // Keep the Unix socket path below SUN_LEN on macOS: temp_dir is already
    // long, so only a compact suffix is appended.
    let unique = NEXT.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!("ukk-pw-{}-{unique}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    }
    root
}

fn sample_helper() -> PathBuf {
    let test_exe = std::env::current_exe().unwrap();
    let profile = test_exe.parent().unwrap().parent().unwrap();
    let name = if cfg!(windows) {
        "ukk-sample-session-helper.exe"
    } else {
        "ukk-sample-session-helper"
    };
    let candidate = profile.join(name);
    if !candidate.is_file() {
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let mut command = std::process::Command::new(cargo);
        command
            .args(["build", "-p", "ukk-plugin-host", "--bins"])
            .current_dir(env!("CARGO_MANIFEST_DIR"));
        if !cfg!(debug_assertions) {
            command.arg("--release");
        }
        assert!(command.status().unwrap().success(), "build sample helper");
    }
    candidate
        .canonicalize()
        .expect("sample helper binary must exist after cargo build")
}

fn sample_manifest() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugins/sample-session-action/plugin.toml")
        .canonicalize()
        .unwrap()
}

fn write_secret(path: &Path) -> String {
    let secret = format!("{}:{}", ukk_domain::new_id(), ukk_domain::new_id());
    fs::write(path, format!("{secret}\n")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
    secret
}

struct HostFixture {
    child: Child,
    root: PathBuf,
    registry: PathBuf,
    token: PathBuf,
    #[cfg(unix)]
    socket: PathBuf,
}

impl HostFixture {
    fn start(tag: &str, enabled: bool, provision_control: bool) -> Self {
        let root = unique_root(tag);
        let registry = root.join("registry.json");
        let registry_value = json!({
            "version": 1,
            "plugins": [{
                "id": "dev.ukk.sample.session-action",
                "manifest": sample_manifest(),
                "helper": sample_helper(),
                "enabled": enabled,
            }]
        });
        fs::write(
            &registry,
            serde_json::to_string_pretty(&registry_value).unwrap(),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&registry, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let token = root.join("plugin-control");
        if provision_control {
            write_secret(&token);
        }

        #[cfg(unix)]
        let socket = root.join("agent-host.sock");
        let log = root.join("host.log");
        let mut command = Command::new(env!("CARGO_BIN_EXE_ukk-host"));
        command
            .arg("serve")
            .env("UKK_PLUGIN_REGISTRY", &registry)
            .env("UKK_PLUGIN_CONTROL_TOKEN_FILE", &token)
            .env("UKK_DB_PATH", root.join("workspace.sqlite3"))
            .env("UKK_HC_BIN", "__ukk_hc_missing_for_plugin_tests__")
            .stdout(Stdio::null())
            .stderr(Stdio::from(fs::File::create(&log).unwrap()));
        #[cfg(unix)]
        command.env("UKK_SOCKET", &socket);
        let child = command.spawn().expect("start Host for plugin tests");
        let mut fixture = Self {
            child,
            root,
            registry,
            token,
            #[cfg(unix)]
            socket,
        };
        fixture.wait_until_ready();
        fixture
    }

    fn client(&self) -> HostClient {
        #[cfg(unix)]
        let client = HostClient::new(&self.socket);
        #[cfg(windows)]
        let client = HostClient::from_env();
        client.with_plugin_control_token_path(&self.token)
    }

    fn wait_until_ready(&mut self) {
        let client = self.client();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(code) = self.child.try_wait().expect("check Host process") {
                let log = fs::read_to_string(self.root.join("host.log")).unwrap_or_default();
                panic!("Host exited before the endpoint became ready: {code}\n{log}");
            }
            if client.ping().unwrap_or(false) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "Host endpoint did not become ready"
            );
            thread::sleep(Duration::from_millis(50));
        }
    }

    fn raw_request(&self, method: &str, params: Value) -> Value {
        let request = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        });
        #[cfg(unix)]
        {
            use std::io::{BufRead, BufReader, Write};
            use std::os::unix::net::UnixStream;

            let mut stream = UnixStream::connect(&self.socket).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            serde_json::to_writer(&mut stream, &request).unwrap();
            stream.write_all(b"\n").unwrap();
            stream.flush().unwrap();
            let mut line = String::new();
            BufReader::new(stream).read_line(&mut line).unwrap();
            serde_json::from_str(&line).unwrap()
        }
        #[cfg(windows)]
        {
            use std::io::{BufRead, BufReader, Write};

            let mut pipe = ukk_host_client::windows_pipe::open_client().unwrap();
            pipe.set_read_timeout(Duration::from_secs(5));
            serde_json::to_writer(&mut pipe, &request).unwrap();
            pipe.write_all(b"\n").unwrap();
            pipe.flush().unwrap();
            let mut line = String::new();
            BufReader::new(pipe).read_line(&mut line).unwrap();
            serde_json::from_str(&line).unwrap()
        }
    }

    fn workspace_token_path(&self) -> PathBuf {
        self.client().mutation_token_path()
    }

    fn registry_json(&self) -> Value {
        serde_json::from_str(&fs::read_to_string(&self.registry).unwrap()).unwrap()
    }
}

impl Drop for HostFixture {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn error_kind(response: &Value) -> &str {
    response
        .pointer("/error/data/kind")
        .and_then(Value::as_str)
        .unwrap_or("")
}

fn control_secret(path: &Path) -> String {
    fs::read_to_string(path).unwrap().trim().to_owned()
}

const PLUGIN_ID: &str = "dev.ukk.sample.session-action";

#[test]
fn sample_plugin_enable_invoke_disable_without_host_restart() {
    #[cfg(windows)]
    let _serial = serial_guard();
    let mut fixture = HostFixture::start("e2e", true, true);
    let client = fixture.client();
    let host_pid = fixture.child.id();

    let snapshot = client.plugin_snapshot().expect("snapshot");
    assert!(snapshot.control_enabled, "provisioned control must be on");
    assert!(snapshot.registry_loaded);
    assert_eq!(snapshot.plugins.len(), 1);
    assert_eq!(snapshot.plugins[0].state, "ready");
    assert!(snapshot.plugins[0].enabled_by_registry);
    assert_eq!(snapshot.session_actions.len(), 1);
    assert_eq!(snapshot.session_tabs.len(), 1);
    assert_eq!(snapshot.session_overlays.len(), 1);
    assert_eq!(snapshot.commands.len(), 1);
    let overlay = snapshot.session_overlays[0].overlay.as_ref().unwrap();
    assert_eq!(overlay.anchor, "top-right");
    assert_eq!(overlay.max_width, 320);
    assert_eq!(overlay.max_height, 240);

    let outcome = client
        .invoke_plugin(
            PLUGIN_ID,
            "sample.session.ping",
            Some("session-1"),
            json!({"probe": true}),
        )
        .expect("invoke sample command");
    assert_eq!(outcome.value["message"], "pong");
    assert_eq!(outcome.value["session_id"], "session-1");
    assert_eq!(outcome.provenance, "first-party");
    assert_eq!(outcome.command, "sample.session.ping");

    let secret = control_secret(&fixture.token);
    let disabled = fixture.raw_request(
        "plugin.disable",
        json!({"plugin_id": PLUGIN_ID, "control": secret}),
    );
    assert!(disabled.get("result").is_some(), "{disabled}");
    assert_eq!(disabled["result"]["state"], "disabled");

    let after_disable = client.plugin_snapshot().unwrap();
    assert_eq!(after_disable.plugins[0].state, "disabled");
    assert!(after_disable.session_actions.is_empty());
    assert!(after_disable.session_tabs.is_empty());
    assert!(after_disable.session_overlays.is_empty());
    let denied = client
        .invoke_plugin(
            PLUGIN_ID,
            "sample.session.ping",
            Some("session-1"),
            Value::Null,
        )
        .unwrap_err();
    assert!(denied.to_string().contains("not ready"), "{denied}");

    // Same Host process: no restart happened.
    assert_eq!(fixture.child.id(), host_pid);
    assert!(fixture.child.try_wait().unwrap().is_none());

    let enabled = fixture.raw_request(
        "plugin.enable",
        json!({"plugin_id": PLUGIN_ID, "control": secret}),
    );
    assert!(enabled.get("result").is_some(), "{enabled}");
    assert_eq!(enabled["result"]["state"], "ready");
    let snapshot = client.plugin_snapshot().unwrap();
    assert_eq!(snapshot.plugins[0].state, "ready");
    assert_eq!(snapshot.session_tabs.len(), 1);
    let outcome = client
        .invoke_plugin(
            PLUGIN_ID,
            "sample.session.ping",
            Some("session-2"),
            Value::Null,
        )
        .expect("invoke after re-enable");
    assert_eq!(outcome.value["message"], "pong");
    assert_eq!(outcome.value["session_id"], "session-2");

    // The operator's enabled choice is persisted for the next Host start.
    let registry = fixture.registry_json();
    assert_eq!(registry["plugins"][0]["enabled"], true);
}

#[test]
fn disabled_registry_entry_requires_an_explicit_enable() {
    #[cfg(windows)]
    let _serial = serial_guard();
    let fixture = HostFixture::start("disabled", false, true);
    let client = fixture.client();
    let snapshot = client.plugin_snapshot().unwrap();
    assert_eq!(snapshot.plugins.len(), 1);
    assert_eq!(snapshot.plugins[0].state, "disabled");
    assert!(!snapshot.plugins[0].enabled_by_registry);
    assert!(snapshot.session_actions.is_empty());

    let denied = client
        .invoke_plugin(PLUGIN_ID, "sample.session.ping", Some("s"), Value::Null)
        .unwrap_err();
    assert!(denied.to_string().contains("not ready"), "{denied}");

    let secret = control_secret(&fixture.token);
    let enabled = fixture.raw_request(
        "plugin.enable",
        json!({"plugin_id": PLUGIN_ID, "control": secret}),
    );
    assert!(enabled.get("result").is_some(), "{enabled}");
    let outcome = client
        .invoke_plugin(PLUGIN_ID, "sample.session.ping", Some("s"), Value::Null)
        .unwrap();
    assert_eq!(outcome.value["message"], "pong");
}

#[test]
fn plugin_control_is_default_off_and_never_accepts_other_tokens() {
    #[cfg(windows)]
    let _serial = serial_guard();
    let fixture = HostFixture::start("control", true, false);
    let client = fixture.client();

    let snapshot = client.plugin_snapshot().unwrap();
    assert!(!snapshot.control_enabled);
    assert!(snapshot.control_problem.is_none());

    // Read-only snapshot works without any capability.
    assert_eq!(snapshot.plugins[0].state, "ready");

    let missing = fixture.raw_request("plugin.disable", json!({"plugin_id": PLUGIN_ID}));
    assert_eq!(missing["error"]["code"], -32061);
    assert_eq!(error_kind(&missing), "plugin_control_disabled");

    // The workspace metadata token is not accepted, in either field.
    let workspace = fs::read_to_string(fixture.workspace_token_path())
        .unwrap()
        .trim()
        .to_owned();
    let with_auth = fixture.raw_request(
        "plugin.disable",
        json!({"plugin_id": PLUGIN_ID, "auth": workspace}),
    );
    assert_eq!(with_auth["error"]["code"], -32061);

    // Provision the separate plugin token while the Host is running; the Host
    // reloads it per request, so no restart is needed.
    let secret = write_secret(&fixture.token);
    let snapshot = client.plugin_snapshot().unwrap();
    assert!(snapshot.control_enabled);

    let forged_workspace = fixture.raw_request(
        "plugin.disable",
        json!({"plugin_id": PLUGIN_ID, "control": workspace}),
    );
    assert_eq!(forged_workspace["error"]["code"], -32062);
    assert_eq!(error_kind(&forged_workspace), "plugin_control_unauthorized");

    let auth_only = fixture.raw_request(
        "plugin.disable",
        json!({"plugin_id": PLUGIN_ID, "auth": secret}),
    );
    assert_eq!(auth_only["error"]["code"], -32062);

    // An agent-control-shaped token is still not the plugin token.
    let mut agent_like = "agent-control:".to_owned();
    agent_like.push_str(&"z".repeat(70));
    let wrong = fixture.raw_request(
        "plugin.disable",
        json!({"plugin_id": PLUGIN_ID, "control": agent_like}),
    );
    assert_eq!(wrong["error"]["code"], -32062);

    let accepted = fixture.raw_request(
        "plugin.disable",
        json!({"plugin_id": PLUGIN_ID, "control": secret}),
    );
    assert!(accepted.get("result").is_some(), "{accepted}");
}

#[test]
fn snapshot_shape_is_contribution_only_and_never_contains_helper_output() {
    #[cfg(windows)]
    let _serial = serial_guard();
    let fixture = HostFixture::start("snapshot", true, true);
    let client = fixture.client();
    let secret = control_secret(&fixture.token);

    let snapshot = fixture.raw_request("plugin.snapshot", json!({}));
    let result = &snapshot["result"];
    assert_eq!(result["capability"], "separate-plugin-control");
    assert_eq!(result["control_enabled"], true);
    assert_eq!(result["registry"]["loaded"], true);
    assert_eq!(result["registry"]["present"], true);
    assert!(result["registry"]["error"].is_null());
    let contributions = result["contributions"].as_object().unwrap();
    assert!(contributions.contains_key("session_actions"));
    assert!(contributions.contains_key("session_tabs"));
    assert!(contributions.contains_key("session_overlays"));
    assert!(contributions.contains_key("commands"));
    assert!(!contributions.contains_key("context_panels"));
    assert!(!contributions.contains_key("settings_sections"));
    let plugin = &result["plugins"][0];
    assert_eq!(plugin["installed"], true);
    assert_eq!(plugin["state"], "ready");
    assert_eq!(plugin["trust"], "first-party");
    assert!(plugin.get("value").is_none());

    let outcome = client
        .invoke_plugin(
            PLUGIN_ID,
            "sample.session.ping",
            Some("session-1"),
            Value::Null,
        )
        .unwrap();
    assert_eq!(outcome.value["message"], "pong");

    let after = fixture.raw_request("plugin.snapshot", json!({}));
    let text = after["result"].to_string();
    assert!(
        !text.contains("pong"),
        "helper output must not enter snapshots"
    );
    assert!(
        !text.contains(&secret),
        "capability value must never be exposed"
    );
    assert!(!text.contains("capabilities_used"));
}

#[test]
fn plugin_rpc_does_not_change_runtime_observation() {
    #[cfg(windows)]
    let _serial = serial_guard();
    let fixture = HostFixture::start("runtime", true, true);
    let client = fixture.client();
    let before = client.host_snapshot().expect("host snapshot before");
    assert!(before.snapshot.terminals.is_empty());
    assert!(!before.core_connected);

    client
        .invoke_plugin(
            PLUGIN_ID,
            "sample.session.ping",
            Some("session-1"),
            Value::Null,
        )
        .expect("plugin invoke");

    let after = client.host_snapshot().expect("host snapshot after");
    assert!(after.snapshot.terminals.is_empty());
    assert!(!after.core_connected);
    assert_eq!(
        after.snapshot.hidden_finished, before.snapshot.hidden_finished,
        "plugin work must not alter terminal/runtime state"
    );
}

#[test]
fn plugin_discovery_happens_only_through_the_explicit_registry() {
    #[cfg(windows)]
    let _serial = serial_guard();
    let root = unique_root("no-registry");
    let log = root.join("host.log");
    let mut command = Command::new(env!("CARGO_BIN_EXE_ukk-host"));
    command
        .arg("serve")
        .env("UKK_PLUGIN_REGISTRY", root.join("missing-registry.json"))
        .env("UKK_PLUGIN_CONTROL_TOKEN_FILE", root.join("plugin-control"))
        .env("UKK_DB_PATH", root.join("workspace.sqlite3"))
        .env("UKK_HC_BIN", "__ukk_hc_missing_for_plugin_tests__")
        .stdout(Stdio::null())
        .stderr(Stdio::from(fs::File::create(&log).unwrap()));
    #[cfg(unix)]
    let socket = root.join("agent-host.sock");
    #[cfg(unix)]
    command.env("UKK_SOCKET", &socket);
    let mut child = command.spawn().unwrap();
    #[cfg(unix)]
    let client = HostClient::new(&socket);
    #[cfg(windows)]
    let client = HostClient::from_env();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if child.try_wait().unwrap().is_some() {
            let log = fs::read_to_string(&log).unwrap_or_default();
            panic!("Host exited before the endpoint became ready\n{log}");
        }
        if client.ping().unwrap_or(false) {
            break;
        }
        assert!(Instant::now() < deadline, "Host did not become ready");
        thread::sleep(Duration::from_millis(50));
    }
    let snapshot = client.plugin_snapshot().unwrap();
    assert!(!snapshot.registry_present);
    assert!(snapshot.registry_error.is_none());
    assert!(snapshot.plugins.is_empty());
    assert!(snapshot.session_actions.is_empty());
    assert!(snapshot.session_tabs.is_empty());
    assert!(snapshot.session_overlays.is_empty());
    assert!(snapshot.commands.is_empty());
    let _ = child.kill();
    let _ = child.wait();
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn plugin_control_cli_provisions_and_revokes_the_separate_token() {
    let root = unique_root("cli");
    let token = root.join("plugin-control");
    let host = env!("CARGO_BIN_EXE_ukk-host");

    let enable = Command::new(host)
        .arg("plugin-control-enable")
        .env("UKK_PLUGIN_CONTROL_TOKEN_FILE", &token)
        .output()
        .unwrap();
    assert!(
        enable.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&enable.stderr)
    );
    assert!(String::from_utf8_lossy(&enable.stdout).contains("created"));
    let secret = control_secret(&token);
    assert!(secret.len() >= 60);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&token).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    let status = Command::new(host)
        .arg("plugin-control-status")
        .env("UKK_PLUGIN_CONTROL_TOKEN_FILE", &token)
        .output()
        .unwrap();
    assert!(status.status.success());
    assert!(String::from_utf8_lossy(&status.stdout).contains("enabled: true"));

    let disable = Command::new(host)
        .arg("plugin-control-disable")
        .env("UKK_PLUGIN_CONTROL_TOKEN_FILE", &token)
        .output()
        .unwrap();
    assert!(disable.status.success());
    assert!(!token.exists());

    let status = Command::new(host)
        .arg("plugin-control-status")
        .env("UKK_PLUGIN_CONTROL_TOKEN_FILE", &token)
        .output()
        .unwrap();
    assert!(status.status.success());
    assert!(String::from_utf8_lossy(&status.stdout).contains("enabled: false"));
    let _ = fs::remove_dir_all(&root);
}
