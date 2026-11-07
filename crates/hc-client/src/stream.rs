//! Bounded stream queues, attachments and event subscriptions.
//!
//! Backpressure is explicit. herdr-core grants a new attach stream 256 KiB of
//! credit and only sends more after the viewer returns CREDIT for bytes it
//! consumed. [`Attachment::read_chunk`] issues that credit automatically;
//! [`Attachment::next_event`] lets a caller inspect RESIZE/FOCUS first and
//! decide when to credit. A consumer that never credits stalls after the
//! initial window instead of growing memory without bound, and the local
//! queue enforces a hard cap as a second line of defence.

use crate::error::{HcError, Result};
use crate::events::{CursorStep, EventCursor, EventNotification, MissedNotification, StreamNotice};
use crate::frame::{Frame, FrameWriter};
use crate::types::{Display, SubscribeResult, Terminal};
use serde_json::Value;
use std::collections::VecDeque;
use std::fmt;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

/// A view channel's local buffer cap. Above this, the queue is discarded and
/// the consumer receives [`HcError::Backpressure`] rather than unbounded RAM.
pub const MAX_STREAM_BUFFER_BYTES: usize = 1024 * 1024;
/// A hard cap on queued control frames.
pub const MAX_STREAM_BUFFER_FRAMES: usize = 4096;
/// Largest single human/automatic input chunk accepted by this client.
pub const MAX_INPUT_CHUNK: usize = 256 * 1024;

/// The connection removes a channel from its routing table when a view lets
/// go of it. This never stops the program: it only ends the stream.
pub trait ChannelLifecycle: Send + Sync {
    fn detach(&self, channel: u32);
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamClose {
    pub reason: Option<String>,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
}

impl StreamClose {
    pub fn from_payload(payload: &[u8]) -> Self {
        if payload.is_empty() {
            return Self {
                reason: None,
                exit_code: None,
                signal: None,
            };
        }
        let value: Value = serde_json::from_slice(payload).unwrap_or(Value::Null);
        Self {
            reason: value
                .get("reason")
                .and_then(Value::as_str)
                .map(str::to_owned),
            exit_code: value
                .get("exit_code")
                .and_then(Value::as_i64)
                .map(|value| value as i32),
            signal: value
                .get("signal")
                .and_then(Value::as_i64)
                .map(|value| value as i32),
        }
    }

    pub fn is_program_exit(&self) -> bool {
        self.reason.is_some()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    Data(Vec<u8>),
    Resize { cols: u16, rows: u16 },
    Focus { gained: bool },
    Credit { bytes: u32 },
    Facts(Vec<u8>),
    Close(StreamClose),
}

struct StreamState {
    events: VecDeque<StreamEvent>,
    bytes: usize,
    closed: Option<String>,
    overflow: Option<usize>,
}

pub struct StreamQueue {
    state: Mutex<StreamState>,
    ready: Condvar,
    max_bytes: usize,
}

impl StreamQueue {
    pub fn new(max_bytes: usize) -> Self {
        Self {
            state: Mutex::new(StreamState {
                events: VecDeque::new(),
                bytes: 0,
                closed: None,
                overflow: None,
            }),
            ready: Condvar::new(),
            max_bytes,
        }
    }

    pub(crate) fn push(&self, frame: Frame) {
        let event = match frame {
            Frame::Data { payload, .. } | Frame::AutomaticInput { payload, .. } => {
                StreamEvent::Data(payload)
            }
            Frame::Facts { payload, .. } => StreamEvent::Facts(payload),
            Frame::Resize { cols, rows, .. } => StreamEvent::Resize { cols, rows },
            Frame::Focus { gained, .. } => StreamEvent::Focus { gained },
            Frame::Credit { bytes, .. } => StreamEvent::Credit { bytes },
            Frame::Close { payload, .. } => StreamEvent::Close(StreamClose::from_payload(&payload)),
            Frame::Unknown { .. } => return,
        };
        let cost = match &event {
            StreamEvent::Data(payload) | StreamEvent::Facts(payload) => payload.len(),
            _ => 0,
        };
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.closed.is_some() {
            return;
        }
        if state.bytes.saturating_add(cost) > self.max_bytes
            || state.events.len() >= MAX_STREAM_BUFFER_FRAMES
        {
            state.events.clear();
            state.bytes = 0;
            state.overflow = Some(self.max_bytes);
            state.closed = Some("stream consumer fell too far behind".into());
            self.ready.notify_all();
            return;
        }
        state.bytes += cost;
        let is_close = matches!(event, StreamEvent::Close(_));
        state.events.push_back(event);
        if is_close {
            state.closed = Some("core closed the stream".into());
        }
        self.ready.notify_all();
    }

    /// The connection is gone. Queued data remains readable, then the caller
    /// sees a typed `Closed`.
    pub(crate) fn disconnect(&self, reason: impl Into<String>) {
        self.finish(Some(reason.into()));
    }

    fn finish(&self, reason: Option<String>) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.closed.is_none() {
            state.closed = reason;
        }
        self.ready.notify_all();
    }

    pub fn next_event(&self, timeout: Duration) -> Result<StreamEvent> {
        let deadline = std::time::Instant::now() + timeout;
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        loop {
            if let Some(limit) = state.overflow.take() {
                return Err(HcError::Backpressure { limit });
            }
            if let Some(event) = state.events.pop_front() {
                if let StreamEvent::Data(payload) | StreamEvent::Facts(payload) = &event {
                    state.bytes = state.bytes.saturating_sub(payload.len());
                }
                return Ok(event);
            }
            if let Some(reason) = &state.closed {
                return Err(HcError::Closed(reason.clone()));
            }
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return Err(HcError::Timeout {
                    operation: "attach stream".into(),
                    seconds: timeout.as_secs(),
                });
            }
            state = self
                .ready
                .wait_timeout(state, remaining)
                .unwrap_or_else(|error| error.into_inner())
                .0;
        }
    }

    pub fn buffered_bytes(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .bytes
    }
}

/// A live attachment. Dropping or detaching it sends CLOSE for the stream;
/// it never sends `terminal.kill`, so closing a view cannot stop a program.
pub struct Attachment {
    channel: u32,
    terminal: Terminal,
    observe: bool,
    automatic_input: bool,
    resize_notifications: bool,
    queue: Arc<StreamQueue>,
    writer: Arc<FrameWriter>,
    lifecycle: Arc<dyn ChannelLifecycle>,
    detached: bool,
}

impl Attachment {
    pub(crate) fn new(
        result: crate::types::AttachResult,
        observe: bool,
        queue: Arc<StreamQueue>,
        writer: Arc<FrameWriter>,
        lifecycle: Arc<dyn ChannelLifecycle>,
    ) -> Self {
        Self {
            channel: result.channel,
            terminal: result.terminal,
            observe,
            automatic_input: result.automatic_input,
            resize_notifications: result.resize_notifications,
            queue,
            writer,
            lifecycle,
            detached: false,
        }
    }

    pub fn channel(&self) -> u32 {
        self.channel
    }

    pub fn terminal(&self) -> &Terminal {
        &self.terminal
    }

    pub fn automatic_input(&self) -> bool {
        self.automatic_input
    }

    pub fn resize_notifications(&self) -> bool {
        self.resize_notifications
    }

    pub fn is_observer(&self) -> bool {
        self.observe
    }

    pub fn buffered_bytes(&self) -> usize {
        self.queue.buffered_bytes()
    }

    pub fn next_event(&self, timeout: Duration) -> Result<StreamEvent> {
        self.queue.next_event(timeout)
    }

    /// Read the next program bytes and return credit for them. Control frames
    /// are skipped; use [`Attachment::next_event`] when they matter.
    pub fn read_chunk(&self, timeout: Duration) -> Result<Vec<u8>> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            match self.queue.next_event(remaining)? {
                StreamEvent::Data(bytes) => {
                    self.grant_credit(bytes.len())?;
                    return Ok(bytes);
                }
                StreamEvent::Close(close) => {
                    return Err(HcError::Closed(format!(
                        "program stream ended: {}",
                        close.reason.unwrap_or_else(|| "closed".into())
                    )))
                }
                _ => continue,
            }
        }
    }

    /// Send human bytes (keys/paste) as DATA. Observers cannot type.
    pub fn send_input(&self, bytes: &[u8]) -> Result<()> {
        if bytes.len() > MAX_INPUT_CHUNK {
            return Err(HcError::InvalidArgument(format!(
                "input chunk {} exceeds {MAX_INPUT_CHUNK} bytes",
                bytes.len()
            )));
        }
        if self.observe {
            return Err(HcError::Unsupported {
                method: "terminal.attach DATA".into(),
                reason: "this attachment was opened as observe-only".into(),
            });
        }
        self.writer.send(&Frame::Data {
            channel: self.channel,
            payload: bytes.to_vec(),
        })
    }

    /// Send terminal-generated bytes as AUTOMATIC_INPUT. Requires both the
    /// core's negotiated feature and the attach confirmation.
    pub fn send_automatic_input(&self, bytes: &[u8]) -> Result<()> {
        if bytes.len() > MAX_INPUT_CHUNK {
            return Err(HcError::InvalidArgument(format!(
                "input chunk {} exceeds {MAX_INPUT_CHUNK} bytes",
                bytes.len()
            )));
        }
        if !self.automatic_input {
            return Err(HcError::Unsupported {
                method: "AUTOMATIC_INPUT".into(),
                reason: "the attach path did not confirm automatic_input".into(),
            });
        }
        if self.observe {
            return Err(HcError::Unsupported {
                method: "AUTOMATIC_INPUT".into(),
                reason: "observe-only attachments never send input".into(),
            });
        }
        self.writer.send(&Frame::AutomaticInput {
            channel: self.channel,
            payload: bytes.to_vec(),
        })
    }

    pub fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        if self.observe {
            return Err(HcError::Unsupported {
                method: "RESIZE".into(),
                reason: "observe-only attachments never resize".into(),
            });
        }
        self.writer.send(&Frame::Resize {
            channel: self.channel,
            cols,
            rows,
        })
    }

    pub fn focus(&self, gained: bool) -> Result<()> {
        self.writer.send(&Frame::Focus {
            channel: self.channel,
            gained,
        })
    }

    pub fn send_facts(&self, display: &Display) -> Result<()> {
        let payload = serde_json::to_vec(display)?;
        self.writer.send(&Frame::Facts {
            channel: self.channel,
            payload,
        })
    }

    /// Return flow-control credit for bytes the consumer wrote out.
    pub fn grant_credit(&self, bytes: usize) -> Result<()> {
        let bytes = u32::try_from(bytes)
            .map_err(|_| HcError::InvalidArgument("credit chunk exceeds u32".into()))?;
        if bytes == 0 {
            return Ok(());
        }
        self.writer.send(&Frame::Credit {
            channel: self.channel,
            bytes,
        })
    }

    /// End this view's stream. This is explicitly not a program stop.
    pub fn detach(mut self) -> Result<()> {
        self.close_stream()
    }

    fn close_stream(&mut self) -> Result<()> {
        if self.detached {
            return Ok(());
        }
        self.detached = true;
        self.lifecycle.detach(self.channel);
        self.writer.send(&Frame::Close {
            channel: self.channel,
            payload: Vec::new(),
        })
    }
}

impl fmt::Debug for Attachment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Attachment")
            .field("channel", &self.channel)
            .field("terminal", &self.terminal.address)
            .field("observe", &self.observe)
            .field("automatic_input", &self.automatic_input)
            .finish()
    }
}

impl Drop for Attachment {
    fn drop(&mut self) {
        let _ = self.close_stream();
    }
}

/// Raw event payload queue with the same bounded behaviour as a stream.
struct EventState {
    payloads: VecDeque<Vec<u8>>,
    bytes: usize,
    closed: Option<String>,
    overflow: Option<usize>,
}

pub struct EventQueue {
    state: Mutex<EventState>,
    ready: Condvar,
    max_bytes: usize,
}

impl EventQueue {
    pub fn new(max_bytes: usize) -> Self {
        Self {
            state: Mutex::new(EventState {
                payloads: VecDeque::new(),
                bytes: 0,
                closed: None,
                overflow: None,
            }),
            ready: Condvar::new(),
            max_bytes,
        }
    }

    pub(crate) fn push_payload(&self, payload: Vec<u8>) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.closed.is_some() {
            return;
        }
        if state.bytes.saturating_add(payload.len()) > self.max_bytes
            || state.payloads.len() >= MAX_STREAM_BUFFER_FRAMES
        {
            state.payloads.clear();
            state.bytes = 0;
            state.overflow = Some(self.max_bytes);
            state.closed = Some("event consumer fell too far behind".into());
            self.ready.notify_all();
            return;
        }
        state.bytes += payload.len();
        state.payloads.push_back(payload);
        self.ready.notify_all();
    }

    pub(crate) fn disconnect(&self, reason: impl Into<String>) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.closed.is_none() {
            state.closed = Some(reason.into());
        }
        self.ready.notify_all();
    }

    fn next_payload(&self, timeout: Duration) -> Result<Vec<u8>> {
        let deadline = std::time::Instant::now() + timeout;
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        loop {
            if let Some(limit) = state.overflow.take() {
                return Err(HcError::Backpressure { limit });
            }
            if let Some(payload) = state.payloads.pop_front() {
                state.bytes = state.bytes.saturating_sub(payload.len());
                return Ok(payload);
            }
            if let Some(reason) = &state.closed {
                return Err(HcError::Closed(reason.clone()));
            }
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return Err(HcError::Timeout {
                    operation: "event subscription".into(),
                    seconds: timeout.as_secs(),
                });
            }
            state = self
                .ready
                .wait_timeout(state, remaining)
                .unwrap_or_else(|error| error.into_inner())
                .0;
        }
    }
}

/// A live `events.subscribe` channel.
pub struct EventSubscription {
    channel: u32,
    snapshot_seq: u64,
    terminals: Vec<Terminal>,
    queue: Arc<EventQueue>,
    writer: Arc<FrameWriter>,
    lifecycle: Arc<dyn ChannelLifecycle>,
    cursor: Mutex<EventCursor>,
    detached: bool,
}

impl EventSubscription {
    pub(crate) fn new(
        result: SubscribeResult,
        allow_gaps: bool,
        queue: Arc<EventQueue>,
        writer: Arc<FrameWriter>,
        lifecycle: Arc<dyn ChannelLifecycle>,
        fallback_channel: u32,
    ) -> Result<Self> {
        Ok(Self {
            channel: result.channel.unwrap_or(fallback_channel),
            snapshot_seq: result.seq,
            terminals: result.terminals,
            queue,
            writer,
            lifecycle,
            cursor: Mutex::new(EventCursor::new(result.seq, allow_gaps)),
            detached: false,
        })
    }

    pub fn channel(&self) -> u32 {
        self.channel
    }

    pub fn snapshot_seq(&self) -> u64 {
        self.snapshot_seq
    }

    pub fn snapshot_terminals(&self) -> &[Terminal] {
        &self.terminals
    }

    /// Wait for the next notice. Credit for the consumed payload is returned
    /// automatically, blockingly with the payload.
    pub fn next_notice(&self, timeout: Duration) -> Result<StreamNotice> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            let payload = self.queue.next_payload(remaining)?;
            self.grant_credit(payload.len())?;
            let value: Value = serde_json::from_slice(&payload)?;
            let method = value
                .get("method")
                .and_then(Value::as_str)
                .ok_or_else(|| HcError::Protocol("event payload is not a notification".into()))?;
            let params = value.get("params").cloned().unwrap_or(Value::Null);
            let mut cursor = self
                .cursor
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            match method {
                "event" => {
                    let event = EventNotification::from_params(&params)?;
                    match cursor.observe_event(event.seq) {
                        CursorStep::Deliver => return Ok(StreamNotice::Event(Box::new(event))),
                        CursorStep::Stale => continue,
                        CursorStep::Gap { expected, got } => {
                            return Err(HcError::SequenceGap { expected, got })
                        }
                    }
                }
                "missed" => {
                    let missed = MissedNotification::from_params(&params)?;
                    cursor.reset(missed.seq);
                    return Ok(StreamNotice::Missed(missed));
                }
                "heartbeat" => {
                    let seq = params
                        .get("seq")
                        .and_then(Value::as_u64)
                        .ok_or_else(|| HcError::Protocol("heartbeat missing seq".into()))?;
                    match cursor.observe_heartbeat(seq) {
                        CursorStep::Gap { expected, got } => {
                            return Err(HcError::SequenceGap { expected, got })
                        }
                        _ => return Ok(StreamNotice::Heartbeat { seq }),
                    }
                }
                other => {
                    return Err(HcError::Protocol(format!(
                        "unexpected event subscription method {other:?}"
                    )))
                }
            }
        }
    }

    fn grant_credit(&self, bytes: usize) -> Result<()> {
        let bytes = u32::try_from(bytes)
            .map_err(|_| HcError::InvalidArgument("credit chunk exceeds u32".into()))?;
        if bytes == 0 {
            return Ok(());
        }
        self.writer.send(&Frame::Credit {
            channel: self.channel,
            bytes,
        })
    }

    pub fn detach(mut self) -> Result<()> {
        self.close_stream()
    }

    fn close_stream(&mut self) -> Result<()> {
        if self.detached {
            return Ok(());
        }
        self.detached = true;
        self.lifecycle.detach(self.channel);
        self.writer.send(&Frame::Close {
            channel: self.channel,
            payload: Vec::new(),
        })
    }
}

impl fmt::Debug for EventSubscription {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EventSubscription")
            .field("channel", &self.channel)
            .field("snapshot_seq", &self.snapshot_seq)
            .finish()
    }
}

impl Drop for EventSubscription {
    fn drop(&mut self) {
        let _ = self.close_stream();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_queue_reports_backpressure_instead_of_growing() {
        let queue = StreamQueue::new(8);
        queue.push(Frame::Data {
            channel: 1,
            payload: vec![0; 5],
        });
        assert_eq!(queue.buffered_bytes(), 5);
        queue.push(Frame::Data {
            channel: 1,
            payload: vec![0; 5],
        });
        assert_eq!(queue.buffered_bytes(), 0);
        assert!(matches!(
            queue.next_event(Duration::from_millis(1)),
            Err(HcError::Backpressure { limit: 8 })
        ));
    }

    #[test]
    fn disconnect_keeps_queued_data_then_reports_closed() {
        let queue = StreamQueue::new(1024);
        queue.push(Frame::Data {
            channel: 1,
            payload: b"abc".to_vec(),
        });
        queue.disconnect("test");
        assert_eq!(
            queue.next_event(Duration::from_millis(1)).unwrap(),
            StreamEvent::Data(b"abc".to_vec())
        );
        assert!(matches!(
            queue.next_event(Duration::from_millis(1)),
            Err(HcError::Closed(_))
        ));
    }

    #[test]
    fn close_payload_is_typed() {
        let queue = StreamQueue::new(1024);
        queue.push(Frame::Close {
            channel: 1,
            payload: br#"{"reason":"exited","exit_code":0,"signal":null}"#.to_vec(),
        });
        assert_eq!(
            queue.next_event(Duration::from_millis(1)).unwrap(),
            StreamEvent::Close(StreamClose {
                reason: Some("exited".into()),
                exit_code: Some(0),
                signal: None
            })
        );
    }

    #[test]
    fn event_queue_gap_is_possible_to_detect() {
        let queue = EventQueue::new(1024);
        queue.push_payload(
            serde_json::to_vec(&serde_json::json!({
                "method":"event","params":{"seq":11,"type":"title",
                "terminal":"t1","data":{"title":"x"}}
            }))
            .unwrap(),
        );
        let writer = Arc::new(FrameWriter::new(Vec::new()));
        struct Noop;
        impl ChannelLifecycle for Noop {
            fn detach(&self, _channel: u32) {}
        }
        let subscription = EventSubscription::new(
            SubscribeResult {
                seq: 10,
                terminals: Vec::new(),
                channel: Some(2),
            },
            false,
            Arc::new(queue),
            writer,
            Arc::new(Noop),
            0,
        )
        .unwrap();
        assert!(matches!(
            subscription
                .next_notice(Duration::from_millis(1))
                .unwrap(),
            StreamNotice::Event(event) if event.seq == 11
        ));
    }
}
