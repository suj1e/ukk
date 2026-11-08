//! End-to-end bridge test: a structured herdr-core terminal status reported
//! through the Host runtime broker becomes a durable typed protocol event on
//! the local stream.
//!
//! This is deliberately narrow: the mock only reports `status.state=working`.
//! Approval inference is out of scope and is explicitly mapped to `unknown`
//! by the bridge.
#![cfg(unix)]

use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use ukk_domain::new_id;
use ukk_protocol::{
    AgentStatus, ClientHello, ClientMessage, DurableCursor, Event, Limits, RequestId,
    ServerMessage, SubscribeRequest, CURRENT_GENERATION,
};
use ukk_store::Store;

struct Fixture {
    child: Child,
    directory: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_dir_all(&self.directory);
    }
}

struct Client {
    stream: UnixStream,
    buffer: Vec<u8>,
    limits: Limits,
}

impl Client {
    fn from_stream(stream: UnixStream) -> Self {
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        Self {
            stream,
            buffer: Vec::new(),
            limits: Limits::default(),
        }
    }

    fn send(&mut self, message: &ClientMessage) {
        let frame = message.encode(&self.limits).unwrap();
        let length = u32::try_from(frame.len()).unwrap();
        self.stream.write_all(&length.to_be_bytes()).unwrap();
        self.stream.write_all(frame.as_bytes()).unwrap();
    }

    fn recv_timeout(&mut self, timeout: Duration) -> Option<ServerMessage> {
        self.stream.set_read_timeout(Some(timeout)).unwrap();
        let result = loop {
            if let Some(message) = self.take_frame() {
                break Some(message);
            }
            let mut chunk = [0u8; 8192];
            match self.stream.read(&mut chunk) {
                Ok(0) => break None,
                Ok(read) => self.buffer.extend_from_slice(&chunk[..read]),
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    break None
                }
                Err(error) => panic!("protocol read failed: {error}"),
            }
        };
        self.stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        result
    }

    fn recv(&mut self) -> ServerMessage {
        self.recv_timeout(Duration::from_secs(5))
            .expect("protocol frame before the deadline")
    }

    fn take_frame(&mut self) -> Option<ServerMessage> {
        if self.buffer.len() < 4 {
            return None;
        }
        let mut prefix = [0u8; 4];
        prefix.copy_from_slice(&self.buffer[..4]);
        let length = u32::from_be_bytes(prefix) as usize;
        let total = 4 + length;
        if self.buffer.len() < total {
            return None;
        }
        let body = self.buffer[4..total].to_vec();
        self.buffer.drain(..total);
        let text = std::str::from_utf8(&body).unwrap();
        Some(ServerMessage::decode(text, &self.limits).unwrap())
    }
}

#[test]
fn broker_status_change_reaches_the_durable_stream() {
    let unique = new_id();
    let directory = PathBuf::from("/tmp").join(format!(
        "ukk-pe-bridge-{}-{}",
        std::process::id(),
        &unique[..8]
    ));
    fs::create_dir_all(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    let socket = directory.join("agent-host.sock");
    let protocol_socket = directory.join("agent-host.protocol");
    let database = directory.join("workspace.sqlite3");
    let hc_bin = directory.join("mock-hc");

    let (session_id, agent_run_id) = {
        let mut store = Store::open(&database).unwrap();
        let space = store.create_space("ukk", "/src/ukk").unwrap();
        let session = store.create_session(&space.id, "Bridge Session").unwrap();
        let run = store
            .bind_agent(&session.id, "core-1", "t3", "codex", None)
            .unwrap();
        (session.id, run.id)
    };

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
    "result": {"seq": 1, "core": {"id": "core-1"}, "terminals": [{
        "address": "t3", "state": "live", "cwd": "/code",
        "argv": ["codex"], "name": "Bound Agent", "parent": None,
        "status": {"state": "working"}
    }]}
}), flush=True)
time.sleep(6)
"#,
    )
    .unwrap();
    fs::set_permissions(&hc_bin, fs::Permissions::from_mode(0o700)).unwrap();

    let child = Command::new(env!("CARGO_BIN_EXE_ukk-host"))
        .arg("serve")
        .env("UKK_SOCKET", &socket)
        .env("UKK_DB_PATH", &database)
        .env("UKK_HC_BIN", &hc_bin)
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("start ukk-host serve");
    let _fixture = Fixture { child, directory };

    let deadline = Instant::now() + Duration::from_secs(8);
    let mut client = loop {
        match UnixStream::connect(&protocol_socket) {
            Ok(stream) => break Client::from_stream(stream),
            Err(_) if Instant::now() < deadline => thread::sleep(Duration::from_millis(50)),
            Err(error) => panic!("protocol endpoint never became usable: {error}"),
        }
    };

    client.send(&ClientMessage::Hello(ClientHello::local(
        RequestId::new_random(),
        vec![CURRENT_GENERATION],
    )));
    match client.recv() {
        ServerMessage::Hello(_) => {}
        other => panic!("expected hello, got {other:?}"),
    }
    client.send(&ClientMessage::Subscribe(SubscribeRequest::new(
        CURRENT_GENERATION,
        ukk_protocol::StreamId::new("workspace").unwrap(),
        DurableCursor::ZERO,
        16,
    )));

    let deadline = Instant::now() + Duration::from_secs(6);
    loop {
        assert!(
            Instant::now() < deadline,
            "broker status never reached the durable protocol stream"
        );
        match client.recv_timeout(Duration::from_millis(300)) {
            Some(ServerMessage::Event(event)) => match event.event {
                Event::AgentRunStatusChanged(changed) => {
                    assert_eq!(changed.session_id.as_str(), session_id);
                    assert_eq!(changed.agent_run_id.as_str(), agent_run_id);
                    assert_eq!(changed.status, AgentStatus::Running);
                    return;
                }
                other => panic!("unexpected durable event: {other:?}"),
            },
            Some(ServerMessage::Snapshot(_)) => continue,
            Some(other) => panic!("unexpected frame: {other:?}"),
            None => continue,
        }
    }
}
