//! End-to-end local integration test: a mock "hc connect" JSON stream
//! drives ukk-host's subscription broker and the public local IPC client.
//! Real herdr-core and native macOS remain separate acceptance tests.
#![cfg(unix)]

use serde_json::json;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use ukk_domain::new_id;
use ukk_host_client::{HostClient, LiveHostState};
use ukk_store::Store;

struct Fixture {
    child: Child,
    directory: std::path::PathBuf,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_dir_all(&self.directory);
    }
}

#[test]
fn host_tracks_live_agent_spawn_and_status_through_socket() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory =
        std::env::temp_dir().join(format!("ukk-broker-e2e-{}-{unique}", std::process::id()));
    fs::create_dir(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    let socket = directory.join("agent.sock");
    let hc_bin = directory.join("mock-hc");
    let db = directory.join("workspace.sqlite3");
    let mut store = Store::open(&db).unwrap();
    let space = store.create_space("ukk", "/src/ukk").unwrap();
    let session = store.create_session(&space.id, "Live Agent sync").unwrap();
    drop(store);
    fs::write(
        &hc_bin,
        r#"#!/usr/bin/env python3
import json
import sys
import time

assert sys.argv[1:] == ["connect"]
request = json.loads(sys.stdin.readline())
assert request["method"] == "events.subscribe"
print(json.dumps({
    "jsonrpc": "2.0", "id": 1,
    "result": {"seq": 4, "terminals": [{
        "address": "t3", "state": "live", "cwd": "/code",
        "argv": ["codex"], "name": "Agent A", "parent": None,
        "status": {"state": "working"}
    }]}
}), flush=True)
time.sleep(1.2)
print(json.dumps({
    "jsonrpc": "2.0", "method": "event",
    "params": {"seq": 5, "type": "created", "terminal": "t4",
    "data": {
        "address": "t4", "state": "live", "cwd": "/code",
        "argv": ["claude"], "name": "Agent B",
        "parent": "t3", "status": None
    }}
}), flush=True)
time.sleep(0.8)
print(json.dumps({
    "jsonrpc": "2.0", "method": "event",
    "params": {"seq": 6, "type": "status", "terminal": "t4",
    "data": {"status": {"state": "blocked"}, "reported": True, "records": {}}}
}), flush=True)
time.sleep(3)
"#,
    )
    .unwrap();
    fs::set_permissions(&hc_bin, fs::Permissions::from_mode(0o700)).unwrap();

    let child = Command::new(env!("CARGO_BIN_EXE_ukk-host"))
        .arg("serve")
        .env("UKK_HC_BIN", &hc_bin)
        .env("UKK_SOCKET", &socket)
        .env("UKK_DB_PATH", &db)
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("start host binary");
    let _fixture = Fixture { child, directory };
    let client = HostClient::new(&socket);

    let deadline = Instant::now() + Duration::from_secs(5);
    let initial: LiveHostState = loop {
        if Instant::now() > deadline {
            panic!("Host never published the first hc snapshot");
        }
        match client.host_snapshot() {
            Ok(state) if state.core_connected && state.snapshot.terminals.len() == 1 => {
                break state;
            }
            _ => thread::sleep(Duration::from_millis(20)),
        }
    };
    assert_eq!(initial.snapshot.terminals[0].address, "t3");
    let workspace = client
        .workspace_snapshot()
        .expect("read persisted Session from same Host");
    assert_eq!(workspace.spaces[0].id, space.id);
    assert_eq!(workspace.sessions[0].id, session.id);
    assert_eq!(workspace.layouts[0].root.leaves().len(), 1);

    // A separate SQLite writer (such as a CLI process) should wake the
    // Desktop's independent metadata subscription without manual Refresh.
    let initial_workspace = client.workspace_state().unwrap();
    let observer = client.clone();
    let db_identity = initial_workspace.db_epoch.clone();
    let cursor = initial_workspace.revision;
    let waiter = thread::spawn(move || observer.wait_for_workspace_change(&db_identity, cursor));
    thread::sleep(Duration::from_millis(80));
    Store::open(&db)
        .unwrap()
        .create_space("Second project", "/src/other")
        .unwrap();
    let changed_workspace = waiter.join().unwrap().unwrap();
    assert!(changed_workspace.revision > initial_workspace.revision);
    assert_eq!(changed_workspace.snapshot.unwrap().spaces.len(), 2);

    // User-authorized local edits go through the SAME Unix IPC as the UI.
    let request_id = new_id();
    let params = json!({
        "request_id":request_id,
        "space_id":space.id.clone(),
        "title":"Created from Desktop"
    });
    let created = client
        .local_workspace_mutation("workspace.session.create", params.clone())
        .expect("create a Session over authenticated Host IPC");
    let session_id = created["id"].as_str().unwrap().to_owned();
    let repeated = client
        .local_workspace_mutation("workspace.session.create", params)
        .expect("retry must be idempotent");
    assert_eq!(repeated["id"], created["id"]);
    let fresh = client.workspace_snapshot().unwrap();
    assert_eq!(fresh.sessions.len(), 2);
    let root = fresh
        .layouts
        .iter()
        .find(|l| l.session_id == session_id)
        .unwrap();
    let first_pane = root.root.id().to_owned();

    let split = client
        .local_workspace_mutation(
            "workspace.pane.split",
            json!({
                "session_id":session_id, "pane_id":first_pane, "axis":"horizontal",
                "content":{"type":"diff"}, "expected_revision":0
            }),
        )
        .unwrap();
    assert_eq!(split["revision"], 1);
    let split_id = split["root"]["id"].as_str().unwrap().to_owned();

    let resized = client
        .local_workspace_mutation(
            "workspace.pane.resize",
            json!({
                "session_id":session_id,"split_id":split_id,"ratio":0.7,"expected_revision":1
            }),
        )
        .unwrap();
    assert_eq!(resized["revision"], 2);
    let second_id = resized["root"]["second"]["id"].as_str().unwrap().to_owned();

    let stale = client.local_workspace_mutation(
        "workspace.pane.close",
        json!({
            "session_id":session_id,"pane_id":second_id,"expected_revision":1
        }),
    );
    assert!(
        stale.is_err(),
        "stale revision must not overwrite another view"
    );

    let closed = client
        .local_workspace_mutation(
            "workspace.pane.close",
            json!({
                "session_id":session_id,"pane_id":second_id,"expected_revision":2
            }),
        )
        .unwrap();
    assert_eq!(closed["revision"], 3);
    assert_eq!(
        client
            .workspace_snapshot()
            .unwrap()
            .layouts
            .iter()
            .find(|l| l.session_id == session_id)
            .unwrap()
            .root
            .leaves()
            .len(),
        1
    );

    let spawned = client
        .wait_for_change(&initial.host_epoch, initial.revision)
        .expect("long poll a newly created child agent");
    assert_eq!(spawned.snapshot.terminals.len(), 2);
    assert_eq!(spawned.snapshot.hierarchy()[1].1, 1);

    let updated = if spawned.snapshot.terminals[1].state_label() == "Needs you" {
        spawned
    } else {
        client
            .wait_for_change(&spawned.host_epoch, spawned.revision)
            .expect("receive child agent status update")
    };
    assert_eq!(updated.snapshot.terminals[1].state_label(), "Needs you");
    assert!(updated.revision > initial.revision);
}
