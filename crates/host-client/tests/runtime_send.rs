//! Contract tests for the explicit `runtime.send` client surface.
//!
//! A local mock Host proves the wire payload: the separate agent-control
//! token, a fresh `request_id`, `intent: "user"`, and the exact binding. It
//! also proves denials are returned, not retried, and that no capability file
//! is read before the line/binding shape is valid.

#![cfg(unix)]
use serde_json::{json, Value};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};
use ukk_host_client::{HostClient, HostError, MAX_RUNTIME_SEND_BYTES};

static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

fn temp_dir(tag: &str) -> PathBuf {
    let unique = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    // macOS temp paths are long; keep the socket path below SUN_LEN.
    let dir =
        std::env::temp_dir().join(format!("ukk{tag}-{}-{unique}-{nanos}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_token(path: &Path, token: &str, mode: u32) {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(mode)
        .open(path)
        .unwrap();
    file.write_all(token.as_bytes()).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

fn agent_token() -> String {
    "a".repeat(80)
}

fn workspace_token() -> String {
    "w".repeat(80)
}

fn client(socket: &Path, token_path: &Path) -> HostClient {
    HostClient::new(socket).with_agent_control_token_path(token_path)
}

/// Accept one connection, capture the request and reply with `respond`.
fn serve_once(
    listener: UnixListener,
    respond: impl FnOnce(&Value) -> Value + Send + 'static,
) -> thread::JoinHandle<Value> {
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut line = String::new();
        BufReader::new(stream.try_clone().unwrap())
            .read_line(&mut line)
            .unwrap();
        let request: Value = serde_json::from_str(&line).unwrap();
        let response = respond(&request);
        writeln!(stream, "{response}").unwrap();
        request
    })
}

#[test]
fn runtime_send_uses_the_separate_control_token_and_exact_binding() {
    let dir = temp_dir("rs");
    let socket = dir.join("host.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let token_path = dir.join("explicit.agent-control");
    write_token(&token_path, &agent_token(), 0o600);
    // A workspace token exists and must never appear in the control field.
    write_token(&dir.join("host.token"), &workspace_token(), 0o600);

    let expected_agent = agent_token();
    let expected_workspace = workspace_token();
    let handler = serve_once(listener, move |request| {
        assert_eq!(request["method"], "runtime.send");
        let params = &request["params"];
        assert_eq!(params["core_id"], "core-1");
        assert_eq!(params["terminal"], "t7");
        assert_eq!(params["text"], "cargo test --all");
        assert_eq!(params["intent"], "user");
        assert_eq!(params["control"], expected_agent);
        assert!(
            params.get("auth").is_none(),
            "workspace auth must be absent"
        );
        assert_ne!(params["control"], expected_workspace);
        let request_id = params["request_id"].as_str().unwrap();
        assert!(!request_id.trim().is_empty());
        assert!(request_id.len() <= 128);
        json!({
            "jsonrpc":"2.0","id":request["id"],
            "result":{"turn":5,"status":"working","started":true,"replayed":false}
        })
    });

    let outcome = client(&socket, &token_path)
        .runtime_send("core-1", "t7", "cargo test --all")
        .unwrap();
    handler.join().unwrap();
    fs::remove_dir_all(&dir).unwrap();

    assert_eq!(outcome.core_id, "core-1");
    assert_eq!(outcome.terminal, "t7");
    assert_eq!(outcome.turn, 5);
    assert_eq!(outcome.status.as_deref(), Some("working"));
    assert_eq!(outcome.started, Some(true));
    assert!(!outcome.replayed);
}

#[test]
fn runtime_send_issues_a_new_request_id_for_each_explicit_line() {
    let dir = temp_dir("ri");
    let socket = dir.join("host.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let token_path = dir.join("explicit.agent-control");
    write_token(&token_path, &agent_token(), 0o600);

    let handler = thread::spawn(move || {
        let mut ids = Vec::new();
        for turn in [1_u64, 2] {
            let (mut stream, _) = listener.accept().unwrap();
            let mut line = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            let request: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(request["params"]["intent"], "user");
            ids.push(request["params"]["request_id"].as_str().unwrap().to_owned());
            let response = json!({
                "jsonrpc":"2.0","id":request["id"],
                "result":{"turn":turn,"status":"working","replayed":false}
            });
            writeln!(stream, "{response}").unwrap();
        }
        ids
    });

    let client = client(&socket, &token_path);
    client.runtime_send("core-1", "t7", "first").unwrap();
    client.runtime_send("core-1", "t7", "second").unwrap();
    let ids = handler.join().unwrap();
    fs::remove_dir_all(&dir).unwrap();

    assert_eq!(ids.len(), 2);
    assert_ne!(ids[0], ids[1], "each explicit line is its own request");
    assert!(ids.iter().all(|id| !id.trim().is_empty()));
}

#[test]
fn runtime_send_surfaces_a_host_denial_without_retrying() {
    let dir = temp_dir("rd");
    let socket = dir.join("host.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let token_path = dir.join("explicit.agent-control");
    write_token(&token_path, &agent_token(), 0o600);

    let handler = serve_once(listener, |request| {
        json!({
            "jsonrpc":"2.0","id":request["id"],
            "error":{"code":-32043,"message":"agent-control capability is not authorized"}
        })
    });

    let error = client(&socket, &token_path)
        .runtime_send("core-1", "t7", "echo hi")
        .expect_err("a denial must not be retried or reported as success");
    handler.join().unwrap();
    fs::remove_dir_all(&dir).unwrap();

    match error {
        HostError::Remote(value) => {
            assert_eq!(value["code"], -32043);
            assert!(value["message"]
                .as_str()
                .unwrap()
                .contains("not authorized"));
        }
        other => panic!("unexpected error: {other}"),
    }
}

#[test]
fn runtime_send_surfaces_an_idempotent_replay_truthfully() {
    let dir = temp_dir("rr");
    let socket = dir.join("host.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let token_path = dir.join("explicit.agent-control");
    write_token(&token_path, &agent_token(), 0o600);

    let handler = serve_once(listener, |request| {
        json!({
            "jsonrpc":"2.0","id":request["id"],
            "result":{"turn":4,"status":"working","replayed":true}
        })
    });

    let outcome = client(&socket, &token_path)
        .runtime_send("core-1", "t7", "echo hi")
        .unwrap();
    handler.join().unwrap();
    fs::remove_dir_all(&dir).unwrap();

    assert_eq!(outcome.turn, 4);
    assert!(outcome.replayed);
}

#[test]
fn runtime_send_never_connects_without_the_control_token() {
    let dir = temp_dir("rm");
    let socket = dir.join("host.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let token_path = dir.join("missing.agent-control");

    let error = client(&socket, &token_path)
        .runtime_send("core-1", "t7", "echo hi")
        .expect_err("control must be required");
    assert!(
        matches!(&error, HostError::Protocol(message) if message.contains("agent control capability is unavailable")),
        "unexpected error: {error}"
    );
    assert!(
        listener.accept().is_err(),
        "no connection may be opened before the capability is read"
    );
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn runtime_send_rejects_an_insecure_or_short_control_token() {
    let dir = temp_dir("rx");
    let socket = dir.join("host.sock");
    let _listener = UnixListener::bind(&socket).unwrap();
    let token_path = dir.join("insecure.agent-control");
    write_token(&token_path, &agent_token(), 0o644);

    let error = client(&socket, &token_path)
        .runtime_send("core-1", "t7", "echo hi")
        .expect_err("0644 must be refused");
    assert!(
        matches!(&error, HostError::Protocol(message) if message.contains("0600")),
        "unexpected error: {error}"
    );

    write_token(&token_path, "short", 0o600);
    let error = client(&socket, &token_path)
        .runtime_send("core-1", "t7", "echo hi")
        .expect_err("a short token must be refused");
    assert!(
        matches!(&error, HostError::Protocol(message) if message.contains("too short")),
        "unexpected error: {error}"
    );
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn runtime_send_validates_the_line_before_reading_the_capability() {
    let dir = temp_dir("rv");
    let socket = dir.join("host.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    // Deliberately no token file: shape errors must win and no IPC may happen.
    let token_path = dir.join("missing.agent-control");
    let client = client(&socket, &token_path);

    for (core, terminal, line) in [
        ("core-1", "t7", ""),
        ("core-1", "t7", "   "),
        ("core-1", "t7", "one\ntwo"),
        ("core-1", "t7", "bell\u{7}"),
        ("core-1", "t7", "spoof\u{202e}txt"),
        ("core-1", "t7", &"x".repeat(MAX_RUNTIME_SEND_BYTES + 1)),
        ("", "t7", "echo hi"),
        ("core-1", "  ", "echo hi"),
    ] {
        let error = client
            .runtime_send(core, terminal, line)
            .expect_err("invalid input must be rejected");
        assert!(
            matches!(error, HostError::Protocol(_)),
            "unexpected error for {core:?}/{terminal:?}/{line:?}: {error}"
        );
    }
    assert!(
        listener.accept().is_err(),
        "invalid input must not open a connection"
    );
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn runtime_status_is_read_only_and_needs_no_capability_file() {
    let dir = temp_dir("rsx");
    let socket = dir.join("host.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let token_path = dir.join("missing.agent-control");

    let handler = serve_once(listener, |request| {
        assert_eq!(request["method"], "runtime.status");
        json!({
            "jsonrpc":"2.0","id":request["id"],
            "result":{
                "enabled":false,
                "capability":"separate-agent-control",
                "problem":"agent-control token is missing",
                "view_close_stops_agent":false
            }
        })
    });

    let status = client(&socket, &token_path).runtime_status().unwrap();
    handler.join().unwrap();
    fs::remove_dir_all(&dir).unwrap();

    assert!(!status.enabled);
    assert_eq!(status.capability, "separate-agent-control");
    assert_eq!(
        status.problem.as_deref(),
        Some("agent-control token is missing")
    );
}
