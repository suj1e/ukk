//! Event notifications and sequence-gap detection.
//!
//! `events.subscribe` is the only stream with sequence numbers: attach
//! streams are ordered byte streams with no per-frame sequence in protocol
//! generation 1. Gaps can therefore only be detected on the event stream.
//! Filtered subscriptions legitimately skip numbers because other terminals'
//! events still advance `seq`; only an unfiltered subscription treats a jump
//! as a gap requiring a fresh snapshot.

use crate::error::{HcError, Result};
use crate::types::{Foreground, Status, Terminal, TerminalAddress, TerminalReason};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq)]
pub struct LostData {
    pub reason: Option<TerminalReason>,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub finished_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FinishedData {
    pub reason: Option<TerminalReason>,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub finished_ms: Option<u64>,
}

impl FinishedData {
    fn from_value(data: &Value) -> Self {
        Self {
            reason: data
                .get("reason")
                .and_then(Value::as_str)
                .map(|raw| serde_json::from_value(Value::String(raw.into())))
                .transpose()
                .ok()
                .flatten(),
            exit_code: data
                .get("exit_code")
                .and_then(Value::as_i64)
                .map(|v| v as i32),
            signal: data.get("signal").and_then(Value::as_i64).map(|v| v as i32),
            finished_ms: data.get("finished_ms").and_then(Value::as_u64),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct StatusData {
    pub status: Option<Status>,
    pub records: BTreeMap<String, Status>,
    pub reported: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LabelsData {
    pub labels: BTreeMap<String, String>,
    pub record: Option<Terminal>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum EventPayload {
    Created(Terminal),
    Lost(LostData),
    Restarted(Terminal),
    Keep {
        keep: bool,
    },
    Finished(FinishedData),
    Removed,
    Status(StatusData),
    Turn {
        turn: u64,
        state: String,
    },
    Title(String),
    Labels(LabelsData),
    Size {
        cols: u16,
        rows: u16,
    },
    Foreground(Option<Foreground>),
    Doc {
        name: String,
        version: u64,
    },
    /// A type this client does not model yet. The raw data is retained so a
    /// caller can log it; it is never interpreted as a control signal.
    Unknown {
        kind: String,
        data: Value,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct EventNotification {
    pub seq: u64,
    pub terminal: Option<TerminalAddress>,
    pub machine: Option<String>,
    pub payload: EventPayload,
}

impl EventNotification {
    pub fn from_params(params: &Value) -> Result<Self> {
        let seq = params
            .get("seq")
            .and_then(Value::as_u64)
            .ok_or_else(|| HcError::Protocol("event missing seq".into()))?;
        let kind = params
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| HcError::Protocol("event missing type".into()))?;
        let terminal = params
            .get("terminal")
            .and_then(Value::as_str)
            .map(TerminalAddress::parse)
            .transpose()?;
        let machine = params
            .get("machine")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let data = params.get("data").cloned().unwrap_or(Value::Null);
        let payload = match kind {
            "created" | "restarted" => {
                let terminal = Terminal::from_value(data)?;
                if kind == "created" {
                    EventPayload::Created(terminal)
                } else {
                    EventPayload::Restarted(terminal)
                }
            }
            "lost" => EventPayload::Lost(LostData {
                reason: FinishedData::from_value(&data).reason,
                exit_code: FinishedData::from_value(&data).exit_code,
                signal: FinishedData::from_value(&data).signal,
                finished_ms: FinishedData::from_value(&data).finished_ms,
            }),
            "finished" => EventPayload::Finished(FinishedData::from_value(&data)),
            "keep" => EventPayload::Keep {
                keep: data.get("keep").and_then(Value::as_bool).unwrap_or(false),
            },
            "removed" => EventPayload::Removed,
            "status" => EventPayload::Status(StatusData {
                status: data
                    .get("status")
                    .filter(|value| !value.is_null())
                    .cloned()
                    .map(serde_json::from_value)
                    .transpose()?,
                records: data
                    .get("records")
                    .cloned()
                    .map(serde_json::from_value)
                    .transpose()?
                    .unwrap_or_default(),
                reported: data
                    .get("reported")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            }),
            "turn" => EventPayload::Turn {
                turn: data.get("turn").and_then(Value::as_u64).unwrap_or(0),
                state: data
                    .get("state")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            },
            "title" => EventPayload::Title(
                data.get("title")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            ),
            "labels" => EventPayload::Labels(LabelsData {
                labels: data
                    .get("labels")
                    .cloned()
                    .map(serde_json::from_value)
                    .transpose()?
                    .unwrap_or_default(),
                record: data
                    .get("record")
                    .filter(|value| !value.is_null())
                    .cloned()
                    .map(Terminal::from_value)
                    .transpose()?,
            }),
            "size" => EventPayload::Size {
                cols: data.get("cols").and_then(Value::as_u64).unwrap_or(0) as u16,
                rows: data.get("rows").and_then(Value::as_u64).unwrap_or(0) as u16,
            },
            "foreground" => EventPayload::Foreground(if data.is_null() {
                None
            } else {
                Some(serde_json::from_value(data.clone())?)
            }),
            "doc" => EventPayload::Doc {
                name: data
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                version: data.get("version").and_then(Value::as_u64).unwrap_or(0),
            },
            other => EventPayload::Unknown {
                kind: other.to_owned(),
                data,
            },
        };
        Ok(Self {
            seq,
            terminal,
            machine,
            payload,
        })
    }
}

/// What a `missed` notification says: drop events at or below `seq`, then
/// take the embedded authoritative snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct MissedNotification {
    pub seq: u64,
    pub terminals: Vec<Terminal>,
}

impl MissedNotification {
    pub fn from_params(params: &Value) -> Result<Self> {
        let seq = params
            .get("seq")
            .and_then(Value::as_u64)
            .ok_or_else(|| HcError::Protocol("missed notification missing seq".into()))?;
        let terminals = params
            .get("terminals")
            .cloned()
            .map(serde_json::from_value)
            .transpose()?
            .unwrap_or_default();
        Ok(Self { seq, terminals })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum StreamNotice {
    Event(Box<EventNotification>),
    Missed(MissedNotification),
    Heartbeat { seq: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorStep {
    Deliver,
    Stale,
    Gap { expected: u64, got: u64 },
}

/// Tracks `seq` continuity for one subscription.
#[derive(Debug, Clone)]
pub struct EventCursor {
    last: u64,
    allow_gaps: bool,
}

impl EventCursor {
    pub fn new(seq: u64, allow_gaps: bool) -> Self {
        Self {
            last: seq,
            allow_gaps,
        }
    }

    pub fn last(&self) -> u64 {
        self.last
    }

    /// A snapshot (initial or after `missed`) is authoritative: reset to its
    /// sequence, including after a core restart where `seq` goes backwards.
    pub fn reset(&mut self, seq: u64) {
        self.last = seq;
    }

    pub fn observe_event(&mut self, seq: u64) -> CursorStep {
        if seq <= self.last {
            return CursorStep::Stale;
        }
        if self.allow_gaps {
            self.last = seq;
            return CursorStep::Deliver;
        }
        let expected = self.last.saturating_add(1);
        if seq == expected {
            self.last = seq;
            CursorStep::Deliver
        } else {
            // Advance past the gap so a caller that takes a fresh snapshot
            // and continues is not stuck; the gap itself is reported.
            self.last = seq;
            CursorStep::Gap { expected, got: seq }
        }
    }

    pub fn observe_heartbeat(&mut self, seq: u64) -> CursorStep {
        if seq <= self.last {
            return CursorStep::Stale;
        }
        if self.allow_gaps {
            self.last = seq;
            return CursorStep::Deliver;
        }
        let expected = self.last.saturating_add(1);
        self.last = seq;
        CursorStep::Gap { expected, got: seq }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn detects_unfiltered_gap_and_resets_after_missed() {
        let mut cursor = EventCursor::new(10, false);
        assert_eq!(cursor.observe_event(11), CursorStep::Deliver);
        assert_eq!(cursor.observe_event(11), CursorStep::Stale);
        assert_eq!(
            cursor.observe_event(14),
            CursorStep::Gap {
                expected: 12,
                got: 14
            }
        );
        cursor.reset(20);
        assert_eq!(cursor.observe_event(21), CursorStep::Deliver);
    }

    #[test]
    fn filtered_subscriptions_allow_skipped_sequence_numbers() {
        let mut cursor = EventCursor::new(10, true);
        assert_eq!(cursor.observe_event(15), CursorStep::Deliver);
        assert_eq!(cursor.observe_event(15), CursorStep::Stale);
        assert_eq!(cursor.observe_heartbeat(20), CursorStep::Deliver);
    }

    #[test]
    fn reconnection_with_lower_snapshot_does_not_report_false_gap() {
        let mut old = EventCursor::new(50, false);
        old.observe_event(51);
        let mut fresh = EventCursor::new(2, false);
        assert_eq!(fresh.observe_event(3), CursorStep::Deliver);
    }

    #[test]
    fn parses_typed_event_payloads() {
        let params = json!({
            "seq": 11, "type": "created", "terminal": "t3",
            "data": {"address":"t3","argv":["codex"],"cwd":"/w","state":"live",
                     "created_ms":1,"screen":"exact","cols":80,"rows":24}
        });
        let event = EventNotification::from_params(&params).unwrap();
        assert!(matches!(event.payload, EventPayload::Created(_)));

        let finish = json!({
            "seq": 12, "type": "finished", "terminal": "t3",
            "data": {"reason":"exited","exit_code":0,"signal":null,"finished_ms":9}
        });
        let event = EventNotification::from_params(&finish).unwrap();
        assert!(matches!(
            event.payload,
            EventPayload::Finished(FinishedData {
                exit_code: Some(0),
                ..
            })
        ));
    }
}
