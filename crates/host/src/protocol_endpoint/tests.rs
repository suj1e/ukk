//! End-to-end tests over a real Unix socket and a real protocol client.
//!
//! Every test starts a listener on a private temporary directory, connects a
//! plain `UnixStream`, and exchanges length-prefixed frames. The Host side is
//! the production `handle_connection`/`accept_loop` path and a real SQLite
//! store, not a mock transport.

use super::*;
use crate::protocol_service::ProtocolHost;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use ukk_domain::new_id;
use ukk_protocol::{
    ClientHello, ClientMessage, Command, CommandEnvelope, CommandResult, DeviceClaim, DeviceId,
    DurableCursor, Event, EventEnvelope, IdempotencyKey, IdentityGeneration, KeyFingerprint,
    ProtocolGeneration, RequestId, Scope, ServerHello, ServerMessage, SessionCreate,
    SubscribeRequest, CURRENT_GENERATION,
};
use ukk_store::Store;

const WORKSPACE: &str = "workspace";

struct TestServer {
    directory: PathBuf,
    path: PathBuf,
    host: Arc<ProtocolHost>,
    stop: Arc<AtomicBool>,
    join: Option<thread::JoinHandle<()>>,
}

impl TestServer {
    fn start(host: ProtocolHost, plane: EndpointPlane) -> Self {
        // Keep the socket path well below SUN_LEN (104 bytes on macOS); the
        // platform temp dir alone can exceed it.
        let unique = new_id();
        let directory =
            PathBuf::from("/tmp").join(format!("ukk-pe-{}-{}", std::process::id(), &unique[..8]));
        fs::create_dir_all(&directory).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
        let path = directory.join("agent-host.protocol");
        let listener = UnixListener::bind(&path).unwrap();
        let host = Arc::new(host);
        let stop = Arc::new(AtomicBool::new(false));
        let join = {
            let host = Arc::clone(&host);
            let stop = Arc::clone(&stop);
            thread::spawn(move || accept_loop(listener, host, plane, Some(stop)))
        };
        Self {
            directory,
            path,
            host,
            stop,
            join: Some(join),
        }
    }

    fn client(&self) -> TestClient {
        TestClient::connect(&self.path)
    }

    fn host(&self) -> &ProtocolHost {
        &self.host
    }

    fn stop(&mut self) {
        if let Some(join) = self.join.take() {
            self.stop.store(true, Ordering::Release);
            let _ = join.join();
        }
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.stop();
        let _ = fs::remove_dir_all(&self.directory);
    }
}

struct TestClient {
    stream: UnixStream,
    buffer: Vec<u8>,
    limits: Limits,
}

impl TestClient {
    fn connect(path: &std::path::Path) -> Self {
        let stream = UnixStream::connect(path).expect("connect to protocol endpoint");
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
        self.send_raw(frame.as_bytes());
    }

    fn send_raw(&mut self, body: &[u8]) {
        self.send_length(u32::try_from(body.len()).unwrap());
        if !body.is_empty() {
            self.stream.write_all(body).unwrap();
        }
    }

    fn send_length(&mut self, length: u32) {
        self.stream.write_all(&length.to_be_bytes()).unwrap();
    }

    fn recv(&mut self) -> ServerMessage {
        loop {
            if let Some(message) = self.take_frame() {
                return message;
            }
            let mut chunk = [0u8; 8192];
            let read = self
                .stream
                .read(&mut chunk)
                .expect("read protocol frame before the deadline");
            if read == 0 {
                panic!("endpoint closed before a complete frame");
            }
            self.buffer.extend_from_slice(&chunk[..read]);
        }
    }

    fn expect_closed(&mut self) {
        let mut chunk = [0u8; 64];
        loop {
            match self.stream.read(&mut chunk) {
                Ok(0) => return,
                Ok(_) => continue,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    panic!("endpoint kept the connection open after a fatal frame")
                }
                Err(_) => return,
            }
        }
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
        let text = std::str::from_utf8(&body).expect("server frames are UTF-8 JSON");
        Some(ServerMessage::decode(text, &self.limits).expect("server frame decodes"))
    }
}

fn temporary_database() -> (PathBuf, PathBuf) {
    let directory = std::env::temp_dir().join(format!(
        "ukk-protocol-db-{}-{}",
        std::process::id(),
        new_id()
    ));
    fs::create_dir_all(&directory).unwrap();
    (directory.clone(), directory.join("workspace.sqlite3"))
}

fn start_remote_server(host: ProtocolHost, scopes: &[Scope]) -> (TestServer, String, String) {
    let device = new_id();
    let fingerprint = format!("sha256:{device}");
    host.register_test_device(&device, &fingerprint, scopes, now_ms() + 60_000)
        .unwrap();
    let server = TestServer::start(host, EndpointPlane::RemoteForTests);
    (server, device, fingerprint)
}

fn local_hello(client: &mut TestClient) -> ServerHello {
    client.send(&ClientMessage::Hello(ClientHello::local(
        RequestId::new_random(),
        vec![CURRENT_GENERATION],
    )));
    match client.recv() {
        ServerMessage::Hello(hello) => hello,
        other => panic!("expected hello, got {other:?}"),
    }
}

fn remote_hello(client: &mut TestClient, device: &str, fingerprint: &str) -> ServerHello {
    client.send(&ClientMessage::Hello(ClientHello::remote(
        RequestId::new_random(),
        vec![CURRENT_GENERATION],
        DeviceClaim {
            device_id: DeviceId::new(device).unwrap(),
            key_fingerprint: KeyFingerprint::new(fingerprint).unwrap(),
            generation: IdentityGeneration::FIRST,
        },
    )));
    match client.recv() {
        ServerMessage::Hello(hello) => hello,
        other => panic!("expected hello, got {other:?}"),
    }
}

fn subscribe(client: &mut TestClient, cursor: u64, max_batch: usize) {
    client.send(&ClientMessage::Subscribe(SubscribeRequest::new(
        CURRENT_GENERATION,
        ukk_protocol::StreamId::new(WORKSPACE).unwrap(),
        DurableCursor::from_raw(cursor),
        max_batch,
    )));
}

fn session_create(space_id: &str, request_id: &str, key: &str) -> ClientMessage {
    ClientMessage::Command(CommandEnvelope {
        protocol: CURRENT_GENERATION,
        request_id: RequestId::new(request_id).unwrap(),
        idempotency_key: IdempotencyKey::new(key).unwrap(),
        issued_at_ms: now_ms(),
        command: Command::SessionCreate(SessionCreate {
            space_id: ukk_protocol::DomainId::new(space_id).unwrap(),
            title: "Protocol Session".into(),
            expected_revision: None,
        }),
    })
}

fn seed_session_event(store: &mut Store, space_id: &str) {
    let stream = ukk_protocol::StreamId::new(WORKSPACE).unwrap();
    store.ensure_event_stream(&stream).unwrap();
    store
        .append_event(
            &stream,
            ukk_protocol::EventId::new_random(),
            Event::SessionCreated(ukk_protocol::SessionCreated {
                space_id: ukk_protocol::DomainId::new(space_id).unwrap(),
                session_id: ukk_protocol::DomainId::new(new_id()).unwrap(),
                title: "Seed Session".into(),
                revision: 0,
            }),
            now_ms(),
        )
        .unwrap();
}

fn expect_result(client: &mut TestClient, replayed: bool) -> CommandResult {
    match client.recv() {
        ServerMessage::Result(result) => {
            assert_eq!(result.replayed, replayed, "unexpected replay flag");
            result.result
        }
        other => panic!("expected a command result, got {other:?}"),
    }
}

fn expect_event(client: &mut TestClient) -> EventEnvelope {
    match client.recv() {
        ServerMessage::Event(event) => event,
        other => panic!("expected a durable event, got {other:?}"),
    }
}

fn expect_error(client: &mut TestClient) -> ukk_protocol::ServerError {
    match client.recv() {
        ServerMessage::Error(error) => error,
        other => panic!("expected a typed server error, got {other:?}"),
    }
}

fn completed_session(result: CommandResult) -> String {
    match result {
        CommandResult::Completed {
            detail: Some(session_id),
        } => session_id,
        other => panic!("expected a completed session result, got {other:?}"),
    }
}

#[test]
fn hello_negotiates_generation_and_rejects_incompatible_versions() {
    let (directory, path) = temporary_database();
    let mut store = Store::open(&path).unwrap();
    store.create_space("ukk", "/src/ukk").unwrap();
    let server = TestServer::start(ProtocolHost::open(store).unwrap(), EndpointPlane::Local);

    let mut client = server.client();
    let hello = local_hello(&mut client);
    assert_eq!(hello.protocol, CURRENT_GENERATION);
    assert_eq!(hello.streams.len(), 1);
    assert_eq!(hello.streams[0].stream.as_str(), WORKSPACE);
    assert_eq!(hello.streams[0].head, DurableCursor::ZERO);

    let mut incompatible = server.client();
    incompatible.send(&ClientMessage::Hello(ClientHello::local(
        RequestId::new_random(),
        vec![ProtocolGeneration::from_raw(2)],
    )));
    let error = expect_error(&mut incompatible);
    assert!(error.denial.is_none());
    assert!(error.message.contains("no common"), "{}", error.message);
    incompatible.expect_closed();

    drop(client);
    drop(server);
    fs::remove_dir_all(directory).ok();
}

#[test]
fn partial_frames_are_tracked_for_the_idle_deadline() {
    let (mut reader, mut writer) = UnixStream::pair().unwrap();
    reader
        .set_read_timeout(Some(Duration::from_millis(10)))
        .unwrap();
    let mut decoder = FrameDecoder::new();
    // Prefix only: the body never arrives before the poll deadline.
    writer.write_all(&5u32.to_be_bytes()).unwrap();
    match decoder.poll(&mut reader, 64) {
        Ok(FrameRead::Empty) => assert!(
            decoder.has_partial_frame(),
            "a half-received frame must be visible to the idle check"
        ),
        other => panic!("expected an empty poll, got {other:?}"),
    }
    writer.write_all(b"hello").unwrap();
    match decoder.poll(&mut reader, 64) {
        Ok(FrameRead::Frame(body)) => assert_eq!(body, b"hello"),
        other => panic!("expected the completed frame, got {other:?}"),
    }
    assert!(!decoder.has_partial_frame());
}

#[test]
fn outbound_queue_is_bounded() {
    let limits = Limits {
        max_queue_depth: 2,
        ..Limits::default()
    };
    let mut outbound = Outbound::new(&limits);
    assert!(outbound.push(vec![1]));
    assert!(outbound.push(vec![2]));
    assert!(
        !outbound.push(vec![3]),
        "a non-draining client must hit the queue bound, not grow Host memory"
    );
}

#[test]
fn subscribe_replays_batches_snapshots_pruned_history_and_streams_live() {
    let (directory, path) = temporary_database();
    let mut store = Store::open(&path).unwrap();
    let space = store.create_space("ukk", "/src/ukk").unwrap();
    let host = ProtocolHost::open(store).unwrap();
    let (server, device, fingerprint) =
        start_remote_server(host, &[Scope::SessionObserve, Scope::SessionManage]);

    let mut writer = server.client();
    remote_hello(&mut writer, &device, &fingerprint);
    for _ in 0..3 {
        writer.send(&session_create(&space.id, &new_id(), &new_id()));
        completed_session(expect_result(&mut writer, false));
    }

    let mut reader = server.client();
    remote_hello(&mut reader, &device, &fingerprint);
    reader.send(&ClientMessage::Subscribe(SubscribeRequest::new(
        CURRENT_GENERATION,
        ukk_protocol::StreamId::new(WORKSPACE).unwrap(),
        DurableCursor::ZERO,
        2,
    )));
    for expected in 1..=3 {
        let event = expect_event(&mut reader);
        assert_eq!(event.cursor.get(), expected, "replay must stay ordered");
    }

    // A cursor ahead of the durable head is a typed error, never a reset.
    subscribe(&mut reader, 4, 16);
    let error = expect_error(&mut reader);
    assert!(error.message.contains("beyond"), "{}", error.message);

    // Retention advanced past the requested cursor: a cursor-bound snapshot
    // replaces history instead of silently dropping events.
    server.host().prune_test_events(2).unwrap();
    subscribe(&mut reader, 0, 16);
    match reader.recv() {
        ServerMessage::Snapshot(snapshot) => {
            assert_eq!(snapshot.stream.as_str(), WORKSPACE);
            assert_eq!(snapshot.at_cursor.get(), 3);
            assert!(snapshot.body.is_object());
        }
        other => panic!("expected a snapshot, got {other:?}"),
    }

    // Still at the retention boundary: events after the floor replay.
    subscribe(&mut reader, 2, 16);
    assert_eq!(expect_event(&mut reader).cursor.get(), 3);

    // And a new Host event reaches the already-open subscription live.
    writer.send(&session_create(&space.id, &new_id(), &new_id()));
    completed_session(expect_result(&mut writer, false));
    let live = expect_event(&mut reader);
    assert_eq!(live.cursor.get(), 4);
    assert!(matches!(live.event, Event::SessionCreated(_)));

    drop(reader);
    drop(writer);
    drop(server);
    fs::remove_dir_all(directory).ok();
}

#[test]
fn command_idempotency_never_executes_twice() {
    let (directory, path) = temporary_database();
    let mut store = Store::open(&path).unwrap();
    let space = store.create_space("ukk", "/src/ukk").unwrap();
    let host = ProtocolHost::open(store).unwrap();
    let (server, device, fingerprint) = start_remote_server(host, &[Scope::SessionManage]);

    let mut client = server.client();
    remote_hello(&mut client, &device, &fingerprint);
    let request_id = new_id();
    let command = session_create(&space.id, &request_id, "idempotent-key-1");

    client.send(&command);
    assert_eq!(
        completed_session(expect_result(&mut client, false)),
        request_id
    );

    client.send(&command);
    assert_eq!(
        completed_session(expect_result(&mut client, true)),
        request_id
    );
    assert_eq!(server.host().session_count(), 1);

    // Same key, different command: a typed conflict, never a cache hit.
    let other_space = server.host().create_test_space("other").unwrap();
    client.send(&session_create(&other_space, &new_id(), "idempotent-key-1"));
    let error = expect_error(&mut client);
    assert!(
        error.message.contains("different command"),
        "{}",
        error.message
    );
    assert_eq!(server.host().session_count(), 1);

    drop(client);
    drop(server);
    fs::remove_dir_all(directory).ok();
}

#[test]
fn handshake_is_required_and_remote_commands_need_their_scope() {
    let (directory, path) = temporary_database();
    let mut store = Store::open(&path).unwrap();
    let space = store.create_space("ukk", "/src/ukk").unwrap();
    let server = TestServer::start(ProtocolHost::open(store).unwrap(), EndpointPlane::Local);

    // No hello: the command is refused before anything is authorized.
    let mut early = server.client();
    early.send(&session_create(&space.id, &new_id(), "early-key"));
    let error = expect_error(&mut early);
    assert!(error.message.contains("handshake required"));
    early.expect_closed();

    // A device claim on the local socket is a plane mismatch.
    let mut local_claim = server.client();
    local_claim.send(&ClientMessage::Hello(ClientHello::remote(
        RequestId::new_random(),
        vec![CURRENT_GENERATION],
        DeviceClaim {
            device_id: DeviceId::new("local-device").unwrap(),
            key_fingerprint: KeyFingerprint::new("sha256:local").unwrap(),
            generation: IdentityGeneration::FIRST,
        },
    )));
    let error = expect_error(&mut local_claim);
    assert_eq!(error.denial, Some(Denial::PlaneMismatch));
    local_claim.expect_closed();

    // The local plane is read-only in this slice: there is no local capability
    // field on the merged wire, so a same-user socket must not execute what the
    // JSON-RPC path gates behind the rotated LocalAuth token.
    let mut local_command = server.client();
    local_hello(&mut local_command);
    local_command.send(&session_create(&space.id, &new_id(), "local-key"));
    let error = expect_error(&mut local_command);
    assert!(
        error
            .message
            .contains("local protocol commands are disabled"),
        "{}",
        error.message
    );
    assert!(error.denial.is_none());
    assert_eq!(server.host().session_count(), 0);
    assert_eq!(server.host().idempotency_count(), 0);

    drop(server);

    // Remote trust plane: observe-only is enough to subscribe but not to
    // create a Session.
    let server = TestServer::start(
        ProtocolHost::open(Store::open(&path).unwrap()).unwrap(),
        EndpointPlane::RemoteForTests,
    );
    let device = "remote-device-1";
    server
        .host()
        .register_test_device(
            device,
            "sha256:remote-device-1",
            &[Scope::SessionObserve],
            now_ms() + 60_000,
        )
        .unwrap();
    let mut client = server.client();
    let hello = remote_hello(&mut client, device, "sha256:remote-device-1");
    assert!(hello.scopes.contains(Scope::SessionObserve));
    assert!(!hello.scopes.contains(Scope::SessionManage));

    subscribe(&mut client, 0, 16);
    client.send(&session_create(&space.id, &new_id(), "unauthorized-key"));
    let error = expect_error(&mut client);
    assert_eq!(
        error.denial,
        Some(Denial::MissingScope {
            scope: Scope::SessionManage
        })
    );
    assert_eq!(server.host().session_count(), 0);

    drop(client);
    drop(server);
    fs::remove_dir_all(directory).ok();
}

#[test]
fn revocation_and_grant_expiry_terminate_an_open_stream() {
    let (directory, path) = temporary_database();
    let mut store = Store::open(&path).unwrap();
    let space = store.create_space("ukk", "/src/ukk").unwrap();
    seed_session_event(&mut store, &space.id);
    let server = TestServer::start(
        ProtocolHost::open(store).unwrap(),
        EndpointPlane::RemoteForTests,
    );

    let revoked = "revoked-device";
    server
        .host()
        .register_test_device(
            revoked,
            "sha256:revoked-device",
            &[Scope::SessionObserve],
            now_ms() + 60_000,
        )
        .unwrap();
    let mut client = server.client();
    remote_hello(&mut client, revoked, "sha256:revoked-device");
    subscribe(&mut client, 0, 16);
    // Consume the replayed event so the subscription is provably live before
    // the device is revoked.
    assert_eq!(expect_event(&mut client).cursor.get(), 1);
    server.host().revoke_test_device(revoked).unwrap();
    let error = expect_error(&mut client);
    assert_eq!(error.denial, Some(Denial::DeviceRevoked));
    client.expect_closed();
    drop(client);

    let expiring = "expiring-device";
    server
        .host()
        .register_test_device(
            expiring,
            "sha256:expiring-device",
            &[Scope::SessionObserve],
            now_ms() + 800,
        )
        .unwrap();
    let mut client = server.client();
    remote_hello(&mut client, expiring, "sha256:expiring-device");
    subscribe(&mut client, 0, 16);
    assert_eq!(expect_event(&mut client).cursor.get(), 1);
    thread::sleep(Duration::from_millis(1100));
    let error = expect_error(&mut client);
    assert_eq!(
        error.denial,
        Some(Denial::GrantExpired {
            scope: Scope::SessionObserve
        })
    );
    client.expect_closed();
    drop(client);

    // A revocation committed outside this adapter (another store writer) must
    // still stop delivery: each batch re-reads the durable registry.
    let external = "externally-revoked";
    server
        .host()
        .register_test_device(
            external,
            "sha256:externally-revoked",
            &[Scope::SessionObserve],
            now_ms() + 60_000,
        )
        .unwrap();
    let mut client = server.client();
    remote_hello(&mut client, external, "sha256:externally-revoked");
    subscribe(&mut client, 0, 16);
    assert_eq!(expect_event(&mut client).cursor.get(), 1);
    server.host().revoke_device_in_store_only(external).unwrap();
    let error = expect_error(&mut client);
    assert_eq!(error.denial, Some(Denial::DeviceRevoked));
    client.expect_closed();

    drop(client);
    drop(server);
    fs::remove_dir_all(directory).ok();
}

#[test]
fn oversized_and_malformed_frames_are_rejected() {
    let (directory, path) = temporary_database();
    let mut store = Store::open(&path).unwrap();
    let space = store.create_space("ukk", "/src/ukk").unwrap();
    seed_session_event(&mut store, &space.id);
    let server = TestServer::start(ProtocolHost::open(store).unwrap(), EndpointPlane::Local);

    // A declared length above the envelope bound is refused before the body is
    // read or allocated.
    let mut oversized = server.client();
    oversized.send_length(1_000_000);
    let error = expect_error(&mut oversized);
    assert!(error.message.contains("limit is"), "{}", error.message);
    oversized.expect_closed();

    // Malformed JSON before the handshake fails closed.
    let mut early = server.client();
    early.send_raw(b"{");
    let error = expect_error(&mut early);
    assert!(error.message.contains("malformed"), "{}", error.message);
    early.expect_closed();

    // After the handshake, malformed frames are typed errors and the
    // connection remains usable for corrected requests.
    let mut client = server.client();
    local_hello(&mut client);
    client.send_raw(b"{");
    let error = expect_error(&mut client);
    assert!(error.message.contains("malformed"), "{}", error.message);
    client.send_raw(&[0xff, 0xfe]);
    let error = expect_error(&mut client);
    assert!(error.message.contains("malformed"), "{}", error.message);
    client.send_raw(b"");
    let error = expect_error(&mut client);
    assert!(error.message.contains("malformed"), "{}", error.message);

    // The connection still serves a valid read request after recoverable
    // malformed frames.
    subscribe(&mut client, 0, 16);
    assert_eq!(expect_event(&mut client).cursor.get(), 1);

    drop(client);
    drop(server);
    fs::remove_dir_all(directory).ok();
}

#[test]
fn reconnect_resumes_from_the_persisted_cursor() {
    let (directory, path) = temporary_database();
    let mut store = Store::open(&path).unwrap();
    let space = store.create_space("ukk", "/src/ukk").unwrap();
    let host = ProtocolHost::open(store).unwrap();
    let (server, device, fingerprint) =
        start_remote_server(host, &[Scope::SessionObserve, Scope::SessionManage]);

    let mut writer = server.client();
    remote_hello(&mut writer, &device, &fingerprint);
    for _ in 0..3 {
        writer.send(&session_create(&space.id, &new_id(), &new_id()));
        completed_session(expect_result(&mut writer, false));
    }

    {
        let mut first = server.client();
        remote_hello(&mut first, &device, &fingerprint);
        subscribe(&mut first, 0, 16);
        for expected in 1..=3 {
            assert_eq!(expect_event(&mut first).cursor.get(), expected);
        }
        assert_eq!(first.buffer.len(), 0, "all replay frames were consumed");
    }

    // A durable event arrives while the client is disconnected.
    writer.send(&session_create(&space.id, &new_id(), &new_id()));
    completed_session(expect_result(&mut writer, false));

    let mut resumed = server.client();
    remote_hello(&mut resumed, &device, &fingerprint);
    subscribe(&mut resumed, 3, 16);
    assert_eq!(expect_event(&mut resumed).cursor.get(), 4);

    // Resume at the current head: no replay, then the next live event.
    subscribe(&mut resumed, 4, 16);
    writer.send(&session_create(&space.id, &new_id(), &new_id()));
    completed_session(expect_result(&mut writer, false));
    assert_eq!(expect_event(&mut resumed).cursor.get(), 5);

    drop(resumed);
    drop(writer);
    drop(server);
    fs::remove_dir_all(directory).ok();
}

#[test]
fn events_and_idempotency_survive_a_host_restart() {
    let (directory, database) = temporary_database();
    let mut store = Store::open(&database).unwrap();
    let space = store.create_space("ukk", "/src/ukk").unwrap();
    let space_id = space.id;
    let host = ProtocolHost::open(store).unwrap();
    let (mut server, device, fingerprint) =
        start_remote_server(host, &[Scope::SessionObserve, Scope::SessionManage]);

    let mut client = server.client();
    remote_hello(&mut client, &device, &fingerprint);
    let first_request = new_id();
    client.send(&session_create(&space_id, &first_request, "restart-key-1"));
    completed_session(expect_result(&mut client, false));
    client.send(&session_create(&space_id, &new_id(), "restart-key-2"));
    completed_session(expect_result(&mut client, false));

    subscribe(&mut client, 0, 16);
    let before: Vec<EventEnvelope> = (0..2).map(|_| expect_event(&mut client)).collect();
    assert_eq!(
        before
            .iter()
            .map(|event| event.cursor.get())
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    let event_ids: Vec<_> = before.iter().map(|event| event.event_id.clone()).collect();
    drop(client);

    // Stop the whole listener, including its worker threads, before reopening
    // the database file.
    server.stop();
    drop(server);

    let store = Store::open(&database).unwrap();
    let host = ProtocolHost::open(store).unwrap();
    // The device registration itself is durable: no test re-registration is
    // needed after the restart.
    let server = TestServer::start(host, EndpointPlane::RemoteForTests);
    let mut client = server.client();
    let hello = remote_hello(&mut client, &device, &fingerprint);
    assert_eq!(hello.streams[0].head.get(), 2);

    subscribe(&mut client, 0, 16);
    let after: Vec<EventEnvelope> = (0..2).map(|_| expect_event(&mut client)).collect();
    assert_eq!(
        after
            .iter()
            .map(|event| event.event_id.clone())
            .collect::<Vec<_>>(),
        event_ids,
        "no durable event may be lost or renumbered across a Host restart"
    );
    assert_eq!(
        after
            .iter()
            .map(|event| event.cursor.get())
            .collect::<Vec<_>>(),
        vec![1, 2]
    );

    // The idempotency record is durable too: the retry is a replay, and no
    // second Session is created.
    client.send(&session_create(&space_id, &first_request, "restart-key-1"));
    completed_session(expect_result(&mut client, true));
    assert_eq!(server.host().session_count(), 2);

    drop(client);
    drop(server);
    fs::remove_dir_all(directory).ok();
}
