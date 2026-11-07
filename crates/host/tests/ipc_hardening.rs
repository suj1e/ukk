//! Local Host IPC hardening contract tests (Unix).
//!
//! These exercise the real `ukk-host` binary over a Unix socket, not a mock of
//! the transport: framing, authorization, size/quota limits, permission
//! rejection, stale-socket recovery and live-socket refusal. Windows Named
//! Pipe equivalents live in `windows_named_pipe.rs` and run in Windows CI.
#![cfg(unix)]

use serde_json::Value;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};
use ukk_host_client::HostClient;

/// Must match `MAX_REQUESTS_PER_CONNECTION` in `crates/host/src/transport.rs`.
const MAX_REQUESTS_PER_CONNECTION: usize = 256;
/// Must match `MAX_CONNECTIONS` in `crates/host/src/transport.rs`.
const MAX_CONNECTIONS: usize = 32;

struct HostProcess {
    child: Child,
}

impl HostProcess {
    fn terminate_and_wait(&mut self) -> std::process::ExitStatus {
        let status = Command::new("kill")
            .args(["-TERM", &self.child.id().to_string()])
            .status()
            .expect("send SIGTERM to Host");
        assert!(status.success(), "kill -TERM failed: {status}");
        self.child.wait().expect("wait for Host exit")
    }
}

impl Drop for HostProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Workspace {
    root: PathBuf,
    socket: PathBuf,
}

static NEXT_WORKSPACE: AtomicU64 = AtomicU64::new(0);

impl Workspace {
    fn new(_tag: &str) -> Self {
        // Keep the socket path below SUN_LEN on macOS (104 bytes including NUL).
        let unique = NEXT_WORKSPACE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("ukk-ipc-{}-{unique}", std::process::id()));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let socket = root.join("agent-host.sock");
        Self { root, socket }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_ukk-host"));
        command
            .arg("serve")
            .env("UKK_SOCKET", &self.socket)
            .env("UKK_DB_PATH", self.root.join("workspace.sqlite3"))
            .env("UKK_HC_BIN", "__ukk_hc_missing_for_hardening_tests__")
            .stdout(Stdio::null());
        command
    }

    fn start(&self) -> HostProcess {
        let mut child = self
            .command()
            .stderr(Stdio::null())
            .spawn()
            .expect("start ukk-host serve");
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            if let Some(status) = child.try_wait().expect("check Host process") {
                panic!("ukk-host exited before the socket became usable: {status}");
            }
            if HostClient::new(&self.socket).ping().unwrap_or(false) {
                return HostProcess { child };
            }
            assert!(
                Instant::now() < deadline,
                "ukk-host socket did not become usable"
            );
            thread::sleep(Duration::from_millis(50));
        }
    }

    fn run_cli(&self, argument: &str) -> std::process::Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_ukk-host"));
        command
            .arg(argument)
            .env("UKK_SOCKET", &self.socket)
            .env("UKK_DB_PATH", self.root.join("workspace.sqlite3"))
            .env("UKK_HC_BIN", "__ukk_hc_missing_for_hardening_tests__")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command.output().expect("run ukk-host CLI")
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

struct RawClient {
    writer: std::os::unix::net::UnixStream,
    reader: BufReader<std::os::unix::net::UnixStream>,
}

impl RawClient {
    fn connect(socket: &Path) -> Self {
        let writer = std::os::unix::net::UnixStream::connect(socket).expect("connect Host socket");
        writer
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        writer
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let reader = BufReader::new(writer.try_clone().unwrap());
        Self { writer, reader }
    }

    fn send(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.writer.write_all(bytes)
    }

    fn read_line(&mut self) -> Option<String> {
        let mut line = String::new();
        match self.reader.read_line(&mut line) {
            Ok(0) => None,
            Ok(_) => Some(line),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                panic!("timed out waiting for Host response")
            }
            Err(error) => panic!("failed reading Host response: {error}"),
        }
    }

    fn ping(&mut self, id: u64) -> Value {
        let request = format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":{id},\"method\":\"host.ping\",\"params\":{{}}}}\n"
        );
        self.send(request.as_bytes()).unwrap();
        let line = self.read_line().expect("Host closed before ping response");
        serde_json::from_str(&line).expect("ping response is JSON")
    }
}

#[test]
fn socket_directory_and_socket_are_private() {
    let workspace = Workspace::new("modes");
    let _host = workspace.start();
    let directory = fs::symlink_metadata(&workspace.root).unwrap();
    assert_eq!(directory.permissions().mode() & 0o777, 0o700);
    let socket = fs::symlink_metadata(&workspace.socket).unwrap();
    assert!(socket.file_type().is_socket());
    assert_eq!(socket.permissions().mode() & 0o777, 0o600);

    let status = workspace.run_cli("status");
    assert!(status.status.success());
    assert!(
        String::from_utf8_lossy(&status.stdout).contains("live Host accepted"),
        "status must diagnose a live endpoint: {}",
        String::from_utf8_lossy(&status.stdout)
    );
}

#[test]
fn rejects_insecure_socket_directory_without_creating_a_socket() {
    let workspace = Workspace::new("permissions");
    let insecure = workspace.root.join("insecure");
    fs::create_dir(&insecure).unwrap();
    fs::set_permissions(&insecure, fs::Permissions::from_mode(0o755)).unwrap();
    let socket = insecure.join("agent-host.sock");
    let output = Command::new(env!("CARGO_BIN_EXE_ukk-host"))
        .arg("serve")
        .env("UKK_SOCKET", &socket)
        .env("UKK_DB_PATH", workspace.root.join("workspace.sqlite3"))
        .env("UKK_HC_BIN", "__ukk_hc_missing_for_hardening_tests__")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .expect("run ukk-host against an insecure directory");
    assert!(
        !output.status.success(),
        "Host must reject an insecure path"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("0755"), "unexpected diagnostic: {stderr}");
    assert!(stderr.contains("0700"), "unexpected diagnostic: {stderr}");
    assert!(
        !socket.exists(),
        "no socket may be created in a loose directory"
    );
}

#[test]
fn rejects_foreign_file_at_the_socket_path() {
    let workspace = Workspace::new("foreign");
    fs::write(&workspace.socket, b"do not delete").unwrap();
    let output = workspace
        .command()
        .stderr(Stdio::piped())
        .output()
        .expect("run ukk-host against a foreign file");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("not a Unix socket"),
        "unexpected diagnostic: {stderr}"
    );
    assert_eq!(fs::read(&workspace.socket).unwrap(), b"do not delete");
}

#[test]
fn oversized_request_is_rejected_and_host_stays_healthy() {
    let workspace = Workspace::new("oversize");
    let _host = workspace.start();
    let mut client = RawClient::connect(&workspace.socket);
    // More than MAX_REQUEST_BYTES without a terminating newline.
    let _ = client.send(&vec![b'a'; 70 * 1024]);
    assert!(
        client.read_line().is_none(),
        "oversized frame must close the connection, not produce a reply"
    );
    assert!(
        HostClient::new(&workspace.socket).ping().unwrap(),
        "Host must remain available after rejecting one oversized frame"
    );
}

#[test]
fn invalid_json_is_a_parse_error_and_connection_recovers() {
    let workspace = Workspace::new("framing");
    let _host = workspace.start();
    let mut client = RawClient::connect(&workspace.socket);
    client.send(b"{not json}\n").unwrap();
    let response: Value = serde_json::from_str(&client.read_line().unwrap()).unwrap();
    assert_eq!(response["error"]["code"], -32700);
    assert!(response["id"].is_null());
    let ping = client.ping(2);
    assert_eq!(ping["result"]["status"], "ok");
}

#[test]
fn unauthorized_workspace_mutation_is_denied() {
    let workspace = Workspace::new("auth");
    let _host = workspace.start();
    let mut client = RawClient::connect(&workspace.socket);
    client
        .send(
            b"{\"jsonrpc\":\"2.0\",\"id\":9,\"method\":\"workspace.session.create\",\
              \"params\":{\"request_id\":\"r1\",\"space_id\":\"s1\",\"title\":\"x\"}}\n",
        )
        .unwrap();
    let response: Value = serde_json::from_str(&client.read_line().unwrap()).unwrap();
    assert_eq!(response["error"]["code"], -32030);
    // Read-only methods remain usable without the capability.
    assert_eq!(client.ping(10)["result"]["status"], "ok");
}

#[test]
fn per_connection_request_quota_closes_the_stream() {
    let workspace = Workspace::new("quota");
    let _host = workspace.start();
    let mut client = RawClient::connect(&workspace.socket);
    for id in 0..MAX_REQUESTS_PER_CONNECTION {
        let response = client.ping(id as u64);
        assert_eq!(response["result"]["status"], "ok");
    }
    assert!(
        client.read_line().is_none(),
        "Host must close a stream after {MAX_REQUESTS_PER_CONNECTION} requests"
    );
    assert!(
        HostClient::new(&workspace.socket).ping().unwrap(),
        "quota exhaustion must not damage the Host"
    );
}

#[test]
fn concurrent_connection_limit_is_enforced() {
    let workspace = Workspace::new("connections");
    let _host = workspace.start();
    let mut held = Vec::new();
    for id in 0..MAX_CONNECTIONS {
        let mut client = RawClient::connect(&workspace.socket);
        // Complete one request so the server has definitely accepted and
        // counted this connection before moving on.
        assert_eq!(client.ping(id as u64)["result"]["status"], "ok");
        held.push(client);
    }
    let mut excess = RawClient::connect(&workspace.socket);
    let response: Value = serde_json::from_str(&excess.read_line().unwrap()).unwrap();
    assert_eq!(response["error"]["code"], -32029);
    assert!(
        HostClient::new(&workspace.socket).ping().is_err(),
        "the excess connection must not displace an accepted slot"
    );
    drop(held);
    // Capacity is released by the RAII slot as clients disconnect.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if HostClient::new(&workspace.socket).ping().unwrap_or(false) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "connection slots were not released"
        );
        thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn live_socket_is_never_replaced_by_a_second_host() {
    let workspace = Workspace::new("live-refusal");
    let host = workspace.start();
    let output = workspace
        .command()
        .stderr(Stdio::piped())
        .output()
        .expect("run second Host against a live socket");
    assert!(!output.status.success(), "second Host must refuse to start");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("another ukk Host"),
        "unexpected diagnostic: {stderr}"
    );
    assert!(
        HostClient::new(&workspace.socket).ping().unwrap(),
        "the live Host socket must remain untouched"
    );
    drop(host);
}

#[test]
fn sigterm_leaves_a_stale_socket_and_restart_recovers_it() {
    let workspace = Workspace::new("restart");
    let mut host = workspace.start();
    let socket_metadata = fs::symlink_metadata(&workspace.socket).unwrap();
    assert!(socket_metadata.file_type().is_socket());

    // Default Unix SIGTERM termination (no shutdown handler); the point is
    // that the socket file is left behind and must be recovered safely.
    let _status = host.terminate_and_wait();
    let stale = fs::symlink_metadata(&workspace.socket).unwrap();
    assert!(
        stale.file_type().is_socket(),
        "the socket file remains after a graceful Host stop"
    );

    let status_output = workspace.run_cli("status");
    assert!(status_output.status.success());
    let status_stdout = String::from_utf8_lossy(&status_output.stdout);
    assert!(
        status_stdout.contains("stale"),
        "status must diagnose the stale socket: {status_stdout}"
    );

    // A fresh Host verifies liveness, removes the stale socket and binds.
    let _restarted = workspace.start();
    assert_eq!(
        fs::symlink_metadata(&workspace.socket)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert!(HostClient::new(&workspace.socket).ping().unwrap());

    // And `cleanup` refuses to remove a live socket.
    let cleanup = workspace.run_cli("cleanup");
    assert!(!cleanup.status.success());
    let stderr = String::from_utf8_lossy(&cleanup.stderr);
    assert!(
        stderr.contains("another ukk Host") || stderr.contains("live ukk Host"),
        "unexpected cleanup diagnostic: {stderr}"
    );
    assert!(workspace.socket.exists());
}

#[test]
fn explicit_cleanup_removes_only_a_verified_stale_socket() {
    let workspace = Workspace::new("cleanup");
    // Bind then drop: the socket file is stale, with no Host process.
    let listener = std::os::unix::net::UnixListener::bind(&workspace.socket).unwrap();
    fs::set_permissions(&workspace.socket, fs::Permissions::from_mode(0o600)).unwrap();
    drop(listener);
    assert!(workspace.socket.exists());
    let output = workspace.run_cli("cleanup");
    assert!(output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("removed stale socket"),
        "unexpected output: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(!workspace.socket.exists());
    let _host = workspace.start();
}

#[test]
fn cleanup_refuses_an_unrelated_file() {
    let workspace = Workspace::new("cleanup-foreign");
    fs::write(&workspace.socket, b"keep me").unwrap();
    let output = workspace.run_cli("cleanup");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("not a Unix socket"));
    assert_eq!(fs::read(&workspace.socket).unwrap(), b"keep me");
}
