//! Supervised helper process for untrusted plugins.
//!
//! One helper owns one child process. The host writes newline-delimited JSON
//! ([`ukk_plugin_api::wire`]) and reads bounded frames; every request has a
//! deadline, and a helper that exits, replies out of protocol or floods the
//! pipe is marked closed instead of blocking the host.
//!
//! What a helper is **not** allowed to do: send commands, request grants, add
//! contributions or influence another plugin. There is no inbound command
//! method in the protocol, so those have to be forged through the host's own
//! dispatch path, which checks ownership and grants.
use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use ukk_plugin_api::wire::{
    EventFrame, HelloResult, IncomingFrame, RequestFrame, ResponseFrame, WireError,
};
use ukk_plugin_api::{limits, HELPER_PROTOCOL_VERSION};

use crate::audit::lock;
use crate::redact::redact;

/// Environment variables a helper may inherit. Everything else is cleared so a
/// plugin cannot read host credentials from the environment.
const ENV_ALLOWLIST: &[&str] = &[
    "SYSTEMROOT",
    "WINDIR",
    "TEMP",
    "TMP",
    "TMPDIR",
    "HOME",
    "USERPROFILE",
    "LANG",
    "LC_ALL",
];

/// Time and size limits applied to one helper.
#[derive(Debug, Clone)]
pub struct HelperLimits {
    /// Deadline for a single request/response round trip.
    pub request_timeout: Duration,
    /// Time a helper gets to stop after `plugin.shutdown` before being killed.
    pub shutdown_grace: Duration,
    /// Maximum bytes in one protocol frame, including the newline.
    pub max_frame_bytes: usize,
    /// Maximum queued host events before drops are counted.
    pub event_queue_capacity: usize,
}

impl Default for HelperLimits {
    fn default() -> Self {
        Self {
            request_timeout: Duration::from_secs(2),
            shutdown_grace: Duration::from_millis(750),
            max_frame_bytes: limits::MAX_FRAME_BYTES,
            event_queue_capacity: limits::MAX_EVENT_QUEUE,
        }
    }
}

/// How to start a helper. The program must be an absolute path resolved by the
/// installer; a manifest's `entrypoint` name alone is never executed from
/// `PATH`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelperCommand {
    program: PathBuf,
    args: Vec<String>,
    env: Vec<(String, String)>,
    working_dir: Option<PathBuf>,
}

impl HelperCommand {
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            env: Vec::new(),
            working_dir: None,
        }
    }

    pub fn with_arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    pub fn with_working_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.working_dir = Some(dir.into());
        self
    }

    pub fn program(&self) -> &PathBuf {
        &self.program
    }

    pub fn args(&self) -> &[String] {
        &self.args
    }

    pub fn env(&self) -> &[(String, String)] {
        &self.env
    }

    pub fn working_dir(&self) -> Option<&PathBuf> {
        self.working_dir.as_ref()
    }
}

/// Why a helper request failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HelperError {
    /// The executable could not be started.
    Spawn { message: String },
    /// `HelperCommand::program` was not absolute.
    ProgramNotAbsolute { program: String },
    /// The helper is not running (never started, stopped or crashed).
    NotRunning,
    /// No response arrived inside the deadline.
    Timeout { method: String, timeout_ms: u64 },
    /// The process exited or its pipe closed unexpectedly.
    Crashed { detail: String },
    /// The helper wrote a frame larger than the configured limit.
    FrameTooLarge { limit: usize },
    /// A host-side frame (for example command arguments) exceeded the limit
    /// before it was sent. This is a caller error, never a plugin failure.
    RequestTooLarge { limit: usize, what: &'static str },
    /// The helper wrote something that is not a valid protocol frame.
    Protocol { detail: String },
    /// The helper answered with `ok = false`.
    Remote { code: String, message: String },
    /// Writing to the helper failed.
    Io { detail: String },
}

impl fmt::Display for HelperError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn { message } => write!(f, "cannot start helper: {message}"),
            Self::ProgramNotAbsolute { program } => {
                write!(f, "helper program `{program}` is not an absolute path")
            }
            Self::NotRunning => f.write_str("helper is not running"),
            Self::Timeout { method, timeout_ms } => {
                write!(f, "helper `{method}` timed out after {timeout_ms}ms")
            }
            Self::Crashed { detail } => write!(f, "helper crashed: {detail}"),
            Self::FrameTooLarge { limit } => {
                write!(f, "helper sent a frame larger than {limit} bytes")
            }
            Self::RequestTooLarge { limit, what } => {
                write!(f, "{what} exceeds the {limit}-byte frame limit")
            }
            Self::Protocol { detail } => {
                write!(f, "helper protocol violation: {}", redact(detail))
            }
            // `code` and `message` are helper-authored: one line, bounded.
            Self::Remote { code, message } => write!(
                f,
                "helper rejected the request (`{}`): {}",
                redact(code),
                redact(message)
            ),
            Self::Io { detail } => write!(f, "helper io error: {detail}"),
        }
    }
}

impl std::error::Error for HelperError {}

impl HelperError {
    /// Stable code fragment for audit records.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Spawn { .. } => "spawn_failed",
            Self::ProgramNotAbsolute { .. } => "program_not_absolute",
            Self::NotRunning => "not_running",
            Self::Timeout { .. } => "timeout",
            Self::Crashed { .. } => "crashed",
            Self::FrameTooLarge { .. } => "frame_too_large",
            Self::RequestTooLarge { .. } => "request_too_large",
            Self::Protocol { .. } => "protocol_violation",
            Self::Remote { .. } => "remote_error",
            Self::Io { .. } => "io_error",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CloseReason {
    Exited { code: Option<i32> },
    Timeout,
    Protocol { detail: String },
    FrameTooLarge { limit: usize },
    WriteFailed { detail: String },
    Stopped,
}

impl CloseReason {
    fn to_error(&self) -> HelperError {
        match self {
            Self::Exited { code } => HelperError::Crashed {
                detail: match code {
                    Some(code) => format!("process exited with code {code}"),
                    None => "process exited".to_owned(),
                },
            },
            Self::Protocol { detail } => HelperError::Protocol {
                detail: detail.clone(),
            },
            Self::FrameTooLarge { limit } => HelperError::FrameTooLarge { limit: *limit },
            Self::Timeout => HelperError::Timeout {
                method: "plugin.health".to_owned(),
                timeout_ms: 0,
            },
            Self::WriteFailed { detail } => HelperError::Crashed {
                detail: format!("stdin closed: {detail}"),
            },
            Self::Stopped => HelperError::NotRunning,
        }
    }
}

/// Result of stopping a helper.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShutdownReport {
    /// Whether the process had to be killed after the grace period.
    pub killed: bool,
    /// Exit code reported by the OS, when available.
    pub exit_code: Option<i32>,
}

struct Outbox {
    requests: VecDeque<Vec<u8>>,
    events: VecDeque<Vec<u8>>,
    dropped_events: u64,
    closed: bool,
}

/// A running helper process with bounded framing and per-request deadlines.
pub struct HelperProcess {
    plugin_id: String,
    limits: HelperLimits,
    child: Mutex<Child>,
    stdin: Mutex<Option<ChildStdin>>,
    pending: Mutex<HashMap<u64, Sender<ResponseFrame>>>,
    close_reason: Mutex<Option<CloseReason>>,
    outbox: Mutex<Outbox>,
    outbox_signal: Condvar,
    closed: AtomicBool,
    /// Set once the host has signaled the process to die.
    killed: AtomicBool,
    next_id: AtomicU64,
    reader_thread: Mutex<Option<JoinHandle<()>>>,
    writer_thread: Mutex<Option<JoinHandle<()>>>,
}

impl fmt::Debug for HelperProcess {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HelperProcess")
            .field("plugin_id", &self.plugin_id)
            .field("closed", &self.is_closed())
            .field("dropped_events", &self.dropped_events())
            .finish_non_exhaustive()
    }
}

impl HelperProcess {
    /// Start `command` and begin reading/writing protocol frames.
    pub fn spawn(
        plugin_id: &str,
        command: &HelperCommand,
        limits: HelperLimits,
    ) -> Result<Arc<Self>, HelperError> {
        if !command.program.is_absolute() {
            return Err(HelperError::ProgramNotAbsolute {
                program: command.program.display().to_string(),
            });
        }
        let mut process = Command::new(&command.program);
        process.args(&command.args);
        process.env_clear();
        for key in ENV_ALLOWLIST {
            if let Ok(value) = std::env::var(key) {
                process.env(key, value);
            }
        }
        for (key, value) in &command.env {
            process.env(key, value);
        }
        process
            .env("UKK_PLUGIN_ID", plugin_id)
            .env("UKK_PLUGIN_PROTOCOL", HELPER_PROTOCOL_VERSION.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // A helper's diagnostics are its own business; discarding stderr
            // keeps a chatty plugin from filling a pipe or the host log.
            .stderr(Stdio::null());
        if let Some(dir) = &command.working_dir {
            process.current_dir(dir);
        }
        let mut child = process.spawn().map_err(|error| HelperError::Spawn {
            message: error.to_string(),
        })?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let Some(stdout) = stdout else {
            let _ = child.kill();
            return Err(HelperError::Spawn {
                message: "helper stdout was not piped".into(),
            });
        };
        let process = Arc::new(Self {
            plugin_id: plugin_id.to_owned(),
            limits,
            child: Mutex::new(child),
            stdin: Mutex::new(stdin),
            pending: Mutex::new(HashMap::new()),
            close_reason: Mutex::new(None),
            outbox: Mutex::new(Outbox {
                requests: VecDeque::new(),
                events: VecDeque::new(),
                dropped_events: 0,
                closed: false,
            }),
            outbox_signal: Condvar::new(),
            closed: AtomicBool::new(false),
            killed: AtomicBool::new(false),
            next_id: AtomicU64::new(1),
            reader_thread: Mutex::new(None),
            writer_thread: Mutex::new(None),
        });
        process.start_reader(stdout);
        process.start_writer();
        Ok(process)
    }

    pub fn plugin_id(&self) -> &str {
        &self.plugin_id
    }

    fn start_reader(self: &Arc<Self>, stdout: std::process::ChildStdout) {
        let process = Arc::clone(self);
        let handle = std::thread::Builder::new()
            .name(format!("ukk-plugin-reader-{}", self.plugin_id))
            .spawn(move || {
                let mut reader = BufReader::new(stdout);
                loop {
                    match read_bounded_line(&mut reader, process.limits.max_frame_bytes) {
                        Ok(Some(line)) => {
                            let text = String::from_utf8_lossy(&line);
                            let trimmed = text.trim_end();
                            if trimmed.is_empty() {
                                continue;
                            }
                            match serde_json::from_str::<IncomingFrame>(trimmed) {
                                Ok(IncomingFrame::Response(response)) => {
                                    let sender = lock(&process.pending).remove(&response.id);
                                    match sender {
                                        Some(sender) => {
                                            let _ = sender.send(response);
                                        }
                                        None => {
                                            process.close(CloseReason::Protocol {
                                                detail: format!(
                                                    "unsolicited response id {}",
                                                    response.id
                                                ),
                                            });
                                            return;
                                        }
                                    }
                                }
                                Ok(IncomingFrame::Event(_)) => {
                                    // Helper-originated events have no consumer
                                    // in v1; they are dropped, never treated as
                                    // authority.
                                }
                                Err(error) => {
                                    // Log only the serde category: the raw
                                    // message embeds helper-provided fragments.
                                    process.close(CloseReason::Protocol {
                                        detail: format!(
                                            "invalid frame ({} at line {}, column {})",
                                            category_name(error.classify()),
                                            error.line(),
                                            error.column()
                                        ),
                                    });
                                    return;
                                }
                            }
                        }
                        Ok(None) => {
                            let code = lock(&process.child)
                                .try_wait()
                                .ok()
                                .flatten()
                                .and_then(|status| status.code());
                            process.close(CloseReason::Exited { code });
                            return;
                        }
                        Err(FrameReadError::FrameTooLarge) => {
                            process.close(CloseReason::FrameTooLarge {
                                limit: process.limits.max_frame_bytes,
                            });
                            return;
                        }
                        Err(FrameReadError::Io(error)) => {
                            process.close(CloseReason::Protocol {
                                detail: format!("stdout read failed: {error}"),
                            });
                            return;
                        }
                    }
                }
            });
        if let Ok(handle) = handle {
            *lock(&self.reader_thread) = Some(handle);
        }
    }

    fn start_writer(self: &Arc<Self>) {
        let process = Arc::clone(self);
        let handle = std::thread::Builder::new()
            .name(format!("ukk-plugin-writer-{}", self.plugin_id))
            .spawn(move || loop {
                let next = {
                    let mut outbox = lock(&process.outbox);
                    loop {
                        if let Some(frame) = outbox.requests.pop_front() {
                            break Some(frame);
                        }
                        if let Some(frame) = outbox.events.pop_front() {
                            break Some(frame);
                        }
                        if outbox.closed {
                            break None;
                        }
                        outbox = match process
                            .outbox_signal
                            .wait_timeout(outbox, Duration::from_millis(25))
                        {
                            Ok((guard, _)) => guard,
                            Err(poisoned) => poisoned.into_inner().0,
                        };
                    }
                };
                let Some(frame) = next else {
                    return;
                };
                let mut stdin = lock(&process.stdin);
                let Some(stdin) = stdin.as_mut() else {
                    process.close(CloseReason::WriteFailed {
                        detail: "stdin unavailable".into(),
                    });
                    return;
                };
                if let Err(error) = stdin.write_all(&frame).and_then(|_| stdin.flush()) {
                    process.close(CloseReason::WriteFailed {
                        detail: error.to_string(),
                    });
                    return;
                }
            });
        if let Ok(handle) = handle {
            *lock(&self.writer_thread) = Some(handle);
        }
    }

    /// Mark the process closed exactly once and release every waiter.
    fn close(&self, reason: CloseReason) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        *lock(&self.close_reason) = Some(reason);
        {
            let mut outbox = lock(&self.outbox);
            outbox.closed = true;
        }
        self.outbox_signal.notify_all();
        // Dropping senders wakes blocked requesters with `Disconnected`.
        lock(&self.pending).clear();
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    /// OS process id, for diagnostics and audit correlation.
    pub fn pid(&self) -> u32 {
        lock(&self.child).id()
    }

    pub fn is_alive(&self) -> bool {
        if self.is_closed() {
            return false;
        }
        matches!(lock(&self.child).try_wait(), Ok(None))
    }

    /// Kill and reap the process without the graceful `plugin.shutdown` round
    /// trip. Used on timeout and by [`Self::shutdown`] after the grace period.
    fn kill_child(&self) {
        self.killed.store(true, Ordering::SeqCst);
        let mut child = lock(&self.child);
        let _ = child.kill();
        let _ = child.wait();
    }

    /// Whether the host killed the process rather than seeing it exit.
    pub fn was_killed(&self) -> bool {
        self.killed.load(Ordering::SeqCst)
    }

    /// Immediate containment: close, kill, reap and join both I/O threads.
    ///
    /// Callers use this when a helper already violated a hard contract (for
    /// example a missed deadline). It performs no `plugin.shutdown` round trip,
    /// so it is bounded by the OS, not by the helper.
    pub fn kill_now(&self) -> ShutdownReport {
        self.close(CloseReason::Timeout);
        self.kill_child();
        self.finish_threads();
        ShutdownReport {
            killed: true,
            exit_code: self.exit_code(),
        }
    }

    fn finish_threads(&self) {
        // Dropping stdin makes a blocked writer observe EOF/EPIPE.
        let _ = lock(&self.stdin).take();
        self.outbox_signal.notify_all();
        let writer = lock(&self.writer_thread).take();
        if let Some(writer) = writer {
            let _ = writer.join();
        }
        let reader = lock(&self.reader_thread).take();
        if let Some(reader) = reader {
            let _ = reader.join();
        }
    }

    fn exit_code(&self) -> Option<i32> {
        lock(&self.child)
            .try_wait()
            .ok()
            .flatten()
            .and_then(|status| status.code())
    }

    /// Last close reason mapped to an error, for supervision and audit.
    pub fn last_error(&self) -> HelperError {
        self.close_error()
    }

    pub fn dropped_events(&self) -> u64 {
        lock(&self.outbox).dropped_events
    }

    pub fn queued_events(&self) -> usize {
        lock(&self.outbox).events.len()
    }

    fn close_error(&self) -> HelperError {
        match &*lock(&self.close_reason) {
            Some(reason) => reason.to_error(),
            None => HelperError::NotRunning,
        }
    }

    fn encode(&self, frame: &RequestFrame) -> Result<Vec<u8>, HelperError> {
        encode_json(frame, self.limits.max_frame_bytes).map_err(|error| match error {
            HelperError::FrameTooLarge { limit } => HelperError::RequestTooLarge {
                limit,
                what: "request frame",
            },
            other => other,
        })
    }

    fn round_trip(
        &self,
        frame: RequestFrame,
        method: &str,
        timeout: Duration,
    ) -> Result<ResponseFrame, HelperError> {
        if self.is_closed() {
            return Err(self.close_error());
        }
        let bytes = self.encode(&frame)?;
        let (sender, receiver) = mpsc::channel();
        lock(&self.pending).insert(frame.id, sender);
        {
            let mut outbox = lock(&self.outbox);
            if outbox.closed {
                drop(outbox);
                lock(&self.pending).remove(&frame.id);
                return Err(self.close_error());
            }
            outbox.requests.push_back(bytes);
        }
        self.outbox_signal.notify_all();

        let response = match receiver.recv_timeout(timeout) {
            Ok(response) => Some(response),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                lock(&self.pending).remove(&frame.id);
                return Err(self.close_error());
            }
        };
        lock(&self.pending).remove(&frame.id);
        let Some(response) = response else {
            // A helper that misses a deadline is broken. Close and kill it here
            // so `is_alive()` is immediately false, the reader/writer threads
            // unblock, and the supervisor can reap the handle deterministically.
            self.close(CloseReason::Timeout);
            self.kill_child();
            return Err(HelperError::Timeout {
                method: method.to_owned(),
                timeout_ms: timeout.as_millis() as u64,
            });
        };
        if response.ok {
            Ok(response)
        } else {
            let WireError { code, message } = response.error.unwrap_or(WireError {
                code: "missing_error".into(),
                message: "helper reported failure without an error object".into(),
            });
            Err(HelperError::Remote { code, message })
        }
    }

    /// Perform the hello handshake.
    pub fn hello(&self, plugin_id: &str) -> Result<HelloResult, HelperError> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let response = self.round_trip(
            RequestFrame::hello(id, plugin_id),
            "plugin.hello",
            self.limits.request_timeout,
        )?;
        let result = response.result.unwrap_or(serde_json::Value::Null);
        serde_json::from_value::<HelloResult>(result).map_err(|error| HelperError::Protocol {
            // Never log the raw serde message: it embeds helper-provided input.
            detail: format!(
                "invalid hello result ({} at line {}, column {})",
                category_name(error.classify()),
                error.line(),
                error.column()
            ),
        })
    }

    /// Liveness probe.
    pub fn health(&self) -> Result<(), HelperError> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        self.round_trip(
            RequestFrame::health(id),
            "plugin.health",
            self.limits.request_timeout,
        )
        .map(|_| ())
    }

    /// Invoke one command. The caller is responsible for authorization.
    pub fn invoke(
        &self,
        command: &str,
        session_id: Option<&str>,
        args: serde_json::Value,
    ) -> Result<serde_json::Value, HelperError> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let response = self.round_trip(
            RequestFrame::invoke(id, command, session_id, args),
            "command.invoke",
            self.limits.request_timeout,
        )?;
        Ok(response.result.unwrap_or(serde_json::Value::Null))
    }

    /// Queue one subscribed host event. Returns `Ok(false)` when the bounded
    /// queue dropped it; the host never blocks on a slow plugin.
    pub fn push_event(&self, kind: &str, payload: serde_json::Value) -> Result<bool, HelperError> {
        if self.is_closed() {
            return Err(self.close_error());
        }
        let bytes = match encode_json(
            &EventFrame::deliver(kind, payload),
            self.limits.max_frame_bytes,
        ) {
            Ok(bytes) => bytes,
            Err(_) => {
                let mut outbox = lock(&self.outbox);
                outbox.dropped_events += 1;
                return Ok(false);
            }
        };
        let queued = {
            let mut outbox = lock(&self.outbox);
            if outbox.closed {
                return Err(self.close_error());
            }
            if outbox.events.len() >= self.limits.event_queue_capacity {
                outbox.dropped_events += 1;
                false
            } else {
                outbox.events.push_back(bytes);
                true
            }
        };
        if queued {
            self.outbox_signal.notify_all();
        }
        Ok(queued)
    }

    /// Ask the helper to stop, then kill it if it outlives the grace period.
    pub fn shutdown(&self) -> ShutdownReport {
        let grace = self.limits.shutdown_grace;
        if !self.is_closed() {
            let id = self.next_id.fetch_add(1, Ordering::SeqCst);
            let _ = self.round_trip(RequestFrame::shutdown(id), "plugin.shutdown", grace);
        }
        self.wait_for_exit(grace);
        let mut killed_after_grace = false;
        {
            let mut child = lock(&self.child);
            if !matches!(child.try_wait(), Ok(Some(_))) {
                killed_after_grace = true;
                let _ = child.kill();
                let _ = child.wait();
            }
        }
        if killed_after_grace {
            self.killed.store(true, Ordering::SeqCst);
        }
        self.close(CloseReason::Stopped);
        self.finish_threads();
        ShutdownReport {
            killed: self.was_killed(),
            exit_code: self.exit_code(),
        }
    }

    fn wait_for_exit(&self, grace: Duration) {
        let deadline = Instant::now() + grace;
        loop {
            let exited = matches!(lock(&self.child).try_wait(), Ok(Some(_)));
            if exited || Instant::now() >= deadline {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for HelperProcess {
    fn drop(&mut self) {
        self.close(CloseReason::Stopped);
        let mut child = lock(&self.child);
        let _ = child.kill();
        let _ = child.wait();
    }
}

fn category_name(category: serde_json::error::Category) -> &'static str {
    match category {
        serde_json::error::Category::Io => "io",
        serde_json::error::Category::Syntax => "syntax",
        serde_json::error::Category::Data => "data",
        serde_json::error::Category::Eof => "eof",
    }
}

fn encode_json<T: serde::Serialize>(
    frame: &T,
    max_frame_bytes: usize,
) -> Result<Vec<u8>, HelperError> {
    let mut bytes = serde_json::to_vec(frame).map_err(|error| HelperError::Io {
        detail: format!("cannot encode frame: {error}"),
    })?;
    bytes.push(b'\n');
    if bytes.len() > max_frame_bytes {
        return Err(HelperError::FrameTooLarge {
            limit: max_frame_bytes,
        });
    }
    Ok(bytes)
}

#[derive(Debug)]
enum FrameReadError {
    FrameTooLarge,
    Io(std::io::Error),
}

/// Read one newline-terminated frame, refusing to buffer beyond `max` bytes.
///
/// Implemented with `fill_buf`/`consume` so a helper that writes an endless
/// line without a newline is cut off at `max` instead of exhausting memory.
fn read_bounded_line<R: BufRead>(
    reader: &mut R,
    max: usize,
) -> Result<Option<Vec<u8>>, FrameReadError> {
    let mut out = Vec::new();
    loop {
        let available = match reader.fill_buf() {
            Ok(buffer) => buffer,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(FrameReadError::Io(error)),
        };
        if available.is_empty() {
            return if out.is_empty() {
                Ok(None)
            } else {
                Ok(Some(out))
            };
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let take = newline.map(|index| index + 1).unwrap_or(available.len());
        if out.len() + take > max {
            return Err(FrameReadError::FrameTooLarge);
        }
        out.extend_from_slice(&available[..take]);
        reader.consume(take);
        if newline.is_some() {
            return Ok(Some(out));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn bounded_reader_accepts_exact_limit() {
        let mut reader = Cursor::new(b"abc\n".to_vec());
        assert_eq!(
            read_bounded_line(&mut reader, 4).unwrap(),
            Some(b"abc\n".to_vec())
        );
    }

    #[test]
    fn bounded_reader_rejects_oversize_without_newline() {
        let mut reader = Cursor::new(b"abcdefgh".to_vec());
        assert!(matches!(
            read_bounded_line(&mut reader, 4),
            Err(FrameReadError::FrameTooLarge)
        ));
    }

    #[test]
    fn bounded_reader_returns_final_partial_line() {
        let mut reader = Cursor::new(b"abc".to_vec());
        assert_eq!(
            read_bounded_line(&mut reader, 16).unwrap(),
            Some(b"abc".to_vec())
        );
        assert_eq!(read_bounded_line(&mut reader, 16).unwrap(), None);
    }

    #[test]
    fn frame_encoding_enforces_limit() {
        let frame = RequestFrame::health(1);
        assert!(encode_json(&frame, 4).is_err());
        assert!(encode_json(&frame, 4096).is_ok());
    }

    #[test]
    fn helper_command_requires_absolute_program() {
        let error = HelperProcess::spawn(
            "dev.ukk.test",
            &HelperCommand::new("not-absolute"),
            HelperLimits::default(),
        )
        .unwrap_err();
        assert_eq!(error.code(), "program_not_absolute");
    }
}
