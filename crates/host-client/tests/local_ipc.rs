#![cfg(unix)]
use serde_json::{json, Value};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use ukk_host_client::{HostClient, HostError};

#[test]
fn end_to_end_local_snapshot() {
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
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

static NEXT_SOCKET: AtomicU64 = AtomicU64::new(0);

fn short_socket_directory(tag: &str) -> std::path::PathBuf {
    // macOS temp dirs are long; keep the total path below SUN_LEN.
    let unique = NEXT_SOCKET.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("ukk{tag}-{}-{unique}", std::process::id()))
}

#[test]
fn read_deadline_bounds_a_silent_host() {
    let dir = short_socket_directory("sd");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("host.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let handler = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        thread::sleep(Duration::from_secs(4));
        drop(stream);
    });
    let started = Instant::now();
    let error = HostClient::new(&path)
        .ping()
        .expect_err("a silent Host must not block forever");
    let elapsed = started.elapsed();
    assert!(elapsed < Duration::from_secs(5), "elapsed {elapsed:?}");
    assert!(
        matches!(error, HostError::Io(_) | HostError::Protocol(_)),
        "unexpected error: {error}"
    );
    handler.join().unwrap();
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn reply_size_is_bounded() {
    let dir = short_socket_directory("os");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("host.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let handler = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = String::new();
        BufReader::new(stream.try_clone().unwrap())
            .read_line(&mut request)
            .unwrap();
        // Exceed the client's 4 MiB reply cap without ever terminating a line.
        let block = vec![b'a'; 64 * 1024];
        for _ in 0..(5 * 1024 * 1024 / block.len() + 1) {
            if stream.write_all(&block).is_err() {
                break; // the client has already enforced its reply cap
            }
        }
        let _ = stream.flush();
    });
    let error = HostClient::new(&path)
        .ping()
        .expect_err("oversized reply must be rejected");
    assert!(
        matches!(&error, HostError::Protocol(message) if message.contains("size cap")),
        "unexpected error: {error}"
    );
    handler.join().unwrap();
    fs::remove_dir_all(&dir).unwrap();
}
