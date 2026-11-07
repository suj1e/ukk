//! Merge and replay semantics for the neutral event contract.
//!
//! Rules:
//! - Sequences are per transport stream and per parsed transport line. All
//!   events produced by one line are ingested as one batch. A gap buffers
//!   later batches; a sequence below `next` is a replay and is dropped.
//! - Lifecycle events with a documented native identity are deduplicated.
//!   Identical duplicates are counted; the same key with different content is
//!   reported as a conflict and the first value is retained.
//! - Deltas are applied in stream order and are explicitly not deduplicated by
//!   content, because providers do not document a replay-stable delta id.
//! - A resumed session is an explicit event. It keeps already-merged history
//!   and never wipes state merely because the transport reconnected.
//! - Unsupported provider actions become typed notices; they never become a
//!   turn, message, tool call or approval.

use crate::contract::{
    AdapterEvent, ApprovalDecision, ApprovalEvidence, ApprovalRequest, Artifact, EventKey,
    EventPayload, Message, MessagePart, PermissionDenied, Plan, Progress, SessionStatus, ToolCall,
    ToolKind, ToolStatus, TurnInfo, UnsupportedAction, CONTRACT_VERSION,
};
use crate::error::AdapterError;
use crate::ids::{ItemId, RequestId, SessionId, StreamId};
use crate::limits::{truncate_utf8, Limits};
use crate::util::stable_fingerprint;
use std::collections::{BTreeMap, HashMap, VecDeque};

#[derive(Clone, Debug, PartialEq)]
pub enum MergeOutcome {
    Applied {
        key: Option<String>,
    },
    Buffered {
        awaiting_sequence: u64,
    },
    Duplicate {
        key: String,
    },
    Conflict {
        key: String,
    },
    Replay {
        sequence: u64,
    },
    ContractMismatch {
        expected: u32,
        actual: u32,
    },
    SessionMismatch {
        expected: String,
        actual: String,
    },
    Unscoped {
        native_type: String,
    },
    Ignored {
        reason: String,
    },
    /// A sequenced line produced no events. The stream cursor still advanced.
    Skipped {
        sequence: u64,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct IngestOutcome {
    pub outcomes: Vec<MergeOutcome>,
    pub fallback: Option<crate::contract::TerminalFallback>,
    pub buffered: usize,
}

impl IngestOutcome {
    fn from_outcome(outcome: MergeOutcome) -> Self {
        Self {
            outcomes: vec![outcome],
            fallback: None,
            buffered: 0,
        }
    }

    fn fallback(error: AdapterError) -> Self {
        Self {
            outcomes: Vec::new(),
            fallback: Some(error.fallback()),
            buffered: 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct MergeConflict {
    pub key: EventKey,
    pub retained_native_type: String,
    pub incoming_native_type: String,
}

#[derive(Clone, Debug, PartialEq)]
struct PayloadStamp {
    fingerprint: u64,
    native_type: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ApprovalClearance {
    pub evidence: ApprovalEvidence,
    /// Only set when the provider supplied documented evidence of the decision.
    /// `serverRequest/resolved` alone does not identify the decision.
    pub decision: Option<ApprovalDecision>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ApprovalView {
    pub request: ApprovalRequest,
    /// Transport stream that carried the request. JSON-RPC request ids are
    /// only unique per connection.
    pub stream_id: StreamId,
    pub clearance: Option<ApprovalClearance>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TurnView {
    pub native_id: String,
    pub info: TurnInfo,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlanTextState {
    pub text: String,
    pub truncated: bool,
    pub final_item: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SessionView {
    pub id: Option<SessionId>,
    pub native_id: Option<String>,
    pub resumed: bool,
    pub status: SessionStatus,
    pub turns: BTreeMap<String, TurnView>,
    pub messages: BTreeMap<String, Message>,
    pub tools: BTreeMap<String, ToolCall>,
    pub approvals: BTreeMap<String, ApprovalView>,
    pub plans: BTreeMap<String, Plan>,
    pub plan_text: BTreeMap<String, PlanTextState>,
    pub artifacts: BTreeMap<String, Vec<Artifact>>,
    pub progress: Vec<Progress>,
    pub permission_denials: Vec<PermissionDenied>,
    pub unsupported: Vec<UnsupportedAction>,
    pub notices: Vec<crate::contract::Notice>,
}

impl SessionView {
    fn new() -> Self {
        Self {
            status: SessionStatus::Unknown,
            ..Self::default()
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ConversationState {
    pub provider: crate::capability::ProviderKind,
    pub session: SessionView,
}

impl ConversationState {
    pub fn new(provider: crate::capability::ProviderKind) -> Self {
        Self {
            provider,
            session: SessionView::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct StreamCursor {
    next: u64,
    pending: BTreeMap<u64, Vec<AdapterEvent>>,
}

pub struct MergeEngine {
    state: ConversationState,
    streams: HashMap<StreamId, StreamCursor>,
    stream_order: VecDeque<StreamId>,
    seen: HashMap<EventKey, PayloadStamp>,
    seen_order: VecDeque<EventKey>,
    conflicts: Vec<MergeConflict>,
    limits: Limits,
}

impl MergeEngine {
    pub fn new(provider: crate::capability::ProviderKind, limits: Limits) -> Self {
        Self {
            state: ConversationState::new(provider),
            streams: HashMap::new(),
            stream_order: VecDeque::new(),
            seen: HashMap::new(),
            seen_order: VecDeque::new(),
            conflicts: Vec::new(),
            limits,
        }
    }

    pub fn state(&self) -> &ConversationState {
        &self.state
    }

    pub fn session(&self) -> &SessionView {
        &self.state.session
    }

    pub fn conflicts(&self) -> &[MergeConflict] {
        &self.conflicts
    }

    pub fn into_state(self) -> ConversationState {
        self.state
    }

    /// Ingest one event. Equivalent to a one-event batch.
    pub fn ingest(&mut self, event: AdapterEvent) -> IngestOutcome {
        self.ingest_batch(vec![event])
    }

    /// Convenience for a non-empty batch. `ingest_line` is the canonical entry
    /// point because a transport line may legitimately produce zero events.
    pub fn ingest_batch(&mut self, events: Vec<AdapterEvent>) -> IngestOutcome {
        let Some(first) = events.first() else {
            return IngestOutcome::fallback(AdapterError::InvalidField {
                native_type: "merge".to_owned(),
                field: "batch",
                detail: "empty batch cannot establish a stream position; use ingest_line"
                    .to_owned(),
            });
        };
        let stream = first.provenance.stream_id.clone();
        let sequence = first.provenance.sequence;
        self.ingest_line(&stream, sequence, events)
    }

    /// Ingest every event produced by one transport line.
    ///
    /// The call advances the stream cursor even when `events` is empty, so
    /// blank lines, responses without modeled events, `ping` and
    /// `message_stop` do not create permanent sequence gaps.
    pub fn ingest_line(
        &mut self,
        stream: &StreamId,
        sequence: u64,
        events: Vec<AdapterEvent>,
    ) -> IngestOutcome {
        if let Some(event) = events
            .iter()
            .find(|event| event.contract_version != CONTRACT_VERSION)
        {
            return IngestOutcome::fallback(AdapterError::ContractVersionMismatch {
                expected: CONTRACT_VERSION,
                actual: event.contract_version,
            });
        }
        if events.iter().any(|event| {
            &event.provenance.stream_id != stream || event.provenance.sequence != sequence
        }) {
            return IngestOutcome::fallback(AdapterError::InvalidField {
                native_type: "merge".to_owned(),
                field: "batch",
                detail: "events from one line must share stream and sequence".to_owned(),
            });
        }

        self.touch_stream(stream);

        enum Step {
            Apply(Vec<AdapterEvent>),
            Buffer,
            Replay,
            Overflow,
        }

        let step = {
            let cursor = self
                .streams
                .get_mut(stream)
                .expect("touch_stream establishes the stream cursor");
            if sequence < cursor.next {
                Step::Replay
            } else if sequence == cursor.next {
                cursor.next += 1;
                Step::Apply(events)
            } else if cursor.pending.contains_key(&sequence) {
                Step::Replay
            } else if cursor.pending.len() >= self.limits.max_pending_events {
                Step::Overflow
            } else {
                cursor.pending.insert(sequence, events);
                Step::Buffer
            }
        };

        match step {
            Step::Replay => IngestOutcome::from_outcome(MergeOutcome::Replay { sequence }),
            Step::Overflow => IngestOutcome::fallback(AdapterError::StreamBufferOverflow {
                limit: self.limits.max_pending_events,
            }),
            Step::Buffer => {
                let awaiting_sequence = self
                    .streams
                    .get(stream)
                    .map(|cursor| cursor.next)
                    .unwrap_or(sequence);
                IngestOutcome {
                    outcomes: vec![MergeOutcome::Buffered { awaiting_sequence }],
                    fallback: None,
                    buffered: self.pending_count(stream),
                }
            }
            Step::Apply(events) => {
                let mut outcomes = Vec::new();
                if events.is_empty() {
                    outcomes.push(MergeOutcome::Skipped { sequence });
                }
                for event in events {
                    outcomes.push(self.apply_event(event));
                }
                while let Some((pending_sequence, pending)) = self.take_next_pending(stream) {
                    if pending.is_empty() {
                        outcomes.push(MergeOutcome::Skipped {
                            sequence: pending_sequence,
                        });
                    }
                    for event in pending {
                        outcomes.push(self.apply_event(event));
                    }
                }
                IngestOutcome {
                    outcomes,
                    fallback: None,
                    buffered: self.pending_count(stream),
                }
            }
        }
    }

    fn touch_stream(&mut self, stream: &StreamId) {
        if self.streams.contains_key(stream) {
            self.stream_order.retain(|known| known != stream);
        } else {
            while self.streams.len() >= self.limits.max_streams.max(1) {
                let evictable = self.stream_order.iter().position(|known| {
                    known != stream
                        && self
                            .streams
                            .get(known)
                            .is_some_and(|cursor| cursor.pending.is_empty())
                });
                let Some(index) = evictable else {
                    break;
                };
                if let Some(oldest) = self.stream_order.remove(index) {
                    self.streams.remove(&oldest);
                }
            }
            self.streams.insert(
                stream.clone(),
                StreamCursor {
                    next: 1,
                    pending: BTreeMap::new(),
                },
            );
        }
        self.stream_order.push_back(stream.clone());
    }

    fn pending_count(&self, stream: &StreamId) -> usize {
        self.streams
            .get(stream)
            .map(|cursor| cursor.pending.len())
            .unwrap_or(0)
    }

    fn take_next_pending(&mut self, stream: &StreamId) -> Option<(u64, Vec<AdapterEvent>)> {
        let cursor = self.streams.get_mut(stream)?;
        let sequence = cursor.next;
        let batch = cursor.pending.remove(&sequence)?;
        cursor.next += 1;
        Some((sequence, batch))
    }

    fn apply_event(&mut self, event: AdapterEvent) -> MergeOutcome {
        let key = event.event_key();
        let fingerprint = key.as_ref().map(|_| payload_fingerprint(&event.payload));
        if let (Some(key), Some(fingerprint)) = (&key, fingerprint) {
            if let Some(existing) = self.seen.get(key) {
                if existing.fingerprint == fingerprint {
                    return MergeOutcome::Duplicate {
                        key: key.to_string(),
                    };
                }
                if self.conflicts.len() < self.limits.max_progress_entries {
                    self.conflicts.push(MergeConflict {
                        key: key.clone(),
                        retained_native_type: existing.native_type.clone(),
                        incoming_native_type: event.provenance.native_type.clone(),
                    });
                }
                return MergeOutcome::Conflict {
                    key: key.to_string(),
                };
            }
        }

        if let Some(session_id) = &event.provenance.session_id {
            if let Some(current) = &self.state.session.id {
                if current != session_id {
                    return MergeOutcome::SessionMismatch {
                        expected: current.to_string(),
                        actual: session_id.to_string(),
                    };
                }
            } else {
                self.state.session.id = Some(session_id.clone());
            }
        } else if requires_session(&event.payload) {
            return MergeOutcome::Unscoped {
                native_type: event.provenance.native_type.clone(),
            };
        }

        let outcome = self.apply_payload(&event);
        if let (Some(key), Some(fingerprint)) = (key, fingerprint) {
            self.remember(
                key,
                PayloadStamp {
                    fingerprint,
                    native_type: event.provenance.native_type.clone(),
                },
            );
        }
        outcome
    }

    fn remember(&mut self, key: EventKey, payload: PayloadStamp) {
        if self.seen.len() >= self.limits.max_dedupe_entries {
            if let Some(oldest) = self.seen_order.pop_front() {
                self.seen.remove(&oldest);
            }
        }
        self.seen_order.push_back(key.clone());
        self.seen.insert(key, payload);
    }

    fn apply_payload(&mut self, event: &AdapterEvent) -> MergeOutcome {
        let session = &mut self.state.session;
        match &event.payload {
            EventPayload::SessionStarted { session: info, .. } => {
                session.id = Some(info.id.clone());
                session.native_id = info.native_session_id.clone();
                session.status = SessionStatus::Started;
                MergeOutcome::Applied { key: None }
            }
            EventPayload::SessionResumed {
                session: info,
                previous_native_id,
            } => {
                session.id = Some(info.id.clone());
                session.native_id = Some(previous_native_id.to_string());
                session.resumed = true;
                session.status = SessionStatus::Started;
                MergeOutcome::Applied { key: None }
            }
            EventPayload::SessionStatus { status } => {
                session.status = status.clone();
                MergeOutcome::Applied { key: None }
            }
            EventPayload::TurnStarted { turn } => {
                let key = turn_key(turn, &event.provenance);
                session.turns.insert(
                    key.clone(),
                    TurnView {
                        native_id: key,
                        info: turn.clone(),
                    },
                );
                MergeOutcome::Applied { key: None }
            }
            EventPayload::TurnCompleted { turn } => {
                let key = turn_key(turn, &event.provenance);
                session.turns.insert(
                    key.clone(),
                    TurnView {
                        native_id: key,
                        info: turn.clone(),
                    },
                );
                MergeOutcome::Applied { key: None }
            }
            EventPayload::MessageStarted { message } => {
                let mut message = message.clone();
                cap_message(&mut message, self.limits.max_text_bytes);
                let id = message.id.to_string();
                match session.messages.get_mut(&id) {
                    Some(existing) if existing.complete => MergeOutcome::Ignored {
                        reason: "message start arrived after completion".to_owned(),
                    },
                    Some(existing) => {
                        *existing = message;
                        MergeOutcome::Applied { key: None }
                    }
                    None => {
                        session.messages.insert(id, message);
                        MergeOutcome::Applied { key: None }
                    }
                }
            }
            EventPayload::MessageDelta {
                message_id,
                role,
                text,
            } => {
                let id = message_id.to_string();
                let message = session.messages.entry(id).or_insert_with(|| Message {
                    id: message_id.clone(),
                    role: role.clone(),
                    parts: Vec::new(),
                    phase: None,
                    complete: false,
                    truncated: false,
                });
                if message.complete {
                    return MergeOutcome::Ignored {
                        reason: "message delta arrived after authoritative completion".to_owned(),
                    };
                }
                message.truncated |= text.truncated;
                let max_text = self.limits.max_text_bytes;
                match message.parts.last_mut() {
                    Some(MessagePart::Text { text: part }) => {
                        part.push_str(&text.text);
                        let (bounded, truncated) = truncate_utf8(part, max_text);
                        if truncated {
                            message.truncated = true;
                            *part = bounded.to_owned();
                        }
                    }
                    _ => message.parts.push(MessagePart::Text {
                        text: text.text.clone(),
                    }),
                }
                cap_message(message, max_text);
                MergeOutcome::Applied { key: None }
            }
            EventPayload::MessageCompleted { message } => {
                let mut message = message.clone();
                cap_message(&mut message, self.limits.max_text_bytes);
                session.messages.insert(message.id.to_string(), message);
                MergeOutcome::Applied { key: None }
            }
            EventPayload::ToolCallStarted { call } => {
                apply_tool_started(session, call.clone());
                MergeOutcome::Applied { key: None }
            }
            EventPayload::ToolCallCompleted { call } => {
                apply_tool_completed(session, call.clone());
                clear_approval_from_tool(session, call, &event.provenance.stream_id);
                MergeOutcome::Applied { key: None }
            }
            EventPayload::Progress { progress } => {
                session.progress.push(progress.clone());
                if session.progress.len() > self.limits.max_progress_entries {
                    let excess = session.progress.len() - self.limits.max_progress_entries;
                    session.progress.drain(0..excess);
                }
                MergeOutcome::Applied { key: None }
            }
            EventPayload::Artifacts { artifacts } => {
                let key = event
                    .provenance
                    .item_id
                    .as_ref()
                    .map(ItemId::to_string)
                    .or_else(|| event.provenance.native_id.clone())
                    .unwrap_or_else(|| "unscoped".to_owned());
                session.artifacts.insert(key, artifacts.clone());
                MergeOutcome::Applied { key: None }
            }
            EventPayload::Plan { plan } => {
                let key = event
                    .provenance
                    .turn_id
                    .as_ref()
                    .map(|turn| turn.to_string())
                    .or_else(|| event.provenance.native_id.clone())
                    .unwrap_or_else(|| "unscoped".to_owned());
                session.plans.insert(key, plan.clone());
                MergeOutcome::Applied { key: None }
            }
            EventPayload::PlanText {
                item_id,
                text,
                delta,
                final_item,
            } => {
                let key = item_id.to_string();
                let entry = session
                    .plan_text
                    .entry(key)
                    .or_insert_with(|| PlanTextState {
                        text: String::new(),
                        truncated: false,
                        final_item: false,
                    });
                if *final_item {
                    entry.text = text.text.clone();
                    entry.final_item = true;
                } else if entry.final_item {
                    return MergeOutcome::Ignored {
                        reason: "plan text arrived after the final item".to_owned(),
                    };
                } else if *delta {
                    entry.text.push_str(&text.text);
                } else {
                    entry.text = text.text.clone();
                }
                entry.truncated |= text.truncated;
                let (bounded, truncated) = truncate_utf8(&entry.text, self.limits.max_text_bytes);
                if truncated {
                    entry.truncated = true;
                    entry.text = bounded.to_owned();
                }
                MergeOutcome::Applied { key: None }
            }
            EventPayload::ApprovalRequested { approval } => {
                session.approvals.insert(
                    approval_key(&event.provenance.stream_id, &approval.request_id),
                    ApprovalView {
                        request: approval.clone(),
                        stream_id: event.provenance.stream_id.clone(),
                        clearance: None,
                    },
                );
                MergeOutcome::Applied { key: None }
            }
            EventPayload::ApprovalCleared {
                approval_id,
                request_id,
                evidence,
            } => {
                let key = approval_key(&event.provenance.stream_id, request_id);
                let Some(view) = session.approvals.get_mut(&key) else {
                    return MergeOutcome::Ignored {
                        reason: format!("approval clearance for unknown request {request_id}"),
                    };
                };
                if view.request.id != *approval_id {
                    return MergeOutcome::Ignored {
                        reason: "approval clearance id does not match the request".to_owned(),
                    };
                }
                let decision = match evidence {
                    ApprovalEvidence::ItemStatus(ToolStatus::Declined) => {
                        Some(ApprovalDecision::Decline)
                    }
                    ApprovalEvidence::ItemStatus(_) => None,
                    ApprovalEvidence::ServerRequestResolved => None,
                };
                if matches!(evidence, ApprovalEvidence::ServerRequestResolved)
                    && view
                        .clearance
                        .as_ref()
                        .is_some_and(|clearance| clearance.decision.is_some())
                {
                    return MergeOutcome::Ignored {
                        reason: "resolved notification cannot clear a documented decision"
                            .to_owned(),
                    };
                }
                view.clearance = Some(ApprovalClearance {
                    evidence: evidence.clone(),
                    decision,
                });
                MergeOutcome::Applied { key: None }
            }
            EventPayload::PermissionDenied { denial } => {
                push_bounded(
                    &mut session.permission_denials,
                    denial.clone(),
                    self.limits.max_progress_entries,
                );
                MergeOutcome::Applied { key: None }
            }
            EventPayload::Notice { notice } => {
                push_bounded(
                    &mut session.notices,
                    notice.clone(),
                    self.limits.max_progress_entries,
                );
                MergeOutcome::Applied { key: None }
            }
            EventPayload::Unsupported { action } => {
                push_bounded(
                    &mut session.unsupported,
                    action.clone(),
                    self.limits.max_progress_entries,
                );
                MergeOutcome::Applied { key: None }
            }
        }
    }
}

fn requires_session(payload: &EventPayload) -> bool {
    !matches!(
        payload,
        EventPayload::SessionStarted { .. }
            | EventPayload::SessionResumed { .. }
            | EventPayload::Notice { .. }
            | EventPayload::Unsupported { .. }
    )
}

fn turn_key(turn: &TurnInfo, provenance: &crate::contract::Provenance) -> String {
    turn.id
        .as_ref()
        .map(|id| id.to_string())
        .or_else(|| provenance.native_id.clone())
        .unwrap_or_else(|| "turn".to_owned())
}

fn payload_fingerprint(payload: &EventPayload) -> u64 {
    match serde_json::to_vec(payload) {
        Ok(serialized) => stable_fingerprint(&String::from_utf8_lossy(&serialized)),
        Err(_) => 0,
    }
}

fn cap_message(message: &mut Message, limit: usize) {
    let mut used = 0usize;
    let mut truncated = message.truncated;
    for part in &mut message.parts {
        match part {
            MessagePart::Text { text } => {
                if used >= limit {
                    if !text.is_empty() {
                        truncated = true;
                    }
                    text.clear();
                    continue;
                }
                let remaining = limit - used;
                let (bounded, was_truncated) = truncate_utf8(text, remaining);
                used = used.saturating_add(bounded.len());
                if was_truncated {
                    truncated = true;
                }
                *text = bounded.to_owned();
            }
            MessagePart::Attachment { label, reference } => {
                used = used.saturating_add(label.len());
                if let Some(reference) = reference {
                    used = used.saturating_add(reference.len());
                }
            }
        }
    }
    message.truncated = truncated;
}

fn push_bounded<T>(values: &mut Vec<T>, value: T, limit: usize) {
    if limit == 0 {
        return;
    }
    if values.len() >= limit {
        values.remove(0);
    }
    values.push(value);
}

fn apply_tool_started(session: &mut SessionView, call: ToolCall) {
    match session.tools.get_mut(call.id.as_str()) {
        Some(existing) => {
            existing.name = call.name.or_else(|| existing.name.clone());
            if call.kind != ToolKind::Unknown {
                existing.kind = call.kind;
            }
            existing.status = call.status;
            existing.input = call.input.or_else(|| existing.input.clone());
            existing.output = call.output.or_else(|| existing.output.clone());
            existing.exit_code = call.exit_code.or(existing.exit_code);
            existing.duration_ms = call.duration_ms.or(existing.duration_ms);
        }
        None => {
            session.tools.insert(call.id.to_string(), call);
        }
    }
}

fn apply_tool_completed(session: &mut SessionView, call: ToolCall) {
    match session.tools.get_mut(call.id.as_str()) {
        Some(existing) => {
            if existing.name.is_none() {
                existing.name = call.name;
            }
            if existing.kind == ToolKind::Unknown && call.kind != ToolKind::Unknown {
                existing.kind = call.kind;
            }
            existing.status = call.status;
            if call.input.is_some() {
                existing.input = call.input;
            }
            if call.output.is_some() {
                existing.output = call.output;
            }
            existing.exit_code = call.exit_code.or(existing.exit_code);
            existing.duration_ms = call.duration_ms.or(existing.duration_ms);
        }
        None => {
            session.tools.insert(call.id.to_string(), call);
        }
    }
}

fn approval_key(stream: &StreamId, request: &RequestId) -> String {
    format!("{stream}:{request}")
}

fn clear_approval_from_tool(session: &mut SessionView, call: &ToolCall, stream: &StreamId) {
    if call.status != ToolStatus::Declined {
        return;
    }
    for view in session.approvals.values_mut() {
        let has_decision = view
            .clearance
            .as_ref()
            .is_some_and(|clearance| clearance.decision.is_some());
        if !has_decision
            && &view.stream_id == stream
            && view.request.item_id.as_ref() == Some(&call.id)
        {
            view.clearance = Some(ApprovalClearance {
                evidence: ApprovalEvidence::ItemStatus(ToolStatus::Declined),
                decision: Some(ApprovalDecision::Decline),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::ProviderKind;
    use crate::contract::{Message, Role};
    use crate::limits::BoundedText;

    fn event(sequence: u64, payload: EventPayload) -> AdapterEvent {
        AdapterEvent::new(
            ProviderKind::CodexAppServer,
            crate::contract::Provenance {
                native_type: "test".to_owned(),
                native_id: None,
                parent_native_id: None,
                session_id: Some(SessionId::from("s1")),
                turn_id: None,
                item_id: None,
                stream_id: StreamId::from("stream"),
                sequence,
            },
            payload,
        )
    }

    fn started(id: &str) -> EventPayload {
        EventPayload::MessageStarted {
            message: Message {
                id: ItemId::from(id),
                role: Role::Assistant,
                parts: vec![MessagePart::Text {
                    text: String::new(),
                }],
                phase: None,
                complete: false,
                truncated: false,
            },
        }
    }

    fn completed(id: &str, text: &str) -> EventPayload {
        EventPayload::MessageCompleted {
            message: Message {
                id: ItemId::from(id),
                role: Role::Assistant,
                parts: vec![MessagePart::Text {
                    text: text.to_owned(),
                }],
                phase: None,
                complete: true,
                truncated: false,
            },
        }
    }

    #[test]
    fn merges_out_of_order_events_once_the_gap_closes() {
        let mut merge = MergeEngine::new(ProviderKind::CodexAppServer, Limits::default());
        let result = merge.ingest(event(3, completed("m1", "final")));
        assert!(matches!(
            result.outcomes.as_slice(),
            [MergeOutcome::Buffered {
                awaiting_sequence: 1
            }]
        ));
        assert_eq!(merge.session().messages.len(), 0);

        let result = merge.ingest(event(1, started("m1")));
        assert!(matches!(
            result.outcomes.as_slice(),
            [MergeOutcome::Applied { .. }]
        ));
        assert_eq!(result.buffered, 1);

        let result = merge.ingest(event(
            2,
            EventPayload::MessageDelta {
                message_id: ItemId::from("m1"),
                role: Role::Assistant,
                text: BoundedText::from_text("partial"),
            },
        ));
        assert_eq!(result.outcomes.len(), 2);
        assert!(merge.session().messages.contains_key("m1"));
    }

    #[test]
    fn duplicate_sequences_are_replays() {
        let mut merge = MergeEngine::new(ProviderKind::CodexAppServer, Limits::default());
        merge.ingest(event(1, started("m1")));
        let again = merge.ingest(event(1, started("m1")));
        assert!(matches!(
            again.outcomes.as_slice(),
            [MergeOutcome::Replay { sequence: 1 }]
        ));
    }

    #[test]
    fn conflicting_payloads_are_reported_and_first_value_wins() {
        let mut merge = MergeEngine::new(ProviderKind::CodexAppServer, Limits::default());
        merge.ingest(event(1, completed("m1", "first")));
        let conflict = merge.ingest(event(2, completed("m1", "second")));
        assert!(matches!(
            conflict.outcomes.as_slice(),
            [MergeOutcome::Conflict { .. }]
        ));
        assert_eq!(merge.session().messages["m1"].text(), "first");
        assert_eq!(merge.conflicts().len(), 1);
    }

    #[test]
    fn delta_after_completion_is_ignored() {
        let mut merge = MergeEngine::new(ProviderKind::CodexAppServer, Limits::default());
        merge.ingest(event(1, completed("m1", "final")));
        let result = merge.ingest(event(
            2,
            EventPayload::MessageDelta {
                message_id: ItemId::from("m1"),
                role: Role::Assistant,
                text: BoundedText::from_text("late"),
            },
        ));
        assert!(matches!(
            result.outcomes.as_slice(),
            [MergeOutcome::Ignored { .. }]
        ));
        assert_eq!(merge.session().messages["m1"].text(), "final");
    }
}
