//! End-to-end local integration test: a mock "hc connect" JSON stream
//! drives ukk-host's subscription broker and the public local IPC client.
//! Real herdr-core and native macOS remain separate acceptance tests.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use ukk_host_client::{HostClient, LiveHostState};

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
    let directory = std::env::temp_dir().join(format!(
        "ukk-broker-e2e-{}-{unique}",
        std::process::id()
    ));
    fs::create_dir(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    let socket = directory.join("agent.sock");
    let hc_bin = directory.join("mock-hc");
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
