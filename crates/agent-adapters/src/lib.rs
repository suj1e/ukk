//! Versioned, provider-neutral structured Agent adapters.
//!
//! The crate turns documented provider event lines into one contract. It is
//! deliberately fixture-driven and side-effect free: it does not spawn agents,
//! perform RPC, inspect terminal text or own process lifetime. A Host
//! integration supplies transport lines and applies the emitted events with
//! [`merge::MergeEngine`].
//!
//! See `docs/AGENT_ADAPTERS.md` for the provider/version matrix and the list of
//! semantics that are intentionally Unsupported or NOT VERIFIED.

pub mod adapter;
pub mod capability;
pub mod contract;
pub mod error;
pub mod fixtures;
pub mod ids;
pub mod limits;
pub mod merge;
pub mod providers;
pub mod redact;
mod util;

pub use adapter::{AdapterConfig, ParseOutcome, ProviderAdapter};
pub use capability::{
    Capabilities, Capability, CapabilityDescriptor, DetectionError, DetectionFailure,
    ProviderHandshake, ProviderKind, VersionStatus,
};
pub use contract::{
    AdapterEvent, ApprovalDecision, ApprovalEvidence, ApprovalKind, ApprovalRequest,
    ApprovalSubject, Artifact, ArtifactKind, BoundedText, EventPayload, FallbackReason, Message,
    MessagePart, Notice, NoticeSeverity, Plan, PlanStep, PlanStepStatus, Progress, ProgressKind,
    Provenance, ProviderError, Role, SessionInfo, SessionStatus, TerminalFallback, ToolCall,
    ToolInput, ToolKind, ToolStatus, TurnInfo, TurnStatus, UnsupportedAction, UnsupportedScope,
    CONTRACT_VERSION,
};
pub use error::AdapterError;
pub use ids::{ApprovalId, ItemId, RequestId, SessionId, StreamId, TurnId};
pub use limits::Limits;
pub use merge::{
    ApprovalClearance, ApprovalView, ConversationState, IngestOutcome, MergeConflict, MergeEngine,
    MergeOutcome, PlanTextState, SessionView, TurnView,
};
pub use providers::{
    adapter_for, detect_adapter, ClaudeStreamJsonAdapter, CodexAppServerAdapter,
    CLAUDE_REQUIRED_METHODS, CODEX_REQUIRED_METHODS,
};
pub use redact::{NoopRedactor, Redactor, RuleRedactor};
