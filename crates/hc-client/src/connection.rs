//! One framed connection to `hc connect`.
//!
//! `hello {framed:true}` is sent in plain mode, then every message travels as
//! a frame. A background reader dispatches channel 0 responses to pending
//! requests and routes stream channels to bounded queues. Requests are
//! matched by JSON-RPC id, so responses may arrive out of order.

use crate::error::{HcError, RemoteError, Result};
use crate::events::StreamNotice;
use crate::frame::{Frame, FrameReader, FrameWriter};
use crate::stream::{
    Attachment, ChannelLifecycle, EventQueue, EventSubscription, StreamQueue,
    MAX_STREAM_BUFFER_BYTES,
};
use crate::types::{
    AttachRequest, AttachResult, EventFilter, Hello, ListResult, ReadRequest, ReadResult,
    RunRequest, RunResult, SendRequest, SendResult, ServerStatus, StopResult, SubscribeResult,
    Terminal, TerminalAddress, PROTOCOL_GENERATION,
};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::ffi::{OsStr, OsString};
use std::io::{BufRead, Read, Write};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
pub const DEFAULT_CALL_TIMEOUT: Duration = Duration::from_secs(30);
pub const DEFAULT_HELLO_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_ORPHAN_FRAMES: usize = 64;
const MAX_ORPHAN_CHANNELS: usize = 64;
const MAX_REQUEST_PAYLOAD: usize = 1024 * 1024;
const MAX_ORPHAN_BYTES: usize = 256 * 1024;
const MAX_SEND_CACHE: usize = 256;
const HELLO_ID: u64 = 0;
const MAX_HELLO_LINE: usize = 1024 * 1024;

/// The product-level launch outcome. `replayed` means no new process was
/// started: an existing terminal with the same name and command was reused.
#[derive(Debug, Clone, PartialEq)]
pub struct LaunchOutcome {
    pub result: RunResult,
    pub replayed: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SendOutcome {
    pub result: SendResult,
    pub replayed: bool,
}

#[derive(Clone)]
struct SendFingerprint(String);

enum SendEntry {
    InFlight(SendFingerprint),
    Done(SendFingerprint, SendResult),
}

#[derive(Default)]
struct SendCache {
    entries: HashMap<String, SendEntry>,
    order: VecDeque<String>,
}

impl SendCache {
    fn evict_done(&mut self) {
        let mut scanned = 0;
        while self.order.len() > MAX_SEND_CACHE && scanned < self.order.len() {
            let Some(key) = self.order.pop_front() else {
                break;
            };
            let done = matches!(self.entries.get(&key), Some(SendEntry::Done(..)));
            if done {
                self.entries.remove(&key);
            } else {
                self.order.push_back(key);
            }
            scanned += 1;
        }
    }
}

enum ChannelSink {
    Stream(Arc<StreamQueue>),
    Events(Arc<EventQueue>),
}

#[derive(Default)]
struct Routing {
    channels: HashMap<u32, ChannelSink>,
    orphans: HashMap<u32, (VecDeque<Frame>, usize)>,
}

struct Shared {
    hello: Hello,
    writer: Arc<FrameWriter>,
    next_id: AtomicU64,
    pending: Mutex<HashMap<u64, SyncSender<Result<Value>>>>,
    routing: Mutex<Routing>,
    closed: Mutex<Option<String>>,
    send_cache: Mutex<SendCache>,
    send_cache_ready: Condvar,
}

impl Shared {
    fn fail(&self, reason: String) {
        let mut closed = self.closed.lock().unwrap_or_else(|e| e.into_inner());
        if closed.is_some() {
            return;
        }
        *closed = Some(reason.clone());
        drop(closed);
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        for (_, sender) in pending.drain() {
            let _ = sender.send(Err(HcError::Closed(reason.clone())));
        }
        let sinks = {
            let mut routing = self.routing.lock().unwrap_or_else(|e| e.into_inner());
            std::mem::take(&mut routing.channels)
        };
        for (_, sink) in sinks {
            match sink {
                ChannelSink::Stream(queue) => queue.disconnect(reason.clone()),
                ChannelSink::Events(queue) => queue.disconnect(reason.clone()),
            }
        }
    }

    fn is_closed(&self) -> Option<String> {
        self.closed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn route(&self, frame: Frame) {
        let channel = frame.channel();
        let mut routing = self.routing.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(sink) = routing.channels.get(&channel) {
            Self::deliver(sink, frame);
            return;
        }
        let cost = frame.payload_len();
        if !routing.orphans.contains_key(&channel) && routing.orphans.len() >= MAX_ORPHAN_CHANNELS {
            return;
        }
        let entry = routing
            .orphans
            .entry(channel)
            .or_insert_with(|| (VecDeque::new(), 0));
        if entry.0.len() >= MAX_ORPHAN_FRAMES || entry.1.saturating_add(cost) > MAX_ORPHAN_BYTES {
            return;
        }
        entry.1 += cost;
        entry.0.push_back(frame);
    }

    fn deliver(sink: &ChannelSink, frame: Frame) {
        match sink {
            ChannelSink::Stream(queue) => queue.push(frame),
            ChannelSink::Events(queue) => match frame {
                Frame::Data { payload, .. } | Frame::AutomaticInput { payload, .. } => {
                    queue.push_payload(payload)
                }
                Frame::Close { .. } => queue.disconnect("core closed the event stream"),
                _ => {}
            },
        }
    }

    fn register(&self, channel: u32, sink: ChannelSink) -> Result<()> {
        // One lock covers orphan removal, sink insertion and draining, so a
        // frame cannot slip through the gap between registration steps. This
        // race used to drop the first paint on Windows.
        let mut routing = self.routing.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(reason) = self
            .closed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        {
            return Err(HcError::Closed(reason));
        }
        let orphan = routing.orphans.remove(&channel);
        routing.channels.insert(channel, sink);
        if let Some((frames, _)) = orphan {
            if let Some(sink) = routing.channels.get(&channel) {
                for frame in frames {
                    Self::deliver(sink, frame);
                }
            }
        }
        Ok(())
    }

    fn dispatch_json(&self, payload: &[u8]) {
        let value: Value = match serde_json::from_slice(payload) {
            Ok(value) => value,
            Err(error) => {
                self.fail(format!("invalid JSON on channel 0: {error}"));
                return;
            }
        };
        if value.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            self.fail("channel 0 message is not JSON-RPC 2.0".into());
            return;
        }
        let Some(id) = value.get("id").and_then(Value::as_u64) else {
            // Notifications on channel 0 are not part of protocol generation 1;
            // event notifications arrive on their own channel.
            return;
        };
        let sender = self
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&id);
        let Some(sender) = sender else {
            // A late duplicate response after a timeout is ignored.
            return;
        };
        let result = match value.get("error") {
            Some(error) => Err(HcError::Remote(RemoteError::from_value(error))),
            None => match value.get("result") {
                Some(result) => Ok(result.clone()),
                None => Err(HcError::Protocol("response missing result".into())),
            },
        };
        let _ = sender.send(result);
    }
}

impl ChannelLifecycle for Shared {
    fn detach(&self, channel: u32) {
        let mut routing = self.routing.lock().unwrap_or_else(|e| e.into_inner());
        routing.channels.remove(&channel);
        // A reused channel id must not inherit a previous stream's queued
        // frames, including a CLOSE acknowledgement.
        routing.orphans.remove(&channel);
    }
}

pub struct FramedConnection {
    shared: Arc<Shared>,
    child: Option<Child>,
    reader: Option<JoinHandle<()>>,
    request_timeout: Duration,
}

impl FramedConnection {
    /// Spawn `hc connect`, negotiate framed mode, and start the reader.
    pub fn connect(binary: &OsStr) -> Result<Self> {
        Self::connect_with_timeouts(binary, DEFAULT_REQUEST_TIMEOUT, DEFAULT_HELLO_TIMEOUT)
    }

    pub fn connect_with_timeout(binary: &OsStr, request_timeout: Duration) -> Result<Self> {
        Self::connect_with_timeouts(
            binary,
            request_timeout,
            request_timeout.min(DEFAULT_HELLO_TIMEOUT),
        )
    }

    /// Bounded handshake: a core that accepts the connection but never answers
    /// `hello` cannot pin the caller. The child is killed and reaped on both
    /// timeout and parse failure.
    pub fn connect_with_timeouts(
        binary: &OsStr,
        request_timeout: Duration,
        hello_timeout: Duration,
    ) -> Result<Self> {
        let mut child = Command::new(binary)
            .arg("connect")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| HcError::Protocol("hc stdin unavailable".into()))?;
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| HcError::Protocol("hc stdout unavailable".into()))?;

        let hello_request = serde_json::to_vec(&json!({
            "jsonrpc":"2.0",
            "id": HELLO_ID,
            "method":"hello",
            "params":{"framed":true,"generation":PROTOCOL_GENERATION}
        }))?;
        if let Err(error) = (|| -> Result<()> {
            stdin.write_all(&hello_request)?;
            stdin.write_all(b"\n")?;
            stdin.flush()?;
            Ok(())
        })() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        let (sender, receiver) = mpsc::sync_channel(1);
        let handshake = thread::Builder::new()
            .name("hc-hello-reader".into())
            .spawn(move || {
                let result = read_limited_line(&mut stdout, MAX_HELLO_LINE)
                    .and_then(|line| crate::protocol::parse_reply(&line, HELLO_ID))
                    .and_then(|result| Hello::from_result(&result))
                    .and_then(|hello| {
                        if !hello.framed {
                            return Err(HcError::Unsupported {
                                method: "hello.framed".into(),
                                reason: "core did not switch this connection to framed mode".into(),
                            });
                        }
                        hello.require_method("hello")?;
                        Ok(hello)
                    });
                let _ = sender.send(result);
                stdout
            })?;
        let hello = match receiver.recv_timeout(hello_timeout) {
            Ok(Ok(hello)) => hello,
            Ok(Err(error)) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = handshake.join();
                return Err(error);
            }
            Err(RecvTimeoutError::Timeout) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = handshake.join();
                return Err(HcError::Timeout {
                    operation: "hello".into(),
                    seconds: hello_timeout.as_secs(),
                });
            }
            Err(RecvTimeoutError::Disconnected) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = handshake.join();
                return Err(HcError::Protocol("hello reader vanished".into()));
            }
        };
        let stdout = handshake
            .join()
            .map_err(|_| HcError::Protocol("hello reader panicked".into()))?;

        let writer = Arc::new(FrameWriter::new(stdin));
        let shared = Arc::new(Shared {
            hello,
            writer,
            next_id: AtomicU64::new(1),
            pending: Mutex::new(HashMap::new()),
            routing: Mutex::new(Routing::default()),
            closed: Mutex::new(None),
            send_cache: Mutex::new(SendCache::default()),
            send_cache_ready: Condvar::new(),
        });
        let reader_shared = Arc::clone(&shared);
        let reader = thread::Builder::new()
            .name("hc-framed-reader".into())
            .spawn(move || reader_loop(stdout, reader_shared))?;
        Ok(Self {
            shared,
            child: Some(child),
            reader: Some(reader),
            request_timeout,
        })
    }

    pub fn hello(&self) -> &Hello {
        &self.shared.hello
    }

    pub fn core_id(&self) -> &str {
        &self.shared.hello.core.id
    }

    pub fn is_closed(&self) -> bool {
        self.shared.is_closed().is_some()
    }

    pub fn set_request_timeout(&mut self, timeout: Duration) {
        self.request_timeout = timeout;
    }

    pub fn call(&self, method: &str, params: Value) -> Result<Value> {
        self.call_with_timeout(method, params, self.request_timeout)
    }

    /// Capability-checked request. A method the core did not advertise in
    /// `hello` is refused before anything is written.
    pub fn call_with_timeout(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value> {
        self.shared.hello.require_method(method)?;
        if let Some(reason) = self.shared.is_closed() {
            return Err(HcError::Closed(reason));
        }
        let id = self.shared.next_id.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = mpsc::sync_channel(1);
        self.shared
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, sender);
        let payload = serde_json::to_vec(&json!({
            "jsonrpc":"2.0","id":id,"method":method,"params":params
        }))?;
        if payload.len() > MAX_REQUEST_PAYLOAD {
            self.shared
                .pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&id);
            return Err(HcError::InvalidArgument(format!(
                "request payload {} exceeds {MAX_REQUEST_PAYLOAD} bytes",
                payload.len()
            )));
        }
        if let Some(reason) = self.shared.is_closed() {
            self.shared
                .pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&id);
            return Err(HcError::Closed(reason));
        }
        if let Err(error) = self.shared.writer.send(&Frame::Data {
            channel: 0,
            payload,
        }) {
            self.shared
                .pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&id);
            return Err(error);
        }
        match receiver.recv_timeout(timeout) {
            Ok(result) => result,
            Err(RecvTimeoutError::Timeout) => {
                self.shared
                    .pending
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&id);
                Err(HcError::Timeout {
                    operation: method.to_owned(),
                    seconds: timeout.as_secs(),
                })
            }
            Err(RecvTimeoutError::Disconnected) => {
                let reason = self
                    .shared
                    .is_closed()
                    .unwrap_or_else(|| "request channel dropped".into());
                Err(HcError::Closed(reason))
            }
        }
    }

    pub fn terminal_list(
        &self,
        all: bool,
        labels: &BTreeMap<String, String>,
    ) -> Result<ListResult> {
        let mut params = serde_json::Map::new();
        if all {
            params.insert("all".into(), Value::Bool(true));
        }
        if !labels.is_empty() {
            params.insert("labels".into(), serde_json::to_value(labels)?);
        }
        self.typed("terminal.list", Value::Object(params))
    }

    pub fn terminal_get(&self, terminal: &TerminalAddress) -> Result<Terminal> {
        self.typed("terminal.get", json!({"terminal":terminal}))
    }

    pub fn terminal_run(&self, request: &RunRequest) -> Result<RunResult> {
        let params = request.to_params()?;
        let timeout = timeout_for_wait(self.request_timeout, request.wait, request.timeout);
        let value = self.call_with_timeout("terminal.run", params, timeout)?;
        RunResult::from_value(value)
    }

    /// Idempotent launch keyed by the terminal name the core requires to be
    /// unique among live terminals. A same-named terminal is reused only when
    /// its command and working directory prove it is the same request; a
    /// different request is a typed conflict, never a second process.
    pub fn terminal_run_idempotent(&self, request: &RunRequest) -> Result<LaunchOutcome> {
        let name = request.name.clone().ok_or_else(|| {
            HcError::InvalidArgument("idempotent launch requires an explicit terminal name".into())
        })?;
        match self.terminal_get(&TerminalAddress::parse(name.as_str())?) {
            Ok(terminal) => {
                if terminal.is_live() && same_launch(&terminal, request) {
                    return Ok(LaunchOutcome {
                        result: RunResult {
                            terminal,
                            first_message: None,
                            output: None,
                        },
                        replayed: true,
                    });
                }
                if terminal.is_live() {
                    return Err(HcError::IdempotencyConflict(name));
                }
                // A finished record with this name does not block a new run.
            }
            Err(HcError::Remote(error)) if error.kind == crate::error::RemoteKind::NotFound => {}
            Err(error) => return Err(error),
        }
        match self.terminal_run(request) {
            Ok(result) => Ok(LaunchOutcome {
                result,
                replayed: false,
            }),
            Err(HcError::Remote(error)) if error.kind == crate::error::RemoteKind::NameTaken => {
                let terminal = self.terminal_get(&TerminalAddress::parse(name.as_str())?)?;
                if same_launch(&terminal, request) {
                    Ok(LaunchOutcome {
                        result: RunResult {
                            terminal,
                            first_message: None,
                            output: None,
                        },
                        replayed: true,
                    })
                } else {
                    Err(HcError::IdempotencyConflict(name))
                }
            }
            Err(error) => Err(error),
        }
    }

    pub fn terminal_send(&self, request: &SendRequest) -> Result<SendResult> {
        let params = request.to_params()?;
        let timeout = timeout_for_wait(self.request_timeout, request.wait, request.timeout);
        self.typed_with_timeout("terminal.send", params, timeout)
    }

    /// Client-side dedupe for one connection. herdr-core has no idempotency
    /// key; this cache is in-memory only and is never replayed after a
    /// reconnect, because re-sending user input is unsafe.
    pub fn terminal_send_once(
        &self,
        idempotency_key: &str,
        request: &SendRequest,
    ) -> Result<SendOutcome> {
        // The fingerprint covers every serialized field (force, if, wait,
        // timeout, caller), not just text, so a changed request cannot be
        // mistaken for a retry.
        let fingerprint = SendFingerprint(serde_json::to_string(request)?);
        let mut cache = self
            .shared
            .send_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        loop {
            match cache.entries.get(idempotency_key) {
                Some(SendEntry::InFlight(existing)) => {
                    if existing.0 != fingerprint.0 {
                        return Err(HcError::IdempotencyConflict(idempotency_key.to_owned()));
                    }
                    // Another caller owns the send. Wait for its outcome so a
                    // concurrent retry never issues a second send.
                    cache = self
                        .shared
                        .send_cache_ready
                        .wait(cache)
                        .unwrap_or_else(|e| e.into_inner());
                }
                Some(SendEntry::Done(existing, result)) => {
                    if existing.0 != fingerprint.0 {
                        return Err(HcError::IdempotencyConflict(idempotency_key.to_owned()));
                    }
                    return Ok(SendOutcome {
                        result: result.clone(),
                        replayed: true,
                    });
                }
                None => {
                    cache.entries.insert(
                        idempotency_key.to_owned(),
                        SendEntry::InFlight(fingerprint.clone()),
                    );
                    cache.order.push_back(idempotency_key.to_owned());
                    break;
                }
            }
        }
        drop(cache);

        let result = self.terminal_send(request);
        let mut cache = self
            .shared
            .send_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        match &result {
            Ok(value) => {
                cache.entries.insert(
                    idempotency_key.to_owned(),
                    SendEntry::Done(fingerprint, value.clone()),
                );
                cache.evict_done();
            }
            Err(_) => {
                cache.entries.remove(idempotency_key);
                if let Some(position) = cache.order.iter().position(|key| key == idempotency_key) {
                    cache.order.remove(position);
                }
            }
        }
        drop(cache);
        self.shared.send_cache_ready.notify_all();
        result.map(|result| SendOutcome {
            result,
            replayed: false,
        })
    }

    pub fn terminal_keys(
        &self,
        request: &crate::types::KeysRequest,
    ) -> Result<crate::types::KeysResult> {
        self.typed("terminal.keys", request.to_params()?)
    }

    pub fn terminal_interrupt(
        &self,
        request: &crate::types::InterruptRequest,
    ) -> Result<crate::types::InterruptResult> {
        self.typed("terminal.interrupt", request.to_params()?)
    }

    /// End the program: the documented hard stop (`terminal.kill`). There is
    /// no softer stop method in protocol generation 1.
    pub fn terminal_kill(&self, terminal: &TerminalAddress) -> Result<Terminal> {
        self.typed("terminal.kill", json!({"terminal":terminal}))
    }

    /// Remove a finished terminal record. This is the runtime half of
    /// archiving; it refuses a live terminal with the core's `live` error.
    pub fn terminal_remove(
        &self,
        terminal: &TerminalAddress,
    ) -> Result<crate::types::RemoveResult> {
        self.typed("terminal.rm", json!({"terminal":terminal}))
    }

    pub fn terminal_keep(&self, terminal: &TerminalAddress, keep: bool) -> Result<Terminal> {
        self.typed("terminal.keep", json!({"terminal":terminal,"keep":keep}))
    }

    pub fn terminal_read(&self, request: &ReadRequest) -> Result<ReadResult> {
        self.typed("terminal.read", serde_json::to_value(request)?)
    }

    pub fn server_status(&self) -> Result<ServerStatus> {
        self.typed("server.status", json!({}))
    }

    pub fn server_stop(&self) -> Result<StopResult> {
        self.typed("server.stop", json!({}))
    }

    pub fn server_restart(&self) -> Result<StopResult> {
        self.typed("server.restart", json!({}))
    }

    pub fn attach(&self, request: &AttachRequest) -> Result<Attachment> {
        self.shared.hello.require_method("terminal.attach")?;
        self.shared.hello.require_feature("attach")?;
        let value = self.call("terminal.attach", request.to_params()?)?;
        let result: AttachResult = serde_json::from_value(value)?;
        let queue = Arc::new(StreamQueue::new(MAX_STREAM_BUFFER_BYTES));
        self.shared
            .register(result.channel, ChannelSink::Stream(Arc::clone(&queue)))?;
        Ok(Attachment::new(
            result,
            request.observe,
            queue,
            Arc::clone(&self.shared.writer),
            Arc::clone(&self.shared) as Arc<dyn ChannelLifecycle>,
        ))
    }

    pub fn subscribe_events(&self, filter: &EventFilter) -> Result<EventSubscription> {
        self.shared.hello.require_method("events.subscribe")?;
        let value = self.call("events.subscribe", filter.to_params()?)?;
        let result: SubscribeResult = serde_json::from_value(value)?;
        let Some(channel) = result.channel else {
            return Err(HcError::Unsupported {
                method: "events.subscribe.channel".into(),
                reason: "the connection did not return a framed event channel".into(),
            });
        };
        let queue = Arc::new(EventQueue::new(MAX_STREAM_BUFFER_BYTES));
        self.shared
            .register(channel, ChannelSink::Events(Arc::clone(&queue)))?;
        EventSubscription::new(
            result,
            filter.is_filtered(),
            queue,
            Arc::clone(&self.shared.writer),
            Arc::clone(&self.shared) as Arc<dyn ChannelLifecycle>,
            0,
        )
    }

    /// Convenience for callers that only need one notice.
    pub fn next_event(&self, timeout: Duration) -> Result<StreamNotice> {
        let subscription = self.subscribe_events(&EventFilter::default())?;
        subscription.next_notice(timeout)
    }

    fn typed<T: serde::de::DeserializeOwned>(&self, method: &str, params: Value) -> Result<T> {
        self.typed_with_timeout(method, params, self.request_timeout)
    }

    fn typed_with_timeout<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<T> {
        let value = self.call_with_timeout(method, params, timeout)?;
        Ok(serde_json::from_value(value)?)
    }

    /// Close this client connection. It does not stop the core or any
    /// terminal; each open stream receives a local CLOSE first.
    pub fn close(mut self) -> Result<()> {
        self.shutdown();
        Ok(())
    }

    fn shutdown(&mut self) {
        let channels = {
            let routing = self
                .shared
                .routing
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            routing.channels.keys().copied().collect::<Vec<_>>()
        };
        for channel in channels {
            let _ = self.shared.writer.send(&Frame::Close {
                channel,
                payload: Vec::new(),
            });
        }
        self.shared.fail("client connection closed".into());
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

impl std::fmt::Debug for FramedConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FramedConnection")
            .field("core", &self.shared.hello.core.id)
            .field("closed", &self.shared.is_closed())
            .finish()
    }
}

impl Drop for FramedConnection {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn same_launch(terminal: &Terminal, request: &RunRequest) -> bool {
    if let Some(cwd) = &request.cwd {
        if terminal.cwd != *cwd {
            return false;
        }
    }
    if !request.argv.is_empty() {
        terminal.argv == request.argv
    } else {
        // `line` execution argv is decided by the machine's shell; without a
        // provable match, refuse to reuse instead of guessing.
        request.line.is_none()
    }
}

fn timeout_for_wait(base: Duration, wait: bool, requested: Option<f64>) -> Duration {
    if !wait {
        return base;
    }
    let seconds = requested.unwrap_or(180.0).min(600.0);
    base.max(Duration::from_secs_f64(seconds + 10.0))
}

fn reader_loop(mut stdout: ChildStdout, shared: Arc<Shared>) {
    let mut reader = FrameReader::new(&mut stdout);
    loop {
        match reader.read_frame() {
            Ok(Some(frame)) => route_frame(&shared, frame),
            Ok(None) => {
                shared.fail("hc closed the connection".into());
                break;
            }
            Err(error) => {
                shared.fail(format!("hc framing failed: {error}"));
                break;
            }
        }
    }
}

fn route_frame(shared: &Shared, frame: Frame) {
    match frame {
        Frame::Data {
            channel: 0,
            payload,
        } => shared.dispatch_json(&payload),
        Frame::Close {
            channel: 0,
            payload,
        } => shared.fail(format!(
            "core closed channel 0: {}",
            String::from_utf8_lossy(&payload)
        )),
        other => shared.route(other),
    }
}

/// Read one newline-terminated line without over-reading into framed bytes.
fn read_limited_line(reader: &mut impl Read, limit: usize) -> Result<String> {
    let mut bytes = Vec::new();
    loop {
        let mut byte = [0u8; 1];
        match reader.read(&mut byte)? {
            0 => return Err(HcError::Protocol("hc closed before the hello reply".into())),
            _ => bytes.push(byte[0]),
        }
        if bytes.len() > limit {
            return Err(HcError::Protocol("hc hello line too large".into()));
        }
        if byte[0] == b'\n' {
            break;
        }
    }
    String::from_utf8(bytes).map_err(|_| HcError::Protocol("hc hello reply is not UTF-8".into()))
}

/// Legacy plain-mode client retained for CLI probes and read-only snapshots.
#[derive(Debug, Clone)]
pub struct HcClient {
    binary: OsString,
    call_timeout: Duration,
}

impl Default for HcClient {
    fn default() -> Self {
        Self::new(std::env::var_os("UKK_HC_BIN").unwrap_or_else(|| OsString::from("hc")))
    }
}

impl HcClient {
    pub fn new(binary: impl Into<OsString>) -> Self {
        Self {
            binary: binary.into(),
            call_timeout: DEFAULT_CALL_TIMEOUT,
        }
    }

    pub fn with_timeout(binary: impl Into<OsString>, timeout: Duration) -> Self {
        Self {
            binary: binary.into(),
            call_timeout: timeout,
        }
    }

    pub fn binary(&self) -> &OsStr {
        &self.binary
    }

    pub fn framed(&self) -> Result<FramedConnection> {
        FramedConnection::connect(&self.binary)
    }

    /// One request on a fresh plain-mode connection.
    ///
    /// The child is always killed and reaped, including on write failure,
    /// read failure and timeout. A hung `hc` therefore cannot pin a Host
    /// worker forever.
    pub fn call(&self, method: &str, params: Value) -> Result<Value> {
        self.call_with_timeout(method, params, self.call_timeout)
    }

    pub fn call_with_timeout(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value> {
        let mut child = Command::new(&self.binary)
            .arg("connect")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let result = self.one_shot(&mut child, method, params, timeout);
        let _ = child.kill();
        let _ = child.wait();
        result
    }

    fn one_shot(
        &self,
        child: &mut Child,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value> {
        crate::protocol::write_request(child, 1, method, params)?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| HcError::Protocol("hc stdout unavailable".into()))?;
        let (sender, receiver) = mpsc::sync_channel(1);
        let reader = thread::Builder::new()
            .name("hc-one-shot-reader".into())
            .spawn(move || {
                let mut stdout = stdout;
                let mut line = Vec::new();
                let result = (|| -> Result<String> {
                    loop {
                        let mut byte = [0u8; 1];
                        match stdout.read(&mut byte)? {
                            0 => {
                                return Err(HcError::Protocol("hc closed before responding".into()))
                            }
                            _ => line.push(byte[0]),
                        }
                        if byte[0] == b'\n' {
                            break;
                        }
                        if line.len() > 4 * 1024 * 1024 {
                            return Err(HcError::Protocol("hc reply too large".into()));
                        }
                    }
                    String::from_utf8(line)
                        .map_err(|_| HcError::Protocol("hc reply is not UTF-8".into()))
                })();
                let _ = sender.send(result);
            })?;
        let outcome = match receiver.recv_timeout(timeout) {
            Ok(Ok(line)) => crate::protocol::parse_reply(&line, 1),
            Ok(Err(error)) => Err(error),
            Err(RecvTimeoutError::Timeout) => Err(HcError::Timeout {
                operation: method.to_owned(),
                seconds: timeout.as_secs(),
            }),
            Err(RecvTimeoutError::Disconnected) => {
                Err(HcError::Protocol("hc response reader vanished".into()))
            }
        };
        // The reader owns stdout; killing the child closes it and lets the
        // reader finish, then the handle is joined without leaking a thread.
        let _ = child.kill();
        let _ = child.wait();
        let _ = reader.join();
        outcome
    }

    /// Legacy plain-mode notification watch. Not the typed event API; use a
    /// [`FramedConnection`] subscription for sequence checking.
    pub fn watch<F>(&self, mut emit: F) -> Result<()>
    where
        F: FnMut(Value) -> Result<()>,
    {
        let mut child = Command::new(&self.binary)
            .arg("connect")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let result = (|| {
            crate::protocol::write_request(&mut child, 1, "events.subscribe", json!({}))?;
            let stdout = child
                .stdout
                .take()
                .ok_or_else(|| HcError::Protocol("hc stdout unavailable".into()))?;
            let mut reader = std::io::BufReader::new(stdout);
            let mut line = String::new();
            if reader.read_line(&mut line)? == 0 {
                return Err(HcError::Protocol(
                    "hc closed before subscription snapshot".into(),
                ));
            }
            let snapshot = crate::protocol::parse_reply(&line, 1)?;
            emit(json!({"kind":"snapshot","data":snapshot}))?;
            loop {
                line.clear();
                if reader.read_line(&mut line)? == 0 {
                    return Err(HcError::Protocol(
                        "hc notification stream disconnected".into(),
                    ));
                }
                let value: Value = serde_json::from_str(&line)?;
                if value.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
                    return Err(HcError::Protocol("invalid JSON-RPC notification".into()));
                }
                if value.get("id").is_some() {
                    return Err(HcError::Protocol(
                        "unexpected response on notification stream".into(),
                    ));
                }
                if !matches!(
                    value.get("method").and_then(Value::as_str),
                    Some("event" | "heartbeat" | "missed")
                ) {
                    return Err(HcError::Protocol("unexpected notification method".into()));
                }
                emit(json!({"kind":"notification","data":value}))?;
            }
        })();
        let _ = child.kill();
        let _ = child.wait();
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wait_timeout_accounts_for_documented_default() {
        assert_eq!(
            timeout_for_wait(Duration::from_secs(30), true, None),
            Duration::from_secs(190)
        );
        assert_eq!(
            timeout_for_wait(Duration::from_secs(30), false, Some(5.0)),
            Duration::from_secs(30)
        );
    }

    fn test_shared() -> Arc<Shared> {
        use crate::types::CoreInfo;
        use std::collections::BTreeSet;

        Arc::new(Shared {
            hello: Hello {
                generation: 1,
                version: "test".into(),
                core: CoreInfo {
                    id: "core".into(),
                    name: "test".into(),
                },
                framed: true,
                methods: BTreeSet::new(),
                features: BTreeSet::new(),
            },
            writer: Arc::new(FrameWriter::new(Vec::new())),
            next_id: AtomicU64::new(1),
            pending: Mutex::new(HashMap::new()),
            routing: Mutex::new(Routing::default()),
            closed: Mutex::new(None),
            send_cache: Mutex::new(SendCache::default()),
            send_cache_ready: Condvar::new(),
        })
    }

    #[test]
    fn detach_drops_orphans_so_a_reused_channel_starts_clean() {
        let shared = test_shared();
        shared.route(Frame::Data {
            channel: 7,
            payload: b"stale".to_vec(),
        });
        shared.detach(7);
        let queue = Arc::new(crate::stream::StreamQueue::new(1024));
        shared
            .register(7, ChannelSink::Stream(Arc::clone(&queue)))
            .unwrap();
        assert!(matches!(
            queue.next_event(Duration::from_millis(1)),
            Err(HcError::Timeout { .. })
        ));
    }

    #[test]
    fn registration_after_connection_failure_is_typed_closed() {
        let shared = test_shared();
        shared.fail("boom".into());
        let queue = Arc::new(crate::stream::StreamQueue::new(1024));
        let error = shared.register(8, ChannelSink::Stream(queue)).unwrap_err();
        assert!(matches!(error, HcError::Closed(reason) if reason.contains("boom")));
    }

    #[test]
    fn registration_never_drops_or_reorders_frames() {
        use crate::stream::StreamQueue;
        use crate::types::CoreInfo;
        use std::collections::BTreeSet;

        let shared = Arc::new(Shared {
            hello: Hello {
                generation: 1,
                version: "test".into(),
                core: CoreInfo {
                    id: "core".into(),
                    name: "test".into(),
                },
                framed: true,
                methods: BTreeSet::new(),
                features: BTreeSet::new(),
            },
            writer: Arc::new(FrameWriter::new(Vec::new())),
            next_id: AtomicU64::new(1),
            pending: Mutex::new(HashMap::new()),
            routing: Mutex::new(Routing::default()),
            closed: Mutex::new(None),
            send_cache: Mutex::new(SendCache::default()),
            send_cache_ready: Condvar::new(),
        });
        let queue = Arc::new(StreamQueue::new(1024 * 1024));
        const TOTAL: usize = 60;
        let router = {
            let shared = Arc::clone(&shared);
            thread::spawn(move || {
                for index in 0..TOTAL {
                    shared.route(Frame::Data {
                        channel: 9,
                        payload: vec![(index % 251) as u8],
                    });
                }
            })
        };
        thread::yield_now();
        shared
            .register(9, ChannelSink::Stream(Arc::clone(&queue)))
            .unwrap();
        router.join().unwrap();
        for index in 0..TOTAL {
            match queue.next_event(Duration::from_millis(10)).unwrap() {
                crate::stream::StreamEvent::Data(payload) => {
                    assert_eq!(payload, vec![(index % 251) as u8], "frame {index}");
                }
                other => panic!("unexpected event: {other:?}"),
            }
        }
    }

    #[test]
    fn same_launch_requires_matching_argv_and_cwd() {
        let terminal = crate::types::Terminal::from_value(json!({
            "address":"t7","argv":["codex"],"cwd":"/work","state":"live",
            "created_ms":1,"screen":"exact","cols":80,"rows":24
        }))
        .unwrap();
        let mut request = RunRequest {
            argv: vec!["codex".into()],
            cwd: Some("/work".into()),
            ..RunRequest::default()
        };
        assert!(same_launch(&terminal, &request));
        request.argv = vec!["claude".into()];
        assert!(!same_launch(&terminal, &request));
    }
}
