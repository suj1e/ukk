//! Real-process acceptance for the local protocol stream.
//!
//! Existing in-crate tests exercise the endpoint module directly. This test
//! proves the shipped `ukk-host serve` path: it derives
//! `<socket dir>/agent-host.protocol`, binds it after the JSON-RPC socket,
//! hydrates the seeded SQLite database, and serves a length-prefixed
//! `hello`/`command`/`subscribe` exchange to a raw Unix socket client.
#![cfg(unix)]

use serde_json::json;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use ukk_domain::new_id;
use ukk_protocol::{
    ClientHello, ClientMessage, DurableCursor, Event, EventId, Limits, RequestId, ServerMessage,
    SessionCreated, StreamId, SubscribeRequest, CURRENT_GENERATION,
};
use ukk_store::Store;

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

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
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
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

    fn recv(&mut self) -> ServerMessage {
        loop {
            if self.buffer.len() >= 4 {
                let mut prefix = [0u8; 4];
                prefix.copy_from_slice(&self.buffer[..4]);
                let length = u32::from_be_bytes(prefix) as usize;
                let total = 4 + length;
                if self.buffer.len() >= total {
                    let body = self.buffer[4..total].to_vec();
                    self.buffer.drain(..total);
                    let text = std::str::from_utf8(&body).unwrap();
                    return ServerMessage::decode(text, &self.limits).unwrap();
                }
            }
            let mut chunk = [0u8; 8192];
            let read = self.stream.read(&mut chunk).expect("read protocol frame");
            assert!(read > 0, "protocol endpoint closed mid-frame");
            self.buffer.extend_from_slice(&chunk[..read]);
        }
    }
}

#[test]
fn serve_exposes_the_local_protocol_stream_next_to_the_rpc_socket() {
    // Keep the socket path under SUN_LEN on macOS.
    let unique = new_id();
    let directory = PathBuf::from("/tmp").join(format!(
        "ukk-pe-bin-{}-{}",
        std::process::id(),
        &unique[..8]
    ));
    fs::create_dir_all(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    let socket = directory.join("agent-host.sock");
    let protocol_socket = directory.join("agent-host.protocol");
    let database = directory.join("workspace.sqlite3");

    {
        let mut store = Store::open(&database).unwrap();
        let space = store.create_space("ukk", "/src/ukk").unwrap();
        // The local protocol plane is read-only in this slice, so the test
        // seeds one durable event directly and proves replay over the socket.
        let stream = StreamId::new("workspace").unwrap();
        store
            .append_event(
                &stream,
                EventId::new_random(),
                Event::SessionCreated(SessionCreated {
                    space_id: ukk_protocol::DomainId::new(&space.id).unwrap(),
                    session_id: ukk_protocol::DomainId::new(new_id()).unwrap(),
                    title: "Serve protocol Session".into(),
                    revision: 0,
                }),
                now(),
            )
            .unwrap();
    }

    let child = Command::new(env!("CARGO_BIN_EXE_ukk-host"))
        .arg("serve")
        .env("UKK_SOCKET", &socket)
        .env("UKK_DB_PATH", &database)
        .env("UKK_HC_BIN", "__ukk_hc_missing_for_protocol_test__")
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("start ukk-host serve");
    let _fixture = Fixture { child, directory };

    // The RPC socket is bound first; the protocol endpoint follows. Probe the
    // protocol socket directly so the test fails if `serve` forgets to start it.
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
    let hello = match client.recv() {
        ServerMessage::Hello(hello) => hello,
        other => panic!("expected hello, got {other:?}"),
    };
    assert_eq!(hello.protocol, CURRENT_GENERATION);
    assert_eq!(hello.streams[0].stream.as_str(), "workspace");
    assert_eq!(hello.streams[0].head, DurableCursor::from_raw(1));

    client.send(&ClientMessage::Subscribe(SubscribeRequest::new(
        CURRENT_GENERATION,
        ukk_protocol::StreamId::new("workspace").unwrap(),
        DurableCursor::ZERO,
        16,
    )));
    match client.recv() {
        ServerMessage::Event(event) => {
            assert_eq!(event.cursor.get(), 1);
            assert_eq!(
                serde_json::to_value(&event.event).unwrap()["event"],
                json!("session_created")
            );
        }
        other => panic!("expected a durable event, got {other:?}"),
    }
}
