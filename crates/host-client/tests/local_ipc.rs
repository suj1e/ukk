#![cfg(unix)]
use serde_json::{json, Value};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};
use ukk_host_client::HostClient;

#[test]
fn end_to_end_local_snapshot() {
    let n = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let dir = std::env::temp_dir().join(format!("ukk-host-ipc-{}-{n}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("host.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let handler = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut line = String::new();
        BufReader::new(stream.try_clone().unwrap())
            .read_line(&mut line)
            .unwrap();
        let request: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(request["method"], "terminal.list");
        let response = json!({
            "jsonrpc":"2.0","id":request["id"],
            "result":{"terminals":[
                {"address":"t17","state":"live","name":"run tests",
                 "cwd":"/work","argv":["codex"],"status":{"state":"blocked"}}
            ],"hidden_finished":0,"machines":[]}
        });
        writeln!(stream, "{response}").unwrap();
    });
    let snapshot = HostClient::new(&path).list_terminals().unwrap();
    handler.join().unwrap();
    fs::remove_dir_all(&dir).unwrap();
    assert_eq!(snapshot.terminals.len(), 1);
    assert_eq!(snapshot.terminals[0].address, "t17");
    assert_eq!(snapshot.terminals[0].state_label(), "Needs you");
}
