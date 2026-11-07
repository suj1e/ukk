//! Native Windows Host API integration: a secure per-user Named Pipe,
//! not TCP or the mobile relay. No real hc binary is needed for ping/offline.
#![cfg(windows)]

use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use ukk_host_client::HostClient;
use ukk_store::Store;
use ukk_domain::new_id;

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn windows_host_pipe_allows_read_only_local_snapshot() {
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

    // Epoch mismatch must return immediately, rather than sleeping 10 sec.
    let recovered = client
        .wait_for_change("a-previous-host-epoch", 0)
        .expect("read state via host.wait");
    assert_eq!(recovered.host_epoch, state.host_epoch);
    assert_eq!(recovered.snapshot.terminals.len(), 0);
    drop(guard);
    std::fs::remove_dir_all(root).unwrap();
}
