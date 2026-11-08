//! Process ownership, request correlation and event subscription.

use crate::config::{ABSOLUTE_MAX_FRAME_BYTES, MAX_STDERR_LINE_BYTES, MIN_FRAME_BYTES};
use crate::error::OmpError;
use crate::event::{parse_event, Event, EventKind};
use crate::io::{read_bounded_line, LineRead};
use crate::redact::bounded_message;
use crate::wire::{ReadyInfo, ResponseFrame};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::io::{BufReader, BufWriter, Write};
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::config::OmpConfig;

/// Bound on queued events per subscriber. A subscriber that stops consuming
/// backpressures omp's stdout instead of growing Host memory. Disconnected
/// subscribers are pruned.
const SUBSCRIBER_QUEUE: usize = 4096;
/// A line on stderr that never terminates is drained but not buffered beyond
/// this many bytes.
const STDERR_DRAIN_MAX_LINE: usize = 256 * 1024;

#[derive(Clone, Debug, PartialEq)]
pub enum OmpState {
    Running,
    Exited { code: Option<i32>, detail: String },
}

impl OmpState {
    pub fn is_running(&self) -> bool {
        matches!(self, Self::Running)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Ack {
    pub command: String,
    pub data: Option<Value>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PromptAck {
    /// `Some(false)` only when omp explicitly reported a local command that
    /// did not invoke the agent. `None` means lifecycle events decide.
    pub agent_invoked: Option<bool>,
    pub data: Option<Value>,
}

/// Receives typed events from one [`OmpClient`]. Dropping the receiver
/// unsubscribes; it never stops the child.
pub struct EventReceiver {
    rx: Receiver<Event>,
}

impl EventReceiver {
    pub fn recv(&self) -> Result<Event, mpsc::RecvError> {
        self.rx.recv()
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Result<Event, mpsc::RecvTimeoutError> {
        self.rx.recv_timeout(timeout)
    }

    pub fn try_recv(&self) -> Result<Event, TryRecvError> {
        self.rx.try_recv()
    }
}

/// One omp child process. Cloning is not supported: the Host owns exactly one
/// client per running omp session.
pub struct OmpClient {
    shared: Arc<Shared>,
}

impl OmpClient {
    /// Spawn the configured child and block until `ready` or the startup
    /// deadline. Failure always kills and reaps the child.
    pub fn spawn(config: OmpConfig) -> Result<Self, OmpError> {
        let mut command = Command::new(&config.binary);
        command
            .args(&config.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(cwd) = &config.cwd {
            command.current_dir(cwd);
        }
        let mut child = command.spawn().map_err(|error| OmpError::Spawn {
            binary: config.binary.display().to_string(),
            message: error.to_string(),
        })?;
        let stdout = child.stdout.take().ok_or_else(|| OmpError::Spawn {
            binary: config.binary.display().to_string(),
            message: "child stdout was not piped".into(),
        })?;
        let stderr = child.stderr.take().ok_or_else(|| OmpError::Spawn {
            binary: config.binary.display().to_string(),
            message: "child stderr was not piped".into(),
        })?;
        let stdin = child.stdin.take().ok_or_else(|| OmpError::Spawn {
            binary: config.binary.display().to_string(),
            message: "child stdin was not piped".into(),
        })?;

        let shared = Arc::new(Shared {
            config: config.clone(),
            cap: AtomicUsize::new(config.max_frame_bytes),
            child: Mutex::new(Some(child)),
            stdin: Mutex::new(Some(BufWriter::new(stdin))),
            pending: Mutex::new(HashMap::new()),
            subscribers: Mutex::new(Vec::new()),
            next_subscriber: AtomicU64::new(0),
            sequence: AtomicU64::new(0),
            next_request: AtomicU64::new(0),
            state: Mutex::new(RunState {
                running: true,
                exit: None,
            }),
            ready: Mutex::new(None),
            stderr: Mutex::new(VecDeque::new()),
            shutting_down: AtomicBool::new(false),
        });

        let (ready_tx, ready_rx) = mpsc::channel();
        let reader_shared = Arc::clone(&shared);
        thread::Builder::new()
            .name("ukk-omp-reader".into())
            .spawn(move || read_loop(reader_shared, stdout, ready_tx))
            .map_err(|error| {
                shared.kill_and_reap();
                OmpError::Spawn {
                    binary: config.binary.display().to_string(),
                    message: format!("could not start the stdout reader: {error}"),
                }
            })?;
        let stderr_shared = Arc::clone(&shared);
        let _ = thread::Builder::new()
            .name("ukk-omp-stderr".into())
            .spawn(move || stderr_loop(stderr_shared, stderr));

        match ready_rx.recv_timeout(config.startup_timeout) {
            Ok(Ok(_ready)) => Ok(Self { shared }),
            Ok(Err(message)) => {
                shared.kill_and_reap();
                Err(OmpError::Protocol(message))
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let stderr_tail = shared.stderr_tail();
                shared.kill_and_reap();
                Err(OmpError::StartupTimeout {
                    waited: config.startup_timeout,
                    stderr_tail,
                })
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let stderr_tail = shared.stderr_tail();
                shared.kill_and_reap();
                Err(OmpError::Transport(format!(
                    "omp stdout reader stopped before the ready handshake{}",
                    if stderr_tail.is_empty() {
                        String::new()
                    } else {
                        format!(" ({})", stderr_tail.join(" | "))
                    }
                )))
            }
        }
    }

    pub fn ready(&self) -> Option<ReadyInfo> {
        self.shared.ready.lock().ok()?.clone()
    }

    pub fn state(&self) -> OmpState {
        let state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        match &state.exit {
            None => OmpState::Running,
            Some(exit) => OmpState::Exited {
                code: exit.code,
                detail: exit.detail.clone(),
            },
        }
    }

    /// Subscribe to typed events. The Host subscribes immediately after
    /// `spawn`, before sending a prompt, so no event can be missed.
    pub fn subscribe(&self) -> EventReceiver {
        let (tx, rx) = mpsc::sync_channel(SUBSCRIBER_QUEUE);
        let id = self.shared.next_subscriber.fetch_add(1, Ordering::SeqCst);
        self.shared
            .subscribers
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .push((id, tx));
        EventReceiver { rx }
    }

    pub fn prompt(&self, message: &str) -> Result<PromptAck, OmpError> {
        let body = self.message_frame(message)?;
        let response = self.request("prompt", body)?;
        let agent_invoked = response
            .data
            .as_ref()
            .and_then(|data| data.get("agentInvoked"))
            .and_then(Value::as_bool);
        Ok(PromptAck {
            agent_invoked,
            data: response.data,
        })
    }

    pub fn steer(&self, message: &str) -> Result<Ack, OmpError> {
        let body = self.message_frame(message)?;
        self.command("steer", body)
    }

    pub fn follow_up(&self, message: &str) -> Result<Ack, OmpError> {
        let body = self.message_frame(message)?;
        self.command("follow_up", body)
    }

    pub fn abort(&self) -> Result<Ack, OmpError> {
        self.command("abort", json!({}))
    }

    /// Close stdin, wait for a graceful exit, then kill and reap. Dropping the
    /// client performs the same shutdown. Idempotent.
    pub fn shutdown(&self) {
        self.shared.shutdown();
    }

    /// Stop the child immediately and reap it. Never called by UI close.
    pub fn kill(&self) -> Result<(), OmpError> {
        self.shared.kill_and_reap();
        Ok(())
    }

    pub fn stderr_tail(&self) -> Vec<String> {
        self.shared.stderr_tail()
    }

    fn message_frame(&self, message: &str) -> Result<Value, OmpError> {
        if message.trim().is_empty() {
            return Err(OmpError::InvalidArgument("message is empty".into()));
        }
        let cap = self.shared.cap.load(Ordering::SeqCst);
        if message.len() + 64 > cap {
            return Err(OmpError::InvalidArgument(format!(
                "message exceeds the {cap}-byte frame budget"
            )));
        }
        Ok(json!({ "message": message }))
    }

    fn command(&self, command: &str, body: Value) -> Result<Ack, OmpError> {
        let response = self.request(command, body)?;
        Ok(Ack {
            command: response.command,
            data: response.data,
        })
    }

    fn request(&self, command: &str, body: Value) -> Result<ResponseFrame, OmpError> {
        if !self.state().is_running() {
            return Err(OmpError::NotRunning(
                self.state()
                    .as_exit_detail()
                    .unwrap_or_else(|| "stdout is closed".into()),
            ));
        }
        let id = format!(
            "ukk-{}",
            self.shared.next_request.fetch_add(1, Ordering::SeqCst) + 1
        );
        let (tx, rx) = mpsc::sync_channel(1);
        self.shared
            .pending
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .insert(id.clone(), tx);
        let mut frame = body;
        frame["id"] = Value::String(id.clone());
        frame["type"] = Value::String(command.to_owned());
        if let Err(error) = self.shared.send_frame(&frame) {
            self.shared
                .pending
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .remove(&id);
            return Err(error);
        }
        let waited = self.shared.config.request_timeout;
        match rx.recv_timeout(waited) {
            Ok(Ok(response)) => {
                if response.success {
                    Ok(response)
                } else {
                    Err(OmpError::CommandRejected {
                        command: command.to_owned(),
                        message: bounded_message(
                            response.error.as_deref().unwrap_or("no error text"),
                            1024,
                        ),
                        code: response.code,
                    })
                }
            }
            Ok(Err(message)) => Err(OmpError::Transport(message)),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.shared
                    .pending
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner())
                    .remove(&id);
                Err(OmpError::RequestTimeout {
                    command: command.to_owned(),
                    waited,
                })
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err(OmpError::Transport("omp event reader stopped".into()))
            }
        }
    }
}

impl OmpState {
    fn as_exit_detail(&self) -> Option<String> {
        match self {
            Self::Running => None,
            Self::Exited { detail, .. } => Some(detail.clone()),
        }
    }
}

impl Drop for OmpClient {
    fn drop(&mut self) {
        self.shared.shutdown();
    }
}

impl std::fmt::Debug for OmpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OmpClient")
            .field("state", &self.state())
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug)]
struct ExitInfo {
    code: Option<i32>,
    detail: String,
}

struct RunState {
    #[allow(dead_code)]
    running: bool,
    exit: Option<ExitInfo>,
}

struct Shared {
    config: OmpConfig,
    cap: AtomicUsize,
    child: Mutex<Option<Child>>,
    stdin: Mutex<Option<BufWriter<ChildStdin>>>,
    pending: Mutex<HashMap<String, SyncSender<Result<ResponseFrame, String>>>>,
    subscribers: Mutex<Vec<(u64, SyncSender<Event>)>>,
    next_subscriber: AtomicU64,
    sequence: AtomicU64,
    next_request: AtomicU64,
    state: Mutex<RunState>,
    ready: Mutex<Option<ReadyInfo>>,
    stderr: Mutex<VecDeque<String>>,
    shutting_down: AtomicBool,
}

impl Shared {
    fn send_frame(&self, frame: &Value) -> Result<(), OmpError> {
        if let Some(detail) = self.exit_detail() {
            return Err(OmpError::NotRunning(detail));
        }
        let mut bytes = serde_json::to_vec(frame)
            .map_err(|error| OmpError::InvalidArgument(error.to_string()))?;
        let cap = self.cap.load(Ordering::SeqCst);
        if bytes.len() + 1 > cap {
            return Err(OmpError::InvalidArgument(format!(
                "command frame exceeds the {cap}-byte transport limit"
            )));
        }
        bytes.push(b'\n');
        let mut guard = self
            .stdin
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let Some(writer) = guard.as_mut() else {
            return Err(OmpError::NotRunning("stdin is closed".into()));
        };
        if let Err(error) = writer.write_all(&bytes).and_then(|_| writer.flush()) {
            drop(guard);
            let detail = format!("writing to omp failed: {error}");
            self.mark_exited(None, detail.clone());
            return Err(OmpError::Transport(detail));
        }
        Ok(())
    }

    fn publish(&self, kind: EventKind, raw: Value) {
        let sequence = self.sequence.fetch_add(1, Ordering::SeqCst) + 1;
        let event = Event {
            sequence,
            kind,
            raw,
        };
        let senders = self
            .subscribers
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone();
        let mut dead = Vec::new();
        for (id, sender) in senders {
            if sender.send(event.clone()).is_err() {
                dead.push(id);
            }
        }
        if !dead.is_empty() {
            self.subscribers
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .retain(|(id, _)| !dead.contains(id));
        }
    }

    fn stderr_tail(&self) -> Vec<String> {
        self.stderr
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .iter()
            .cloned()
            .collect()
    }

    fn exit_detail(&self) -> Option<String> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        state.exit.as_ref().map(|exit| exit.detail.clone())
    }

    fn mark_exited(&self, code: Option<i32>, detail: String) {
        {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            if state.exit.is_some() {
                return;
            }
            state.running = false;
            state.exit = Some(ExitInfo {
                code,
                detail: bounded_message(&detail, 1024),
            });
        }
        self.stdin
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .take();
        let pending: Vec<_> = self
            .pending
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .drain()
            .collect();
        let detail = self.exit_detail().unwrap_or_else(|| "omp stopped".into());
        for (_, sender) in pending {
            let _ = sender.send(Err(detail.clone()));
        }
    }

    fn try_wait_status(&self) -> Option<Option<i32>> {
        let mut guard = self.child.lock().ok()?;
        let child = guard.as_mut()?;
        child.try_wait().ok().flatten().map(|status| status.code())
    }

    /// Wait briefly for the child to be reapable so an exit code can be
    /// attributed to the event stream ending. Returns `None` if it is still
    /// running at the deadline.
    fn wait_exit_status(&self, timeout: Duration) -> Option<Option<i32>> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.try_wait_status() {
                return Some(status);
            }
            if Instant::now() >= deadline {
                return None;
            }
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn kill_and_reap(&self) {
        let mut guard = self
            .child
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        self.stdin
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .take();
        if let Some(child) = guard.as_mut() {
            let _ = child.kill();
        }
        if let Some(mut child) = guard.take() {
            let _ = child.wait();
        }
        drop(guard);
        self.mark_exited(None, "omp child was stopped".into());
    }

    fn shutdown(&self) {
        if self.shutting_down.swap(true, Ordering::SeqCst) {
            return;
        }
        self.stdin
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .take();
        let deadline = Instant::now() + self.config.shutdown_grace;
        loop {
            let finished = {
                let mut guard = self
                    .child
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner());
                match guard.as_mut() {
                    Some(child) => match child.try_wait() {
                        Ok(Some(_)) => true,
                        Ok(None) => false,
                        Err(_) => true,
                    },
                    None => true,
                }
            };
            if finished {
                break;
            }
            if Instant::now() >= deadline {
                let mut guard = self
                    .child
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner());
                if let Some(child) = guard.as_mut() {
                    let _ = child.kill();
                }
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        let code = self.try_wait_status().flatten();
        if let Some(mut child) = self
            .child
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .take()
        {
            let _ = child.wait();
        }
        self.mark_exited(code, "host closed stdin and stopped the omp child".into());
    }
}

fn stderr_loop(shared: Arc<Shared>, stderr: ChildStderr) {
    let mut reader = BufReader::new(stderr);
    loop {
        match read_bounded_line(&mut reader, MAX_STDERR_LINE_BYTES, STDERR_DRAIN_MAX_LINE) {
            Ok(LineRead::Eof) | Err(_) => return,
            Ok(LineRead::Oversized { .. }) => {
                push_stderr(&shared, "[stderr line exceeded the diagnostic limit]");
            }
            Ok(LineRead::Line(bytes)) => {
                let text = String::from_utf8_lossy(&bytes);
                let text = text.trim_end_matches('\r');
                if !text.trim().is_empty() {
                    push_stderr(&shared, text);
                }
            }
        }
    }
}

fn push_stderr(shared: &Shared, line: &str) {
    let line = bounded_message(line, MAX_STDERR_LINE_BYTES);
    let limit = shared.config.max_stderr_lines.max(1);
    let mut tail = shared
        .stderr
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    while tail.len() >= limit {
        tail.pop_front();
    }
    tail.push_back(line);
}

fn read_loop(
    shared: Arc<Shared>,
    stdout: ChildStdout,
    ready_tx: mpsc::Sender<Result<ReadyInfo, String>>,
) {
    let mut reader = BufReader::new(stdout);
    let mut handshake_done = false;
    loop {
        let cap = shared.cap.load(Ordering::SeqCst);
        let line = match read_bounded_line(&mut reader, cap, cap.saturating_mul(2)) {
            Ok(line) => line,
            Err(error) => {
                let code = shared
                    .wait_exit_status(Duration::from_millis(1000))
                    .flatten();
                shared.mark_exited(code, format!("reading omp stdout failed: {error}"));
                // A child blocked on a full stdout pipe would otherwise stay
                // alive until Host shutdown. kill_and_reap is idempotent.
                shared.kill_and_reap();
                return;
            }
        };
        match line {
            LineRead::Eof => {
                let code = shared
                    .wait_exit_status(Duration::from_millis(1000))
                    .flatten();
                shared.mark_exited(code, "omp closed stdout".into());
                return;
            }
            LineRead::Oversized { bytes } => {
                if !handshake_done {
                    let _ = ready_tx.send(Err(format!(
                        "ready frame exceeded the {cap}-byte frame limit"
                    )));
                    shared.mark_exited(None, "oversized ready frame".into());
                    return;
                }
                shared.publish(
                    EventKind::OversizedFrame { limit: cap },
                    json!({"type":"ukk_oversized_frame","bytes":bytes}),
                );
            }
            LineRead::Line(bytes) => {
                let text = String::from_utf8_lossy(&bytes);
                let trimmed = text.trim_end_matches('\r').trim();
                if trimmed.is_empty() {
                    continue;
                }
                let value: Value = match serde_json::from_str(trimmed) {
                    Ok(value) => value,
                    Err(error) => {
                        if !handshake_done {
                            let _ = ready_tx
                                .send(Err(format!("first stdout frame is not JSON: {error}")));
                            shared.mark_exited(None, "invalid ready frame".into());
                            return;
                        }
                        shared.publish(
                            EventKind::MalformedFrame {
                                detail: bounded_message(&error.to_string(), 256),
                                bytes: bytes.len(),
                            },
                            json!({"type":"ukk_malformed_frame"}),
                        );
                        continue;
                    }
                };
                if !handshake_done {
                    match ReadyInfo::from_value(&value) {
                        Ok(ready) => {
                            let advertised = ready
                                .max_frame_bytes
                                .clamp(MIN_FRAME_BYTES, ABSOLUTE_MAX_FRAME_BYTES);
                            let configured = shared.config.max_frame_bytes.max(MIN_FRAME_BYTES);
                            shared
                                .cap
                                .store(advertised.min(configured), Ordering::SeqCst);
                            *shared
                                .ready
                                .lock()
                                .unwrap_or_else(|poison| poison.into_inner()) = Some(ready.clone());
                            handshake_done = true;
                            let _ = ready_tx.send(Ok(ready));
                        }
                        Err(message) => {
                            let _ = ready_tx.send(Err(message));
                            shared.mark_exited(None, "unsupported ready frame".into());
                            return;
                        }
                    }
                    continue;
                }
                dispatch_frame(&shared, value);
            }
        }
    }
}

fn dispatch_frame(shared: &Shared, value: Value) {
    match value.get("type").and_then(Value::as_str) {
        Some("response") => match ResponseFrame::from_value(&value) {
            Ok(response) => {
                let delivered = response.id.as_ref().is_some_and(|id| {
                    shared
                        .pending
                        .lock()
                        .unwrap_or_else(|poison| poison.into_inner())
                        .remove(id)
                        .is_some_and(|sender| sender.send(Ok(response.clone())).is_ok())
                });
                if delivered {
                    return;
                }
                if response.success {
                    shared.publish(
                        EventKind::Unsupported {
                            frame_type: "response".into(),
                            detail: format!("uncorrelated response for `{}`", response.command),
                        },
                        value,
                    );
                } else {
                    shared.publish(
                        EventKind::ProtocolError {
                            message: bounded_message(
                                response.error.as_deref().unwrap_or("command failed"),
                                1024,
                            ),
                        },
                        value,
                    );
                }
            }
            Err(message) => shared.publish(
                EventKind::ProtocolError { message },
                value,
            ),
        },
        Some("extension_ui_request") => {
            let id = value
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            let method = value
                .get("method")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_owned();
            let requires_response = matches!(
                method.as_str(),
                "select" | "confirm" | "input" | "editor"
            );
            if requires_response && !id.is_empty() {
                let _ = shared.send_frame(&json!({
                    "type": "extension_ui_response",
                    "id": id,
                    "cancelled": true
                }));
            }
            shared.publish(
                EventKind::ExtensionUi {
                    id,
                    method,
                    requires_response,
                },
                value,
            );
        }
        Some("host_tool_call") => {
            if let Some(id) = value.get("id").and_then(Value::as_str) {
                let _ = shared.send_frame(&json!({
                    "type": "host_tool_result",
                    "id": id,
                    "isError": true,
                    "result": {
                        "content": [{
                            "type": "text",
                            "text": "ukk Host does not register omp host tools"
                        }],
                        "isError": true
                    }
                }));
            }
            shared.publish(
                EventKind::Unsupported {
                    frame_type: "host_tool_call".into(),
                    detail: "host tools are not registered by this Host slice".into(),
                },
                value,
            );
        }
        Some("host_uri_request") => {
            if let Some(id) = value.get("id").and_then(Value::as_str) {
                let _ = shared.send_frame(&json!({
                    "type": "host_uri_result",
                    "id": id,
                    "isError": true,
                    "error": "ukk Host does not register omp host URI schemes"
                }));
            }
            shared.publish(
                EventKind::Unsupported {
                    frame_type: "host_uri_request".into(),
                    detail: "host URI schemes are not registered by this Host slice".into(),
                },
                value,
            );
        }
        Some("rpc_chunk") => shared.publish(
            EventKind::Unsupported {
                frame_type: "rpc_chunk".into(),
                detail:
                    "protocol v2 was not negotiated; chunked frames cannot be reassembled by this client"
                        .into(),
            },
            value,
        ),
        Some("rpc_frame_error") => shared.publish(parse_event(&value), value),
        Some(_) => shared.publish(parse_event(&value), value),
        None => shared.publish(
            EventKind::Unknown {
                frame_type: "missing type".into(),
            },
            value,
        ),
    }
}
