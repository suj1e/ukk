//! Native Windows Host API integration: a secure per-user Named Pipe,
//! not TCP or the mobile relay. No real hc binary is needed for ping/offline.
#![cfg(windows)]

use serde_json::json;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use ukk_domain::new_id;
use ukk_host_client::HostClient;
use ukk_store::Store;

/// The Named Pipe name is per-user, so Host tests in this binary must not run
/// concurrently (the first-instance flag correctly refuses a second Host).
static HOST_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn serial_host_test() -> std::sync::MutexGuard<'static, ()> {
    HOST_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn windows_host_pipe_allows_read_only_local_snapshot() {
    let _serial = serial_host_test();
    // On a clean CI runner there is no preexisting ukk-host process.
    // The first-instance flag ensures an existing pipe cannot be hijacked.
    let root = std::env::temp_dir().join(format!("ukk-win-ipc-{}", new_id()));
    std::fs::create_dir_all(&root).unwrap();
    let db = root.join("workspace.sqlite3");
    let mut store = Store::open(&db).unwrap();
    let space = store.create_space("Win Space", "C:/src/ukk").unwrap();
    let session = store.create_session(&space.id, "Windows Session").unwrap();
    drop(store);
    let child = Command::new(env!("CARGO_BIN_EXE_ukk-host"))
        .arg("serve")
        .env("UKK_HC_BIN", "__ukk_hc_not_installed_for_win_test__")
        .env("UKK_DB_PATH", &db)
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("start Windows Host");
    let mut guard = ChildGuard(child);
    let client = HostClient::from_env();
    let deadline = Instant::now() + Duration::from_secs(8);

    loop {
        if let Some(code) = guard.0.try_wait().expect("check Host process") {
            panic!("Windows Host exited before pipe became ready: {code}");
        }
        if client.ping().unwrap_or(false) {
            break;
        }
        if Instant::now() > deadline {
            panic!("Windows Host Named Pipe did not become available");
        }
        thread::sleep(Duration::from_millis(80));
    }

    // The next listener must be available while an earlier connection is
    // being handled. Two independent UI observers should both be able to
    // complete a read-only ping without sharing handles.
    let first = thread::spawn(|| HostClient::from_env().ping());
    let second = thread::spawn(|| HostClient::from_env().ping());
    assert!(first.join().unwrap().unwrap());
    assert!(second.join().unwrap().unwrap());

    let state = client.host_snapshot().expect("read initial Host snapshot");
    assert!(!state.core_connected, "mock hc is deliberately unavailable");
    assert_eq!(state.snapshot.terminals.len(), 0);
    let workspace = client.workspace_snapshot().unwrap();
    assert_eq!(workspace.spaces[0].id, space.id);
    assert_eq!(workspace.sessions[0].id, session.id);

    let request_id = new_id();
    let created = client
        .local_workspace_mutation(
            "workspace.session.create",
            json!({
                "request_id":request_id,
                "space_id":space.id,
                "title":"Windows UI Session"
            }),
        )
        .expect("local user can create a Session via protected Pipe");
    assert!(created["id"].is_string());
    let repeated = client
        .local_workspace_mutation(
            "workspace.session.create",
            json!({
                "request_id":request_id,
                "space_id":space.id,
                "title":"Windows UI Session"
            }),
        )
        .unwrap();
    assert_eq!(created["id"], repeated["id"]);
    assert_eq!(client.workspace_snapshot().unwrap().sessions.len(), 2);

    // Epoch mismatch must return immediately, rather than sleeping 10 sec.
    let recovered = client
        .wait_for_change("a-previous-host-epoch", 0)
        .expect("read state via host.wait");
    assert_eq!(recovered.host_epoch, state.host_epoch);
    assert_eq!(recovered.snapshot.terminals.len(), 0);
    drop(guard);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn windows_second_host_is_rejected_while_first_is_live() {
    let _serial = serial_host_test();
    let root = std::env::temp_dir().join(format!("ukk-win-live-{}", new_id()));
    std::fs::create_dir_all(&root).unwrap();
    let db = root.join("workspace.sqlite3");
    drop(Store::open(&db).unwrap());
    let host_environment = |command: &mut Command| {
        command
            .arg("serve")
            .env("UKK_HC_BIN", "__ukk_hc_not_installed_for_win_test__")
            .env("UKK_DB_PATH", &db);
    };
    let mut first = Command::new(env!("CARGO_BIN_EXE_ukk-host"));
    host_environment(&mut first);
    let first = first
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("start first Windows Host");
    let mut guard = ChildGuard(first);
    let client = HostClient::from_env();
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        if let Some(code) = guard.0.try_wait().expect("check first Host") {
            panic!("first Windows Host exited early: {code}");
        }
        if client.ping().unwrap_or(false) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "first Windows Host did not start"
        );
        thread::sleep(Duration::from_millis(80));
    }

    // FILE_FLAG_FIRST_PIPE_INSTANCE must make a second Host fail, never
    // silently share or replace the live endpoint.
    let mut second = Command::new(env!("CARGO_BIN_EXE_ukk-host"));
    host_environment(&mut second);
    let second = second
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start second Windows Host");
    let output = second.wait_with_output().expect("wait for second Host");
    assert!(
        !output.status.success(),
        "a second Windows Host must be rejected while the first is live"
    );
    assert!(
        client.ping().unwrap_or(false),
        "the live Host pipe must survive the rejected second Host"
    );
    drop(guard);
    std::fs::remove_dir_all(root).unwrap();
}
