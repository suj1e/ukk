//! Bounded projection of typed omp events into a Host conversation timeline.
//!
//! Only documented RPC fields are rendered. Nothing is inferred from terminal
//! text, approvals are never synthesized, and assistant text comes from
//! streaming deltas (not from `message_start`/`message_end` snapshots, which
//! would duplicate it).

use std::collections::{HashMap, HashSet};
use ukk_omp_client::{redact, Event, EventKind, OmpTimelineEntry, OmpTimelineKind};

use super::runtime_auth::now_ms;

/// Maximum entries retained. Older entries are dropped and `truncated` is
/// reported in the snapshot.
pub(crate) const MAX_ENTRIES: usize = 400;
/// Maximum bytes for one rendered entry.
pub(crate) const MAX_ENTRY_BYTES: usize = 8 * 1024;
/// Maximum bytes for the whole retained timeline.
pub(crate) const MAX_TOTAL_BYTES: usize = 512 * 1024;

pub(crate) struct Timeline {
    entries: Vec<OmpTimelineEntry>,
    next_seq: u64,
    truncated: bool,
    total_bytes: usize,
    current_assistant: Option<u64>,
    current_thinking: Option<u64>,
    tools: HashMap<String, u64>,
    /// Per-turn dedupe for repeated fire-and-forget UI updates.
    suppressed: HashSet<String>,
}

impl Timeline {
    pub(crate) fn new() -> Self {
        Self {
            entries: Vec::new(),
            next_seq: 1,
            truncated: false,
            total_bytes: 0,
            current_assistant: None,
            current_thinking: None,
            tools: HashMap::new(),
            suppressed: HashSet::new(),
        }
    }

    pub(crate) fn entries(&self) -> &[OmpTimelineEntry] {
        &self.entries
    }

    pub(crate) fn truncated(&self) -> bool {
        self.truncated
    }

    /// Called when a new turn starts so repeated UI updates are not spammed.
    pub(crate) fn begin_turn(&mut self) {
        self.current_assistant = None;
        self.current_thinking = None;
        self.suppressed.clear();
    }

    pub(crate) fn push(
        &mut self,
        kind: OmpTimelineKind,
        text: impl Into<String>,
        tool_name: Option<String>,
        tool_call_id: Option<String>,
        is_error: bool,
    ) -> u64 {
        let (text, truncated) = bound(text.into(), MAX_ENTRY_BYTES);
        let seq = self.next_seq;
        self.next_seq = self.next_seq.saturating_add(1);
        self.total_bytes = self.total_bytes.saturating_add(text.len());
        self.entries.push(OmpTimelineEntry {
            seq,
            at_ms: now_ms(),
            kind,
            text,
            tool_name,
            tool_call_id,
            is_error,
            truncated,
        });
        self.prune();
        seq
    }

    fn append(&mut self, seq: u64, delta: &str) {
        let Some(entry) = self.entries.iter_mut().find(|entry| entry.seq == seq) else {
            return;
        };
        if entry.text.len() >= MAX_ENTRY_BYTES {
            entry.truncated = true;
            return;
        }
        let mut remaining = MAX_ENTRY_BYTES.saturating_sub(entry.text.len());
        let mut taken = String::new();
        for character in delta.chars() {
            let size = character.len_utf8();
            if size > remaining {
                entry.truncated = true;
                break;
            }
            taken.push(character);
            remaining -= size;
        }
        entry.text.push_str(&taken);
        self.total_bytes = self.total_bytes.saturating_add(taken.len());
    }

    fn replace(&mut self, seq: u64, text: &str, is_error: Option<bool>) {
        let Some(entry) = self.entries.iter_mut().find(|entry| entry.seq == seq) else {
            return;
        };
        let (text, truncated) = bound(text.to_owned(), MAX_ENTRY_BYTES);
        self.total_bytes = self.total_bytes.saturating_sub(entry.text.len());
        self.total_bytes = self.total_bytes.saturating_add(text.len());
        entry.text = text;
        entry.truncated = truncated;
        if let Some(is_error) = is_error {
            entry.is_error = is_error;
        }
    }

    fn prune(&mut self) {
        while self.entries.len() > MAX_ENTRIES || self.total_bytes > MAX_TOTAL_BYTES {
            if self.entries.is_empty() {
                break;
            }
            let removed = self.entries.remove(0);
            self.total_bytes = self.total_bytes.saturating_sub(removed.text.len());
            self.truncated = true;
            if self.current_assistant == Some(removed.seq) {
                self.current_assistant = None;
            }
            if self.current_thinking == Some(removed.seq) {
                self.current_thinking = None;
            }
        }
        if self.truncated {
            let live: HashSet<u64> = self.entries.iter().map(|entry| entry.seq).collect();
            self.tools.retain(|_, seq| live.contains(seq));
        }
    }

    /// Fold one typed event into the timeline. Returns whether anything
    /// visible changed (used to bump the Host generation).
    pub(crate) fn apply(&mut self, event: &Event) -> bool {
        match &event.kind {
            EventKind::MessageStart(message) if message.role == "assistant" => {
                // Start a fresh message; deltas create the entry so an empty
                // bubble is never rendered.
                self.current_assistant = None;
                self.current_thinking = None;
                false
            }
            EventKind::MessageUpdate {
                delta_kind, delta, ..
            } => match delta_kind.as_str() {
                "text_delta" => {
                    if delta.is_empty() {
                        return false;
                    }
                    let seq = match self.current_assistant {
                        Some(seq) => seq,
                        None => {
                            let seq = self.push(OmpTimelineKind::Assistant, "", None, None, false);
                            self.current_assistant = Some(seq);
                            seq
                        }
                    };
                    self.append(seq, delta);
                    true
                }
                "thinking_delta" => {
                    if delta.is_empty() {
                        return false;
                    }
                    let seq = match self.current_thinking {
                        Some(seq) => seq,
                        None => {
                            let seq = self.push(OmpTimelineKind::Thinking, "", None, None, false);
                            self.current_thinking = Some(seq);
                            seq
                        }
                    };
                    self.append(seq, delta);
                    true
                }
                "error" => {
                    let message = event
                        .raw
                        .get("assistantMessageEvent")
                        .and_then(|update| update.get("error"))
                        .and_then(|error| error.get("errorMessage"))
                        .and_then(|message| message.as_str())
                        .unwrap_or("omp reported an assistant error");
                    self.push(OmpTimelineKind::Error, redact(message), None, None, true);
                    true
                }
                _ => false,
            },
            EventKind::MessageEnd(message) if message.role == "assistant" => {
                if let (Some(seq), Some(text)) = (self.current_assistant, message.text.as_deref()) {
                    let entry = self.entries.iter_mut().find(|entry| entry.seq == seq);
                    if entry.is_some_and(|entry| entry.text.is_empty()) {
                        self.replace(seq, text, None);
                    }
                } else if let Some(text) = message.text.as_deref().filter(|text| !text.is_empty()) {
                    // No streaming deltas were observed; use the final message.
                    self.push(
                        OmpTimelineKind::Assistant,
                        text.to_owned(),
                        None,
                        None,
                        false,
                    );
                }
                self.current_assistant = None;
                true
            }
            EventKind::ToolExecutionStart(tool) => {
                let seq = self.push(
                    OmpTimelineKind::Tool,
                    tool.text.clone().unwrap_or_default(),
                    tool.tool_name.clone(),
                    tool.tool_call_id.clone(),
                    false,
                );
                if let Some(call_id) = &tool.tool_call_id {
                    self.tools.insert(call_id.clone(), seq);
                }
                true
            }
            EventKind::ToolExecutionUpdate(tool) => {
                let seq = tool
                    .tool_call_id
                    .as_ref()
                    .and_then(|call_id| self.tools.get(call_id).copied());
                let Some(seq) = seq else {
                    return false;
                };
                if let Some(text) = &tool.text {
                    // `partialResult` is a snapshot of the output so far.
                    self.replace(seq, text, None);
                    true
                } else {
                    false
                }
            }
            EventKind::ToolExecutionEnd(tool) => {
                let seq = tool
                    .tool_call_id
                    .as_ref()
                    .and_then(|call_id| self.tools.get(call_id).copied())
                    .unwrap_or_else(|| {
                        self.push(
                            OmpTimelineKind::Tool,
                            String::new(),
                            tool.tool_name.clone(),
                            tool.tool_call_id.clone(),
                            false,
                        )
                    });
                if let Some(text) = &tool.text {
                    self.replace(seq, text, tool.is_error);
                } else if let Some(is_error) = tool.is_error {
                    if let Some(entry) = self.entries.iter_mut().find(|entry| entry.seq == seq) {
                        entry.is_error = is_error;
                    }
                }
                true
            }
            EventKind::Notice { level, message, .. } => {
                let kind = if level == "error" {
                    OmpTimelineKind::Error
                } else {
                    OmpTimelineKind::Notice
                };
                self.push(kind, redact(message), None, None, level == "error");
                true
            }
            EventKind::ErrorFrame { message } => {
                self.push(OmpTimelineKind::Error, redact(message), None, None, true);
                true
            }
            EventKind::ProtocolError { message } => {
                self.push(
                    OmpTimelineKind::Error,
                    format!("omp protocol error: {}", redact(message)),
                    None,
                    None,
                    true,
                );
                true
            }
            EventKind::MalformedFrame { detail, bytes } => {
                self.push(
                    OmpTimelineKind::Notice,
                    format!(
                        "Ignored a malformed omp frame ({bytes} bytes): {}",
                        redact(detail)
                    ),
                    None,
                    None,
                    false,
                );
                true
            }
            EventKind::OversizedFrame { limit } => {
                self.push(
                    OmpTimelineKind::Notice,
                    format!(
                        "Ignored an omp frame above the {limit}-byte v1 transport limit; \
                         protocol v2 chunk reassembly is not implemented"
                    ),
                    None,
                    None,
                    false,
                );
                true
            }
            EventKind::Unsupported { frame_type, detail } => {
                self.push(
                    OmpTimelineKind::Notice,
                    format!("Unsupported omp frame `{frame_type}`: {detail}"),
                    None,
                    None,
                    false,
                );
                true
            }
            EventKind::ExtensionUi {
                method,
                requires_response,
                ..
            } => {
                let key = format!("{method}:{requires_response}");
                if !self.suppressed.insert(key) {
                    return false;
                }
                let text = if *requires_response {
                    format!(
                        "omp UI request `{method}` was declined: this Host slice has no omp \
                         approvals or UI bridge"
                    )
                } else {
                    format!("omp `{method}` UI update ignored (headless Host)")
                };
                self.push(OmpTimelineKind::Notice, text, None, None, false);
                true
            }
            EventKind::CommandOutput { text } => {
                if text.trim().is_empty() {
                    return false;
                }
                self.push(OmpTimelineKind::Status, redact(text), None, None, false);
                true
            }
            EventKind::PromptResult {
                agent_invoked: false,
                ..
            } => {
                self.push(
                    OmpTimelineKind::Status,
                    "Handled locally by omp; no agent run was started",
                    None,
                    None,
                    false,
                );
                true
            }
            EventKind::AgentEnd { is_terminal: false } => {
                let key = "deferred-agent-end".to_owned();
                if !self.suppressed.insert(key) {
                    return false;
                }
                self.push(
                    OmpTimelineKind::Status,
                    "omp deferred completion (`isTerminal: false`); asynchronous work will resume",
                    None,
                    None,
                    false,
                );
                true
            }
            EventKind::AutoRetry {
                phase,
                attempt,
                message,
            } => {
                let attempt = attempt
                    .map(|attempt| format!(" (attempt {attempt})"))
                    .unwrap_or_default();
                let message = message
                    .as_deref()
                    .map(|message| format!(": {}", redact(message)))
                    .unwrap_or_default();
                self.push(
                    OmpTimelineKind::Status,
                    format!("omp automatic retry {phase}{attempt}{message}"),
                    None,
                    None,
                    false,
                );
                true
            }
            EventKind::AutoCompaction { phase, reason } => {
                let reason = reason
                    .as_deref()
                    .map(|reason| format!(" ({reason})"))
                    .unwrap_or_default();
                self.push(
                    OmpTimelineKind::Status,
                    format!("omp session compaction {phase}{reason}"),
                    None,
                    None,
                    false,
                );
                true
            }
            _ => false,
        }
    }
}

/// Bound text to `max_bytes` at a char boundary and report truncation.
fn bound(mut text: String, max_bytes: usize) -> (String, bool) {
    if text.len() <= max_bytes {
        return (text, false);
    }
    let mut cut = max_bytes;
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    text.truncate(cut);
    text.push('…');
    text.push_str(" [truncated]");
    (text, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use ukk_omp_client::{MessageEvent, ToolEvent};

    fn event(sequence: u64, kind: EventKind) -> Event {
        Event {
            sequence,
            kind,
            raw: json!({}),
        }
    }

    #[test]
    fn streaming_deltas_form_one_assistant_entry() {
        let mut timeline = Timeline::new();
        timeline.apply(&event(
            1,
            EventKind::MessageStart(MessageEvent {
                role: "assistant".into(),
                text: Some("duplicate".into()),
                tool_name: None,
                tool_call_id: None,
                is_error: None,
            }),
        ));
        for delta in ["Hel", "lo"] {
            timeline.apply(&event(
                2,
                EventKind::MessageUpdate {
                    role: "assistant".into(),
                    delta_kind: "text_delta".into(),
                    delta: delta.into(),
                    content_index: Some(0),
                },
            ));
        }
        assert_eq!(timeline.entries().len(), 1);
        assert_eq!(timeline.entries()[0].kind, OmpTimelineKind::Assistant);
        assert_eq!(timeline.entries()[0].text, "Hello");
    }

    #[test]
    fn tool_updates_replace_partial_output_and_end_sets_error() {
        let mut timeline = Timeline::new();
        timeline.apply(&event(
            1,
            EventKind::ToolExecutionStart(ToolEvent {
                tool_call_id: Some("t1".into()),
                tool_name: Some("bash".into()),
                text: Some("{\"command\":\"ls\"}".into()),
                is_error: None,
            }),
        ));
        timeline.apply(&event(
            2,
            EventKind::ToolExecutionUpdate(ToolEvent {
                tool_call_id: Some("t1".into()),
                tool_name: Some("bash".into()),
                text: Some("partial".into()),
                is_error: None,
            }),
        ));
        timeline.apply(&event(
            3,
            EventKind::ToolExecutionEnd(ToolEvent {
                tool_call_id: Some("t1".into()),
                tool_name: Some("bash".into()),
                text: Some("done".into()),
                is_error: Some(true),
            }),
        ));
        let entry = &timeline.entries()[0];
        assert_eq!(entry.text, "done");
        assert!(entry.is_error);
        assert_eq!(entry.tool_name.as_deref(), Some("bash"));
    }

    #[test]
    fn timeline_is_bounded_and_reports_truncation() {
        let mut timeline = Timeline::new();
        for index in 0..(MAX_ENTRIES + 25) {
            timeline.push(
                OmpTimelineKind::Status,
                format!("entry {index}"),
                None,
                None,
                false,
            );
        }
        assert_eq!(timeline.entries().len(), MAX_ENTRIES);
        assert!(timeline.truncated());
        assert_eq!(timeline.entries().last().unwrap().text, "entry 424");
    }

    #[test]
    fn total_bytes_cap_is_enforced_and_serialized_snapshots_stay_small() {
        let mut timeline = Timeline::new();
        for _ in 0..MAX_ENTRIES {
            timeline.push(
                OmpTimelineKind::Assistant,
                "x".repeat(MAX_ENTRY_BYTES),
                None,
                None,
                false,
            );
        }
        assert!(timeline.total_bytes <= MAX_TOTAL_BYTES);
        assert!(timeline.truncated());
        assert!(timeline.entries().len() < MAX_ENTRIES);
        let serialized = serde_json::to_vec(timeline.entries()).unwrap();
        // The Host reply cap is 4 MiB; the timeline must stay well below it.
        assert!(
            serialized.len() < 4 * 1024 * 1024,
            "snapshot {}",
            serialized.len()
        );
    }

    #[test]
    fn replace_and_append_keep_total_bytes_symmetric() {
        let mut timeline = Timeline::new();
        timeline.push(OmpTimelineKind::Tool, "start", None, None, false);
        let seq = timeline.entries()[0].seq;
        timeline.replace(seq, "a longer replacement", None);
        timeline.append(seq, " plus delta");
        let expected: usize = timeline
            .entries()
            .iter()
            .map(|entry| entry.text.len())
            .sum();
        assert_eq!(timeline.total_bytes, expected);
    }

    #[test]
    fn one_entry_is_bounded() {
        let mut timeline = Timeline::new();
        timeline.push(
            OmpTimelineKind::Tool,
            "x".repeat(MAX_ENTRY_BYTES * 2),
            None,
            None,
            false,
        );
        let entry = &timeline.entries()[0];
        assert!(entry.truncated);
        assert!(entry.text.len() < MAX_ENTRY_BYTES + 32);
    }

    #[test]
    fn identical_ui_updates_are_suppressed_per_turn() {
        let mut timeline = Timeline::new();
        let ui = || {
            event(
                1,
                EventKind::ExtensionUi {
                    id: "1".into(),
                    method: "setWidget".into(),
                    requires_response: false,
                },
            )
        };
        assert!(timeline.apply(&ui()));
        assert!(!timeline.apply(&ui()));
        timeline.begin_turn();
        assert!(timeline.apply(&ui()));
    }
}
