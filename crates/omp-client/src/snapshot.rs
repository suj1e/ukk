//! Bounded conversation snapshot shared by the Host RPC and the Desktop.
//!
//! These types are the wire contract for `omp.snapshot` / `omp.wait`. They
//! contain only information derived from omp's documented RPC frames; nothing
//! is synthesized from terminal text.

use serde::{Deserialize, Serialize};

/// Provenance label shown by every surface that renders this snapshot.
pub const OMP_PROVENANCE: &str = "omp RPC · local";
/// Wire protocol version this snapshot was produced under.
pub const OMP_RPC_PROTOCOL_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OmpTimelineKind {
    User,
    Assistant,
    Thinking,
    Tool,
    Notice,
    Error,
    Status,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OmpTimelineEntry {
    /// Monotonic sequence assigned by the Host when the entry was created.
    pub seq: u64,
    /// Unix milliseconds, Host clock.
    pub at_ms: i64,
    pub kind: OmpTimelineKind,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default)]
    pub is_error: bool,
    /// The Host truncated this entry to its per-entry byte budget.
    #[serde(default)]
    pub truncated: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OmpTurnState {
    #[default]
    Idle,
    Running,
    Aborting,
    Complete,
    Error,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct OmpTurn {
    pub state: OmpTurnState,
    /// `Some(false)` only when omp explicitly reported a local command.
    pub agent_invoked: Option<bool>,
    pub started_ms: Option<i64>,
    pub finished_ms: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OmpExit {
    pub code: Option<i32>,
    pub detail: String,
}

/// One Host-owned omp session.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OmpSnapshot {
    pub session_id: String,
    pub cwd: String,
    /// `running`, `exited`, or `stopped`.
    pub state: String,
    pub protocol_version: u32,
    pub provenance: String,
    /// Bumps on every timeline or turn change; used by `omp.wait`.
    pub generation: u64,
    /// Last omp event sequence number folded into the timeline.
    pub cursor: u64,
    pub turn: OmpTurn,
    pub timeline: Vec<OmpTimelineEntry>,
    /// Older entries were discarded to keep the timeline bounded.
    pub truncated: bool,
    pub unknown_frames: u64,
    pub stderr_tail: Vec<String>,
    pub exit: Option<OmpExit>,
    pub error: Option<String>,
    pub restart_note: String,
}

impl OmpSnapshot {
    pub fn is_running(&self) -> bool {
        self.state == "running"
    }
}
