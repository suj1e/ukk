//! Local-only streaming endpoint for the versioned Host protocol.
//!
//! The endpoint is a same-user Unix Domain Socket at
//! `~/.ukk/run/agent-host.protocol` (default). It is **not** a WAN listener:
//!
//! * no TCP socket is created, ever;
//! * the socket lives in the 0700 run directory as mode 0600 and every
//!   accepted connection is verified with `SO_PEERCRED`/`getpeereid`;
//! * a device claim on this local binding is rejected by
//!   `ukk_protocol::ProtocolService::accept_hello`, so a local peer cannot
//!   talk its way into the remote trust plane;
//! * the first frame must be `hello`; commands before the handshake are
//!   refused.
//!
//! Framing is length-prefixed (4-byte big-endian length, UTF-8 JSON body), not
//! newline-delimited JSON. The declared length is checked against the protocol
//! envelope bound before any body byte is allocated, and every outbound frame
//! is encoded through `ServerMessage::encode`, which enforces the envelope and
//! snapshot limits. A bounded per-connection outbound queue closes a client
//! that stops draining the stream.
//!
//! Windows Named Pipe streaming for this endpoint is **NOT VERIFIED** and not
//! implemented in this slice; the existing JSON-RPC Named Pipe in
//! `transport.rs` is unaffected. See `docs/subsystems/protocol.md`.

use crate::broker::Broker;
use crate::protocol_service::{
    now_ms, CommandOutcome, Delivery, ProtocolHost, ProtocolHostError, WORKSPACE_STREAM,
};
use crate::{local_fs, peer, AppResult};
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use std::{env, io};
use ukk_protocol::{
    AuthenticatedSession, ClientMessage, Denial, DurableCursor, Limits, ProtocolGeneration,
    RequestId, ServerError, ServerMessage, CURRENT_GENERATION, MAX_ERROR_MESSAGE_BYTES,
};

/// A client that stops sending frames before the handshake is dropped after
/// this interval. An authenticated subscriber is not idle: it is receiving a
/// live stream.
const CLIENT_IDLE_TIMEOUT: Duration = Duration::from_secs(60);
/// Writes to a non-reading client are bounded.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
/// How often an open subscription re-checks authorization and pulls the next
/// durable batch. This also bounds revocation latency on an idle stream.
const LIVE_POLL: Duration = Duration::from_millis(40);
/// Concurrent protocol connections. Each one can hold a streaming
/// subscription, so this is deliberately smaller than the JSON-RPC quota.
const MAX_PROTOCOL_CONNECTIONS: usize = 16;
/// Malformed frames tolerated on one connection before it is closed.
const MAX_MALFORMED_FRAMES: usize = 8;

/// Connection trust plane chosen by the listener.
#[derive(Debug, Clone, Copy)]
pub(crate) enum EndpointPlane {
    /// Production: OS-authenticated local owner on a same-user Unix socket.
    Local,
    /// Test-only: exercises the remote trust plane (device grants and
    /// mid-stream revocation) over the same socket. Production never
    /// constructs this value, and a WAN/TCP listener does not exist.
    #[cfg(test)]
    RemoteForTests,
}

/// Default endpoint path.
///
/// `UKK_PROTOCOL_SOCKET` wins; otherwise it is a sibling of the JSON-RPC
/// endpoint (`agent-host.protocol`), so an overridden `UKK_SOCKET` keeps both
/// endpoints inside the same private directory.
pub(crate) fn socket_path() -> AppResult<PathBuf> {
    if let Some(override_path) = env::var_os("UKK_PROTOCOL_SOCKET") {
        return Ok(PathBuf::from(override_path));
    }
    let main = crate::transport::socket_path()?;
    let parent = main.parent().ok_or("invalid Host socket path")?;
    Ok(parent.join("agent-host.protocol"))
}

/// Binds and serves the protocol endpoint in a background thread.
///
/// Called by `transport::serve` while the Host lifecycle lock is held, so the
/// stale-socket probe cannot race a second Host. A failure here disables only
/// the protocol stream; the existing local JSON-RPC endpoint keeps running.
pub(crate) fn serve_in_background(broker: Arc<Broker>) -> AppResult<()> {
    use std::fs;

    let path = socket_path()?;
    let parent = path.parent().ok_or("invalid protocol socket path")?;
    let default_location =
        env::var_os("UKK_PROTOCOL_SOCKET").is_none() && env::var_os("UKK_SOCKET").is_none();
    local_fs::secure_socket_directory(parent, default_location)
        .map_err(|error| format!("protocol endpoint setup failed: {error}"))?;
    if local_fs::remove_stale_socket(&path)
        .map_err(|error| format!("protocol endpoint recovery failed: {error}"))?
    {
        println!(
            "ukk protocol endpoint: recovered a stale socket at {} after confirming \
             no live Host",
            path.display()
        );
    }
    let listener = UnixListener::bind(&path)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    local_fs::verify_socket_permissions(&path)
        .map_err(|error| format!("protocol socket is not private: {error}"))?;
    let host = Arc::new(ProtocolHost::open_default().map_err(
        |error| -> Box<dyn std::error::Error + Send + Sync> {
            format!("protocol state unavailable: {error}").into()
        },
    )?);
    println!(
        "ukk protocol endpoint listening on {} (length-prefixed, same-user, local-only)",
        path.display()
    );
    crate::protocol_bridge::start(Arc::clone(&host), broker);
    thread::spawn(move || accept_loop(listener, host, EndpointPlane::Local, None));
    Ok(())
}

fn accept_loop(
    listener: UnixListener,
    host: Arc<ProtocolHost>,
    plane: EndpointPlane,
    stop: Option<Arc<AtomicBool>>,
) {
    let tracking = stop.is_some();
    if tracking {
        let _ = listener.set_nonblocking(true);
    }
    let clients = Arc::new(AtomicUsize::new(0));
    let mut workers: Vec<thread::JoinHandle<()>> = Vec::new();
    loop {
        if stop
            .as_ref()
            .is_some_and(|stop| stop.load(Ordering::Acquire))
        {
            break;
        }
        match listener.accept() {
            Ok((stream, _)) => {
                let Some(slot) = ConnectionSlot::acquire(&clients) else {
                    reject_overloaded(stream);
                    continue;
                };
                let host = Arc::clone(&host);
                let stop = stop.clone();
                let worker = thread::spawn(move || {
                    let _slot = slot;
                    handle_connection(stream, host, plane, stop);
                });
                if tracking {
                    workers.push(worker);
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) =>
            {
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => {
                eprintln!("ukk protocol endpoint accept error: {error}");
                thread::sleep(Duration::from_millis(20));
            }
        }
    }
    for worker in workers {
        let _ = worker.join();
    }
}

struct ConnectionSlot(Arc<AtomicUsize>);

impl ConnectionSlot {
    fn acquire(counter: &Arc<AtomicUsize>) -> Option<Self> {
        let mut current = counter.load(Ordering::Acquire);
        loop {
            if current >= MAX_PROTOCOL_CONNECTIONS {
                return None;
            }
            match counter.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Some(Self(Arc::clone(counter))),
                Err(next) => current = next,
            }
        }
    }
}

impl Drop for ConnectionSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

fn reject_overloaded(mut stream: UnixStream) {
    let _ = stream.set_write_timeout(Some(Duration::from_secs(1)));
    let error = ServerMessage::Error(ServerError {
        protocol: CURRENT_GENERATION,
        request_id: None,
        denial: None,
        message: "too many concurrent protocol clients".into(),
    });
    let _ = write_message(&mut stream, &error, &Limits::default());
}

/// Pending outbound frames are bounded. A client that stops reading does not
/// get an unbounded Host-side buffer; the connection is closed instead.
struct Outbound {
    frames: VecDeque<Vec<u8>>,
    cap: usize,
}

impl Outbound {
    fn new(limits: &Limits) -> Self {
        Self {
            frames: VecDeque::new(),
            cap: limits.max_queue_depth,
        }
    }

    fn push(&mut self, frame: Vec<u8>) -> bool {
        if self.frames.len() >= self.cap {
            return false;
        }
        self.frames.push_back(frame);
        true
    }

    fn push_message(&mut self, message: &ServerMessage, limits: &Limits) -> bool {
        match message.encode(limits) {
            Ok(frame) => self.push(frame.into_bytes()),
            Err(error) => {
                eprintln!("ukk protocol endpoint could not encode a frame: {error}");
                false
            }
        }
    }

    fn flush(&mut self, stream: &mut UnixStream) -> bool {
        while let Some(frame) = self.frames.pop_front() {
            if write_frame(stream, &frame).is_err() {
                return false;
            }
        }
        stream.flush().is_ok()
    }
}

fn write_frame(stream: &mut UnixStream, frame: &[u8]) -> io::Result<()> {
    let length = u32::try_from(frame.len())
        .map_err(|_| io::Error::other("protocol frame exceeds the length prefix"))?;
    stream.write_all(&length.to_be_bytes())?;
    stream.write_all(frame)
}

fn write_message(stream: &mut UnixStream, message: &ServerMessage, limits: &Limits) -> bool {
    match message.encode(limits) {
        Ok(frame) => write_frame(stream, frame.as_bytes()).is_ok(),
        Err(error) => {
            eprintln!("ukk protocol endpoint could not encode a frame: {error}");
            false
        }
    }
}

/// Server-side frame decoder. The declared length is checked before the body is
/// read or allocated, so an oversized claim cannot consume the budget.
#[derive(Debug)]
enum FrameRead {
    Frame(Vec<u8>),
    /// No complete frame yet (read deadline elapsed).
    Empty,
}

#[derive(Debug)]
enum FrameError {
    TooLarge { claimed: u64, max: usize },
    Closed,
    Io(io::Error),
}

struct FrameDecoder {
    buffer: Vec<u8>,
}

impl FrameDecoder {
    fn new() -> Self {
        Self {
            buffer: Vec::with_capacity(1024),
        }
    }

    fn poll(&mut self, stream: &mut UnixStream, max_frame: usize) -> Result<FrameRead, FrameError> {
        loop {
            if let Some(frame) = self.take_frame(max_frame)? {
                return Ok(FrameRead::Frame(frame));
            }
            let want = match self.declared_length() {
                Some(length) => (4 + length as usize).saturating_sub(self.buffer.len()),
                None => 4 - self.buffer.len(),
            };
            let mut chunk = [0u8; 8192];
            let read = want.min(chunk.len());
            match stream.read(&mut chunk[..read]) {
                Ok(0) => return Err(FrameError::Closed),
                Ok(count) => self.buffer.extend_from_slice(&chunk[..count]),
                Err(error) if is_timeout(&error) => return Ok(FrameRead::Empty),
                Err(error) => return Err(FrameError::Io(error)),
            }
        }
    }

    fn declared_length(&self) -> Option<u32> {
        if self.buffer.len() < 4 {
            return None;
        }
        let mut prefix = [0u8; 4];
        prefix.copy_from_slice(&self.buffer[..4]);
        Some(u32::from_be_bytes(prefix))
    }

    /// True while a declared frame is only partly received. A client that
    /// sends a prefix and then stalls is disconnected by the idle deadline.
    fn has_partial_frame(&self) -> bool {
        !self.buffer.is_empty()
    }

    fn take_frame(&mut self, max_frame: usize) -> Result<Option<Vec<u8>>, FrameError> {
        let Some(length) = self.declared_length() else {
            return Ok(None);
        };
        if length as usize > max_frame {
            return Err(FrameError::TooLarge {
                claimed: u64::from(length),
                max: max_frame,
            });
        }
        let total = 4 + length as usize;
        if self.buffer.len() < total {
            return Ok(None);
        }
        let body: Vec<u8> = self.buffer[4..total].to_vec();
        self.buffer.drain(..total);
        Ok(Some(body))
    }
}

struct Subscription {
    cursor: DurableCursor,
    batch: usize,
}

fn handle_connection(
    mut stream: UnixStream,
    host: Arc<ProtocolHost>,
    plane: EndpointPlane,
    stop: Option<Arc<AtomicBool>>,
) {
    let _ = stream.set_read_timeout(Some(LIVE_POLL));
    let _ = stream.set_write_timeout(Some(WRITE_TIMEOUT));
    // The OS identity is verified before any protocol byte is read. It also
    // becomes the `TransportBinding` principal, so a local peer can never
    // claim a device.
    let uid = match peer::verified_same_user_uid(&stream) {
        Ok(uid) => uid,
        Err(error) => {
            eprintln!("ukk protocol endpoint rejected local client: {error}");
            return;
        }
    };
    let binding = match plane {
        EndpointPlane::Local => ukk_protocol::TransportBinding::local_ipc(uid),
        #[cfg(test)]
        EndpointPlane::RemoteForTests => ukk_protocol::TransportBinding::remote(),
    };
    let limits = host.limits();
    let mut session: Option<AuthenticatedSession> = None;
    let mut decoder = FrameDecoder::new();
    let mut outbound = Outbound::new(&limits);
    let mut subscription: Option<Subscription> = None;
    let mut acked = DurableCursor::ZERO;
    let mut malformed = 0usize;
    let mut last_client_frame = Instant::now();

    loop {
        if stop
            .as_ref()
            .is_some_and(|stop| stop.load(Ordering::Acquire))
        {
            return;
        }

        // Live delivery: re-authorize before every batch and pull the next
        // durable events (or a snapshot when history was pruned).
        if let (Some(active), Some(auth)) = (subscription.as_mut(), session.as_ref()) {
            match host.deliver(auth, active.cursor, active.batch, now_ms()) {
                Ok(Delivery::Idle) => {}
                Ok(Delivery::Events { events, next }) => {
                    for event in events {
                        if !outbound.push_message(&ServerMessage::Event(event), &limits) {
                            outbound.flush(&mut stream);
                            return;
                        }
                    }
                    active.cursor = next;
                }
                Ok(Delivery::Snapshot { snapshot, next }) => {
                    if !outbound.push_message(&ServerMessage::Snapshot(snapshot), &limits) {
                        outbound.flush(&mut stream);
                        return;
                    }
                    active.cursor = next;
                }
                Err(error) => {
                    // Delivery authorization failed (revocation, rotation or
                    // grant expiry) or durable state is unreadable. The open
                    // subscription is terminated fail-closed.
                    push_error(&mut outbound, &limits, auth.generation, None, &error);
                    outbound.flush(&mut stream);
                    return;
                }
            }
        }
        if !outbound.flush(&mut stream) {
            return;
        }

        let frame = match decoder.poll(&mut stream, limits.max_envelope_bytes) {
            Ok(FrameRead::Frame(frame)) => frame,
            Ok(FrameRead::Empty) => {
                let idle = last_client_frame.elapsed() > CLIENT_IDLE_TIMEOUT;
                // An authenticated subscriber may legitimately send nothing
                // while it receives a live stream. Any other idle connection,
                // or one stuck mid-frame, is dropped.
                if idle && (decoder.has_partial_frame() || subscription.is_none()) {
                    return;
                }
                continue;
            }
            Err(FrameError::TooLarge { claimed, max }) => {
                let protocol = session
                    .as_ref()
                    .map_or(CURRENT_GENERATION, |auth| auth.generation);
                let message = format!("frame declares {claimed} bytes; limit is {max}");
                push_error_message(&mut outbound, &limits, protocol, None, message);
                outbound.flush(&mut stream);
                return;
            }
            Err(FrameError::Closed) => return,
            Err(FrameError::Io(error)) => {
                if !is_timeout(&error) {
                    eprintln!("ukk protocol endpoint read error: {error}");
                }
                return;
            }
        };
        last_client_frame = Instant::now();

        let text = match std::str::from_utf8(&frame) {
            Ok(text) => text,
            Err(_) => {
                if !handle_malformed(
                    &mut stream,
                    &mut outbound,
                    &limits,
                    session.as_ref(),
                    &mut malformed,
                ) {
                    return;
                }
                continue;
            }
        };
        let message = match ClientMessage::decode(text, &limits) {
            Ok(message) => message,
            Err(error) => {
                let protocol = session
                    .as_ref()
                    .map_or(CURRENT_GENERATION, |auth| auth.generation);
                push_error_message(
                    &mut outbound,
                    &limits,
                    protocol,
                    None,
                    format!("malformed frame: {error}"),
                );
                malformed += 1;
                if malformed > MAX_MALFORMED_FRAMES || session.is_none() {
                    outbound.flush(&mut stream);
                    return;
                }
                if !outbound.flush(&mut stream) {
                    return;
                }
                continue;
            }
        };

        let Some(auth) = session.as_ref() else {
            let ClientMessage::Hello(hello) = message else {
                push_error_message(
                    &mut outbound,
                    &limits,
                    CURRENT_GENERATION,
                    None,
                    "handshake required before any command".into(),
                );
                outbound.flush(&mut stream);
                return;
            };
            match host.accept_hello(&binding, &hello, now_ms()) {
                Ok((authenticated, reply)) => {
                    session = Some(authenticated);
                    if !outbound.push_message(&ServerMessage::Hello(reply), &limits) {
                        return;
                    }
                }
                Err(error) => {
                    push_error(&mut outbound, &limits, CURRENT_GENERATION, None, &error);
                    outbound.flush(&mut stream);
                    return;
                }
            }
            if !outbound.flush(&mut stream) {
                return;
            }
            continue;
        };

        if let Some(protocol) = message.protocol() {
            if protocol != auth.generation {
                push_error_message(
                    &mut outbound,
                    &limits,
                    auth.generation,
                    None,
                    format!(
                        "envelope is generation {}, connection is generation {}",
                        protocol.raw(),
                        auth.generation.raw()
                    ),
                );
                if !outbound.flush(&mut stream) {
                    return;
                }
                continue;
            }
        }

        match message {
            ClientMessage::Hello(_) => {
                push_error_message(
                    &mut outbound,
                    &limits,
                    auth.generation,
                    None,
                    "connection is already authenticated".into(),
                );
            }
            ClientMessage::Subscribe(request) => {
                // A failed re-subscribe must not silently keep the old stream.
                subscription = None;
                match host.subscribe(auth, &request, now_ms()) {
                    Ok(delivery) => {
                        let batch = request.max_batch.clamp(1, limits.max_replay_batch);
                        let next = match delivery {
                            Delivery::Idle => {
                                subscription = Some(Subscription {
                                    cursor: request.cursor,
                                    batch,
                                });
                                request.cursor
                            }
                            Delivery::Events { events, next } => {
                                if !push_events(&mut outbound, &limits, events) {
                                    return;
                                }
                                next
                            }
                            Delivery::Snapshot { snapshot, next } => {
                                if !outbound
                                    .push_message(&ServerMessage::Snapshot(snapshot), &limits)
                                {
                                    return;
                                }
                                next
                            }
                        };
                        if subscription.is_none() {
                            subscription = Some(Subscription {
                                cursor: next,
                                batch,
                            });
                        }
                    }
                    Err(error) => {
                        // A cursor error is answered and the connection stays
                        // usable. A live-authorization denial closes instead:
                        // a revoked device must not keep a half-open channel.
                        let fatal = error.denial().is_some();
                        push_error(&mut outbound, &limits, auth.generation, None, &error);
                        if fatal {
                            outbound.flush(&mut stream);
                            return;
                        }
                    }
                }
            }
            ClientMessage::Ack(request) => {
                if request.stream.as_str() != WORKSPACE_STREAM {
                    push_error_message(
                        &mut outbound,
                        &limits,
                        auth.generation,
                        None,
                        "only the workspace stream is served".into(),
                    );
                } else if request.cursor < acked {
                    push_error_message(
                        &mut outbound,
                        &limits,
                        auth.generation,
                        None,
                        format!(
                            "acknowledgement would move the checkpoint backwards from {} to {}",
                            acked.get(),
                            request.cursor.get()
                        ),
                    );
                } else {
                    acked = request.cursor;
                }
            }
            ClientMessage::Command(envelope) => {
                let request_id = envelope.request_id.clone();
                match host.command(auth, &envelope, now_ms()) {
                    Ok(CommandOutcome::Completed(result)) => {
                        if !outbound.push_message(&ServerMessage::Result(result), &limits) {
                            return;
                        }
                    }
                    Ok(CommandOutcome::InFlight { .. }) => {
                        push_error_message(
                            &mut outbound,
                            &limits,
                            auth.generation,
                            Some(request_id),
                            "command is still in flight; do not retry blindly".into(),
                        );
                    }
                    Err(error) => {
                        push_error(
                            &mut outbound,
                            &limits,
                            auth.generation,
                            Some(request_id),
                            &error,
                        );
                    }
                }
            }
        }
        if !outbound.flush(&mut stream) {
            return;
        }
    }
}

fn push_events(
    outbound: &mut Outbound,
    limits: &Limits,
    events: Vec<ukk_protocol::EventEnvelope>,
) -> bool {
    for event in events {
        if !outbound.push_message(&ServerMessage::Event(event), limits) {
            return false;
        }
    }
    true
}

fn is_timeout(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

fn handle_malformed(
    stream: &mut UnixStream,
    outbound: &mut Outbound,
    limits: &Limits,
    session: Option<&AuthenticatedSession>,
    malformed: &mut usize,
) -> bool {
    let protocol = session.map_or(CURRENT_GENERATION, |auth| auth.generation);
    push_error_message(
        outbound,
        limits,
        protocol,
        None,
        "malformed frame: body is not UTF-8 protocol JSON".into(),
    );
    *malformed += 1;
    if *malformed > MAX_MALFORMED_FRAMES || session.is_none() {
        outbound.flush(stream);
        return false;
    }
    outbound.flush(stream)
}

fn push_error(
    outbound: &mut Outbound,
    limits: &Limits,
    protocol: ProtocolGeneration,
    request_id: Option<RequestId>,
    error: &ProtocolHostError,
) {
    push_error_frame(
        outbound,
        limits,
        protocol,
        request_id,
        error.to_string(),
        error.denial().cloned(),
    );
}

fn push_error_message(
    outbound: &mut Outbound,
    limits: &Limits,
    protocol: ProtocolGeneration,
    request_id: Option<RequestId>,
    message: String,
) {
    push_error_frame(outbound, limits, protocol, request_id, message, None);
}

fn push_error_frame(
    outbound: &mut Outbound,
    limits: &Limits,
    protocol: ProtocolGeneration,
    request_id: Option<RequestId>,
    message: String,
    denial: Option<Denial>,
) {
    let error = ServerMessage::Error(ServerError {
        protocol,
        request_id,
        denial,
        message: bounded_message(message),
    });
    let _ = outbound.push_message(&error, limits);
}

fn bounded_message(message: String) -> String {
    if message.len() <= MAX_ERROR_MESSAGE_BYTES {
        return message;
    }
    let mut end = MAX_ERROR_MESSAGE_BYTES;
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    message[..end].to_owned()
}

#[cfg(all(test, unix))]
mod tests;
