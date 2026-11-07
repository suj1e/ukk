//! A black-box test of the hc connect JSON-line transport without requiring
//! the real daemon in CI. Real herdr-core integration remains a Mac test.
#![cfg(unix)]

use serde_json::{json, Value};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use ukk_hc_client::{HcClient, HcError};

struct MockHc {
    dir: PathBuf,
    binary: PathBuf,
}

impl MockHc {
    fn new() -> Self {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("ukk-hc-mock-{}-{unique}", std::process::id()));
        fs::create_dir(&dir).expect("create isolated mock directory");
        let binary = dir.join("mock-hc");
        fs::write(
            &binary,
            r#"#!/usr/bin/env python3
import json
import sys

if sys.argv[1:] != ["connect"]:
    sys.exit(2)

request = json.loads(sys.stdin.readline())
assert request["jsonrpc"] == "2.0"
assert request["id"] == 1

if request["method"] == "terminal.list":
    print(json.dumps({
        "jsonrpc": "2.0", "id": 1,
        "result": {"terminals": [
            {"id": "t3", "name": "codex"},
            {"id": "t4", "name": "claude"}
        ], "hidden_finished": 0}
    }), flush=True)
elif request["method"] == "events.subscribe":
    print(json.dumps({"jsonrpc": "2.0", "id": 1,
        "result": {"seq": 10, "terminals": []}}), flush=True)
    print(json.dumps({"jsonrpc": "2.0", "method": "event",
        "params": {"seq": 11, "type": "created",
                   "terminal": "t3", "data": {"id": "t3"}}}), flush=True)
    print(json.dumps({"jsonrpc": "2.0", "method": "missed",
        "params": {"seq": 18, "terminals": []}}), flush=True)
else:
    print(json.dumps({"jsonrpc": "2.0", "id": 1,
        "error": {"code": -32601, "message": "not found"}}), flush=True)
"#,
        )
        .expect("write mock executable");
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
        Self { dir, binary }
    }

    fn client(&self) -> HcClient {
        HcClient::new(self.binary.as_os_str())
    }
}

impl Drop for MockHc {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn lists_multiple_terminals_through_hc_connect() {
    let mock = MockHc::new();
    let result = mock
        .client()
        .call("terminal.list", json!({}))
        .expect("real JSON-RPC round-trip to mock hc");
    assert_eq!(result["terminals"].as_array().unwrap().len(), 2);
    assert_eq!(result["terminals"][1]["name"], "claude");
}

#[test]
fn watches_snapshot_event_and_missed() {
    let mock = MockHc::new();
    let mut received: Vec<Value> = Vec::new();
    let final_result = mock.client().watch(|message| {
        received.push(message);
        Ok(())
    });
    // A finite mock closes stdout. A live core should stream indefinitely.
    assert!(matches!(final_result, Err(HcError::Protocol(_))));
    assert_eq!(received.len(), 3);
    assert_eq!(received[0]["kind"], "snapshot");
    assert_eq!(received[0]["data"]["seq"], 10);
    assert_eq!(received[1]["data"]["method"], "event");
    assert_eq!(received[2]["data"]["method"], "missed");
}
