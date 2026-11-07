//! The versioned provider-neutral conversation contract.
//!
//! Every structured observation a provider adapter emits is one of these
//! events. The contract deliberately contains no terminal-text interpretation:
//! if a provider does not document a semantic, the adapter emits an
//! [`EventPayload::Unsupported`] notice or asks the host to degrade to raw
//! terminal output. It never invents a turn, tool call or approval.

use crate::capability::ProviderKind;
use crate::ids::{ApprovalId, ItemId, RequestId, SessionId, StreamId, TurnId};
pub use crate::limits::BoundedText;
use serde::{Deserialize, Serialize};

/// Current wire version of this contract.
pub const CONTRACT_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AdapterEvent {
    pub contract_version: u32,
    pub provider: ProviderKind,
    pub provenance: Provenance,
    pub payload: EventPayload,
}

impl AdapterEvent {
    pub fn new(provider: ProviderKind, provenance: Provenance, payload: EventPayload) -> Self {
        Self {
            contract_version: CONTRACT_VERSION,
            provider,
            provenance,
            payload,
        }
    }

    /// A stable identity for idempotent lifecycle events, when one is known.
    ///
    /// Streaming deltas and progress observations deliberately return `None`:
    /// providers do not document a replay-stable identity for them, and
    /// treating equal text as a duplicate would erase legitimate repetition.
    pub fn event_key(&self) -> Option<EventKey> {
        if matches!(
            self.payload,
            EventPayload::MessageDelta { .. }
                | EventPayload::Progress { .. }
                | EventPayload::Notice { .. }
                | EventPayload::Unsupported { .. }
        ) {
            return None;
        }
        let identity = self
            .provenance
            .native_id
            .as_deref()
            .or_else(|| self.provenance.item_id.as_ref().map(ItemId::as_str))
            .or_else(|| self.provenance.turn_id.as_ref().map(TurnId::as_str))
            .or_else(|| self.provenance.session_id.as_ref().map(SessionId::as_str))?;
        Some(EventKey(format!(
            "{}:{}:{}",
            self.provider.as_str(),
            payload_kind(&self.payload),
            identity
        )))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EventKey(String);

pub(crate) fn payload_kind(payload: &EventPayload) -> &'static str {
    match payload {
        EventPayload::SessionStarted { .. } => "session_started",
        EventPayload::SessionResumed { .. } => "session_resumed",
        EventPayload::SessionStatus { .. } => "session_status",
        EventPayload::TurnStarted { .. } => "turn_started",
        EventPayload::TurnCompleted { .. } => "turn_completed",
        EventPayload::MessageStarted { .. } => "message_started",
        EventPayload::MessageDelta { .. } => "message_delta",
        EventPayload::MessageCompleted { .. } => "message_completed",
        EventPayload::ToolCallStarted { .. } => "tool_call_started",
        EventPayload::ToolCallCompleted { .. } => "tool_call_completed",
        EventPayload::Progress { .. } => "progress",
        EventPayload::Artifacts { .. } => "artifacts",
        EventPayload::Plan { .. } => "plan",
        EventPayload::PlanText { .. } => "plan_text",
        EventPayload::ApprovalRequested { .. } => "approval_requested",
        EventPayload::ApprovalCleared { .. } => "approval_cleared",
        EventPayload::PermissionDenied { .. } => "permission_denied",
        EventPayload::Notice { .. } => "notice",
        EventPayload::Unsupported { .. } => "unsupported",
    }
}

impl EventKey {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for EventKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Where an event came from. The native identifiers are retained so a host can
/// correlate UI state with provider logs and approval responses.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Provenance {
    /// Provider method, notification type, item type or response kind.
    pub native_type: String,
    /// Stable provider event/message/request identity, when documented.
    pub native_id: Option<String>,
    /// Parent tool-use identity for subagent messages, when documented.
    pub parent_native_id: Option<String>,
    pub session_id: Option<SessionId>,
    pub turn_id: Option<TurnId>,
    pub item_id: Option<ItemId>,
    /// Host-assigned transport stream. Sequence is per stream, not global.
    pub stream_id: StreamId,
    /// Adapter-assigned per-stream sequence used for replay and gap handling.
    pub sequence: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventPayload {
    SessionStarted {
        session: SessionInfo,
        resumed_from: Option<SessionId>,
    },
    SessionResumed {
        session: SessionInfo,
        previous_native_id: SessionId,
    },
    SessionStatus {
        status: SessionStatus,
    },
    TurnStarted {
        turn: TurnInfo,
    },
    TurnCompleted {
        turn: TurnInfo,
    },
    MessageStarted {
        message: Message,
    },
    MessageDelta {
        message_id: ItemId,
        role: Role,
        text: BoundedText,
    },
    MessageCompleted {
        message: Message,
    },
    ToolCallStarted {
        call: ToolCall,
    },
    ToolCallCompleted {
        call: ToolCall,
    },
    Progress {
        progress: Progress,
    },
    Artifacts {
        artifacts: Vec<Artifact>,
    },
    Plan {
        plan: Plan,
    },
    PlanText {
        item_id: ItemId,
        text: BoundedText,
        /// True when this observation is an incremental delta and must be
        /// appended. False when it is a full item update, which replaces any
        /// partial text.
        delta: bool,
        final_item: bool,
    },
    ApprovalRequested {
        approval: ApprovalRequest,
    },
    ApprovalCleared {
        approval_id: ApprovalId,
        request_id: RequestId,
        evidence: ApprovalEvidence,
    },
    PermissionDenied {
        denial: PermissionDenied,
    },
    Notice {
        notice: Notice,
    },
    Unsupported {
        action: UnsupportedAction,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: SessionId,
    pub native_session_id: Option<String>,
    pub model: Option<String>,
    pub cwd: Option<BoundedText>,
    pub tools: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionStatus {
    #[default]
    Unknown,
    Started,
    Idle,
    Active {
        flags: Vec<String>,
    },
    NotLoaded,
    Error,
    Closed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnInfo {
    /// Present when the provider documents a stable turn id.
    pub id: Option<TurnId>,
    pub status: TurnStatus,
    pub error: Option<ProviderError>,
    pub duration_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnStatus {
    InProgress,
    Completed,
    Interrupted,
    Failed,
    Unknown(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderError {
    pub message: BoundedText,
    pub code: Option<String>,
    pub details: Option<BoundedText>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub id: ItemId,
    pub role: Role,
    pub parts: Vec<MessagePart>,
    pub phase: Option<String>,
    pub complete: bool,
    pub truncated: bool,
}

impl Message {
    pub fn text(&self) -> String {
        let mut text = String::new();
        for part in &self.parts {
            match part {
                MessagePart::Text { text: part } | MessagePart::Attachment { label: part, .. } => {
                    if !text.is_empty() && !part.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(part);
                }
            }
        }
        text
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
    System,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MessagePart {
    Text {
        text: String,
    },
    /// Non-text documented content (image/skill/mention) kept as a typed
    /// attachment rather than synthesized prose.
    Attachment {
        label: String,
        reference: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: ItemId,
    /// `None` when a completion arrives without a documented tool name.
    pub name: Option<String>,
    pub kind: ToolKind,
    pub status: ToolStatus,
    pub input: Option<ToolInput>,
    pub output: Option<BoundedText>,
    pub exit_code: Option<i32>,
    pub duration_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    Command,
    FileChange,
    Mcp,
    WebSearch,
    Dynamic,
    /// Provider-named tool whose semantics the adapter does not classify.
    ProviderNamed,
    /// Completion arrived without enough documented information.
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
    Declined,
    Interrupted,
    Unknown(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolInput {
    Command {
        command: BoundedText,
        cwd: Option<BoundedText>,
    },
    FileChange {
        changes: Vec<FileChange>,
    },
    Mcp {
        server: String,
        tool: String,
        arguments: BoundedText,
    },
    WebSearch {
        query: String,
    },
    Named {
        name: String,
        arguments: BoundedText,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileChange {
    pub path: String,
    pub kind: Option<String>,
    pub diff: BoundedText,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Progress {
    pub kind: ProgressKind,
    pub text: Option<BoundedText>,
    pub tool_call_id: Option<ItemId>,
    pub attempt: Option<u32>,
    pub max_attempts: Option<u32>,
    pub retry_delay_ms: Option<u64>,
    pub elapsed_seconds: Option<u64>,
    pub heartbeat: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgressKind {
    ToolOutput,
    ToolProgress,
    ToolInput,
    ReasoningSummary,
    ReasoningRaw,
    PlanText,
    Retry,
    Status,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artifact {
    pub path: Option<String>,
    pub kind: ArtifactKind,
    pub change_kind: Option<String>,
    pub content: BoundedText,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    FileChange,
    TurnDiff,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    pub explanation: Option<BoundedText>,
    pub steps: Vec<PlanStep>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanStep {
    pub step: String,
    pub status: PlanStepStatus,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanStepStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalRequest {
    pub id: ApprovalId,
    pub request_id: RequestId,
    pub kind: ApprovalKind,
    /// The tool/item this approval gates, when documented.
    pub item_id: Option<ItemId>,
    pub subject: ApprovalSubject,
    pub options: Vec<ApprovalDecision>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalKind {
    CommandExecution,
    FileChange,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ApprovalSubject {
    Command {
        reason: Option<BoundedText>,
        command: Option<BoundedText>,
        cwd: Option<BoundedText>,
        network: Option<NetworkApproval>,
    },
    FileChange {
        reason: Option<BoundedText>,
        grant_root: Option<BoundedText>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkApproval {
    pub host: String,
    pub protocol: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    Accept,
    AcceptForSession,
    AcceptWithExecpolicyAmendment,
    Decline,
    Cancel,
    Other(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalEvidence {
    /// `serverRequest/resolved` confirms the request was answered or cleared,
    /// but the provider does not include which decision was taken.
    ServerRequestResolved,
    /// The completed tool item reported a terminal status. A `declined` status
    /// is documented evidence of a decline; other statuses are not proof that
    /// an approval was granted.
    ItemStatus(ToolStatus),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionDenied {
    pub tool_call_id: ItemId,
    pub tool_name: String,
    pub message: BoundedText,
    pub reason: Option<String>,
    pub reason_type: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notice {
    pub severity: NoticeSeverity,
    pub message: BoundedText,
    pub native_type: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeSeverity {
    Info,
    Warning,
    Error,
    Deprecated,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnsupportedAction {
    /// Provider action name, sanitized and bounded. Never includes payload text.
    pub action: String,
    pub reason: String,
    pub scope: UnsupportedScope,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnsupportedScope {
    Provider,
    Version,
    Action,
}

/// Explicit instruction to fall back to the raw terminal stream.
///
/// A fallback never carries structured events. The host may keep displaying
/// already-applied structured state, but it must not synthesize new turns,
/// messages, tool calls or approvals from terminal text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalFallback {
    pub reason: FallbackReason,
    pub detail: String,
    pub structured_events_available: bool,
}

impl TerminalFallback {
    pub fn raw_terminal(reason: FallbackReason, detail: impl Into<String>) -> Self {
        Self {
            reason,
            detail: detail.into(),
            structured_events_available: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FallbackReason {
    UnknownProvider,
    UnsupportedVersion,
    MalformedEvent,
    PayloadTooLarge,
    FieldTooLong,
    MissingField,
    ContractVersionMismatch,
    StreamBufferOverflow,
}
