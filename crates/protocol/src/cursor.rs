//! Durable per-stream cursors, snapshot/replay and client deduplication.
//!
//! A [`DurableCursor`] is issued only by [`EventLog`] and survives Host and
//! herdr-core restarts. Ephemeral runtime positions (`hc_seq`, `host_epoch`)
//! are never accepted here: see [`crate::reserved`].
//!
//! Ordering is guaranteed only per stream. A cursor that is ahead of the log
//! head is a protocol error, not a reason to reset; a cursor behind retained
//! history requires a fresh snapshot rather than silent event loss.

use crate::envelope::{EnvelopeError, Event, EventEnvelope, Snapshot};
use crate::id::{EventId, StreamId};
use crate::limits::{LimitError, Limits};
use crate::reserved;
use crate::version::CURRENT_GENERATION;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::VecDeque;
use std::error::Error;
use std::fmt;

/// Durable, strictly increasing position on exactly one stream.
///
/// There is intentionally no constructor from a runtime sequence: a durable
/// cursor can only come from decoding a persisted checkpoint or from the event
/// log itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DurableCursor(u64);

impl DurableCursor {
    pub const ZERO: Self = Self(0);

    #[must_use]
    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    #[must_use]
    pub fn next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }

    /// Advances a checkpoint, refusing to move backwards.
    pub fn advance(self, next: Self) -> Result<Self, CursorError> {
        if next <= self {
            return Err(CursorError::Regression {
                current: self.0,
                next: next.0,
            });
        }
        Ok(next)
    }
}

impl fmt::Display for DurableCursor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "cursor {}", self.0)
    }
}

/// Typed cursor and checkpoint failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CursorError {
    Regression { current: u64, next: u64 },
    Json(String),
    ReservedEphemeralField(&'static str),
    InvalidCheckpoint(&'static str),
}

impl fmt::Display for CursorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Regression { current, next } => {
                write!(
                    formatter,
                    "cursor would move backwards from {current} to {next}"
                )
            }
            Self::Json(message) => write!(formatter, "invalid cursor checkpoint: {message}"),
            Self::ReservedEphemeralField(field) => write!(
                formatter,
                "field '{field}' is ephemeral runtime state, not a durable cursor"
            ),
            Self::InvalidCheckpoint(reason) => {
                write!(formatter, "invalid cursor checkpoint: {reason}")
            }
        }
    }
}

impl Error for CursorError {}

/// Maximum persisted checkpoint size. Checkpoints are small metadata records.
pub const MAX_CHECKPOINT_BYTES: usize = 4 * 1024;

/// Persisted per-client, per-stream acknowledgement cursor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CursorCheckpoint {
    pub stream: StreamId,
    pub cursor: DurableCursor,
}

impl CursorCheckpoint {
    #[must_use]
    pub fn new(stream: StreamId, cursor: DurableCursor) -> Self {
        Self { stream, cursor }
    }

    pub fn advance(&mut self, next: DurableCursor) -> Result<(), CursorError> {
        self.cursor = self.cursor.advance(next)?;
        Ok(())
    }

    pub fn encode(&self) -> Result<String, CursorError> {
        serde_json::to_string(self).map_err(|error| CursorError::Json(error.to_string()))
    }

    /// Decodes a persisted checkpoint and rejects ephemeral runtime fields.
    pub fn decode(raw: &str) -> Result<Self, CursorError> {
        if raw.len() > MAX_CHECKPOINT_BYTES {
            return Err(CursorError::InvalidCheckpoint("checkpoint is too large"));
        }
        let value: Value =
            serde_json::from_str(raw).map_err(|error| CursorError::Json(error.to_string()))?;
        if let Some(field) = reserved::scan_value(&value) {
            return Err(CursorError::ReservedEphemeralField(field));
        }
        serde_json::from_value(value).map_err(|error| CursorError::Json(error.to_string()))
    }
}

/// Replay bounds advertised to a client during the handshake.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StreamBounds {
    pub stream: StreamId,
    /// Highest cursor that has been pruned. Resuming exactly at `floor` is
    /// valid when later events are still retained.
    pub floor: DurableCursor,
    pub head: DurableCursor,
}

/// Result of a resume request.
#[derive(Debug, Clone, PartialEq)]
pub enum Resume {
    /// Ordered events after the requested cursor.
    Events {
        events: Vec<EventEnvelope>,
        head: DurableCursor,
        has_more: bool,
    },
    /// Requested history is no longer retained. The Host must materialize a
    /// snapshot at `at_cursor`; the client resumes from there.
    SnapshotRequired {
        at_cursor: DurableCursor,
        bounds: StreamBounds,
    },
}

/// Typed event-log failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncError {
    Limit(LimitError),
    Envelope(EnvelopeError),
    CursorBeyondHead { cursor: u64, head: u64 },
    InvalidDurableLog(&'static str),
    StreamMismatch,
}

impl fmt::Display for SyncError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Limit(error) => error.fmt(formatter),
            Self::Envelope(error) => error.fmt(formatter),
            Self::CursorBeyondHead { cursor, head } => {
                write!(formatter, "cursor {cursor} is beyond the log head {head}")
            }
            Self::InvalidDurableLog(reason) => {
                write!(formatter, "invalid durable event log: {reason}")
            }
            Self::StreamMismatch => formatter.write_str("event belongs to a different stream"),
        }
    }
}

impl Error for SyncError {}

impl From<LimitError> for SyncError {
    fn from(error: LimitError) -> Self {
        Self::Limit(error)
    }
}

impl From<EnvelopeError> for SyncError {
    fn from(error: EnvelopeError) -> Self {
        Self::Envelope(error)
    }
}

/// Append-only durable event log for one stream with bounded retention.
#[derive(Debug)]
pub struct EventLog {
    stream: StreamId,
    head: DurableCursor,
    floor: DurableCursor,
    events: VecDeque<EventEnvelope>,
    limits: Limits,
}

impl EventLog {
    pub fn new(stream: StreamId, limits: Limits) -> Result<Self, SyncError> {
        limits.validate()?;
        Ok(Self {
            stream,
            head: DurableCursor::ZERO,
            floor: DurableCursor::ZERO,
            events: VecDeque::new(),
            limits,
        })
    }

    /// Rebuilds a log from durable records after a Host restart.
    ///
    /// `head` and `floor` come from the durable store, so the next appended
    /// event continues the cursor sequence instead of restarting at 1.
    pub fn restore(
        stream: StreamId,
        events: Vec<EventEnvelope>,
        head: DurableCursor,
        floor: DurableCursor,
        limits: Limits,
    ) -> Result<Self, SyncError> {
        limits.validate()?;
        if floor > head {
            return Err(SyncError::InvalidDurableLog(
                "retention floor is beyond the head",
            ));
        }
        if events.len() > limits.max_retained_events {
            return Err(SyncError::InvalidDurableLog(
                "retained events exceed the configured limit",
            ));
        }
        if events.is_empty() {
            if floor != head {
                return Err(SyncError::InvalidDurableLog(
                    "empty log must retain its floor at the head",
                ));
            }
        } else {
            let mut previous: Option<DurableCursor> = None;
            for event in &events {
                if event.stream != stream {
                    return Err(SyncError::InvalidDurableLog(
                        "event stream does not match the log stream",
                    ));
                }
                if let Some(previous_cursor) = previous {
                    let expected = previous_cursor
                        .next()
                        .ok_or(SyncError::InvalidDurableLog("cursor space exhausted"))?;
                    if event.cursor != expected {
                        return Err(SyncError::InvalidDurableLog(
                            "retained event cursors are not contiguous",
                        ));
                    }
                } else if event.cursor.get() <= floor.get() {
                    return Err(SyncError::InvalidDurableLog(
                        "retained events overlap the pruned range",
                    ));
                }
                previous = Some(event.cursor);
            }
            if previous != Some(head) {
                return Err(SyncError::InvalidDurableLog(
                    "last retained event does not match the head",
                ));
            }
        }
        Ok(Self {
            stream,
            head,
            floor,
            events: events.into(),
            limits,
        })
    }

    #[must_use]
    pub fn stream(&self) -> &StreamId {
        &self.stream
    }

    #[must_use]
    pub fn head(&self) -> DurableCursor {
        self.head
    }

    #[must_use]
    pub fn floor(&self) -> DurableCursor {
        self.floor
    }

    #[must_use]
    pub fn bounds(&self) -> StreamBounds {
        StreamBounds {
            stream: self.stream.clone(),
            floor: self.floor,
            head: self.head,
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.events.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Appends an event and assigns the next durable cursor.
    pub fn append(&mut self, event: Event, emitted_at_ms: i64) -> Result<EventEnvelope, SyncError> {
        self.append_with_id(EventId::new_random(), event, emitted_at_ms)
    }

    /// Appends an event with a caller-supplied id, used by deterministic tests
    /// and by durable-store replay.
    pub fn append_with_id(
        &mut self,
        event_id: EventId,
        event: Event,
        emitted_at_ms: i64,
    ) -> Result<EventEnvelope, SyncError> {
        let cursor = self
            .head
            .next()
            .ok_or(SyncError::InvalidDurableLog("cursor space exhausted"))?;
        let envelope = EventEnvelope {
            protocol: CURRENT_GENERATION,
            event_id,
            stream: self.stream.clone(),
            cursor,
            emitted_at_ms,
            event,
        };
        envelope.encode(&self.limits)?;
        self.head = cursor;
        self.events.push_back(envelope.clone());
        while self.events.len() > self.limits.max_retained_events {
            if let Some(dropped) = self.events.pop_front() {
                self.floor = dropped.cursor;
            }
        }
        Ok(envelope)
    }

    /// Replays events after `cursor`, or requires a snapshot when history was
    /// pruned.
    pub fn resume(&self, cursor: DurableCursor, max_batch: usize) -> Result<Resume, SyncError> {
        self.limits.check_replay_batch(max_batch)?;
        if cursor > self.head {
            return Err(SyncError::CursorBeyondHead {
                cursor: cursor.get(),
                head: self.head.get(),
            });
        }
        if cursor < self.floor {
            return Ok(Resume::SnapshotRequired {
                at_cursor: self.head,
                bounds: self.bounds(),
            });
        }
        let events: Vec<EventEnvelope> = self
            .events
            .iter()
            .filter(|event| event.cursor > cursor)
            .take(max_batch)
            .cloned()
            .collect();
        let has_more = events.last().is_some_and(|event| event.cursor < self.head);
        Ok(Resume::Events {
            events,
            head: self.head,
            has_more,
        })
    }

    /// Materializes a snapshot at the current head. The Host supplies the
    /// durable domain body; the protocol owns the cursor binding.
    #[must_use]
    pub fn snapshot(&self, revision: u64, body: Value) -> Snapshot {
        Snapshot::new(self.stream.clone(), self.head, revision, body)
    }
}

/// How a client applied a delivered event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyOutcome {
    Applied {
        cursor: DurableCursor,
    },
    /// Already applied. This is the normal outcome after a dropped ACK causes
    /// the server to replay from an older cursor.
    Duplicate {
        cursor: DurableCursor,
    },
}

/// Typed client inbox failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InboxError {
    StreamMismatch,
    OutOfOrder {
        expected: DurableCursor,
        received: DurableCursor,
    },
    /// The same event id appeared at a different cursor. That is a protocol or
    /// storage fault, not a duplicate delivery.
    InconsistentReplay {
        cursor: DurableCursor,
    },
    InvalidCapacity,
}

impl fmt::Display for InboxError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StreamMismatch => formatter.write_str("event belongs to a different stream"),
            Self::OutOfOrder { expected, received } => {
                write!(formatter, "expected {expected}, received {received}")
            }
            Self::InconsistentReplay { cursor } => write!(
                formatter,
                "event id repeated at a different cursor ({cursor})"
            ),
            Self::InvalidCapacity => formatter.write_str("inbox capacity must be at least one"),
        }
    }
}

impl Error for InboxError {}

/// Client-side deduplicating inbox for at-least-once delivery.
///
/// Per-stream cursors are contiguous, so one applied cursor plus a bounded
/// window of recent event ids is enough to deduplicate without unbounded
/// memory.
#[derive(Debug)]
pub struct EventInbox {
    stream: StreamId,
    applied: DurableCursor,
    recent: VecDeque<(EventId, DurableCursor)>,
    capacity: usize,
}

impl EventInbox {
    pub fn new(stream: StreamId, capacity: usize) -> Result<Self, InboxError> {
        if capacity == 0 {
            return Err(InboxError::InvalidCapacity);
        }
        Ok(Self {
            stream,
            applied: DurableCursor::ZERO,
            recent: VecDeque::with_capacity(capacity.min(1024)),
            capacity,
        })
    }

    #[must_use]
    pub fn applied(&self) -> DurableCursor {
        self.applied
    }

    pub fn apply(&mut self, envelope: &EventEnvelope) -> Result<ApplyOutcome, InboxError> {
        if envelope.stream != self.stream {
            return Err(InboxError::StreamMismatch);
        }
        if let Some((_, seen_cursor)) = self
            .recent
            .iter()
            .find(|(event_id, _)| event_id == &envelope.event_id)
        {
            if *seen_cursor != envelope.cursor {
                return Err(InboxError::InconsistentReplay {
                    cursor: envelope.cursor,
                });
            }
            return Ok(ApplyOutcome::Duplicate {
                cursor: envelope.cursor,
            });
        }
        if envelope.cursor <= self.applied {
            return Ok(ApplyOutcome::Duplicate {
                cursor: envelope.cursor,
            });
        }
        let expected = self.applied.next().unwrap_or(self.applied);
        if envelope.cursor != expected {
            return Err(InboxError::OutOfOrder {
                expected,
                received: envelope.cursor,
            });
        }
        self.applied = envelope.cursor;
        self.recent
            .push_back((envelope.event_id.clone(), envelope.cursor));
        while self.recent.len() > self.capacity {
            self.recent.pop_front();
        }
        Ok(ApplyOutcome::Applied {
            cursor: envelope.cursor,
        })
    }
}
