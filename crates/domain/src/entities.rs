//! Durable Workspace and AgentRun records.
use serde::{Deserialize, Serialize};
use crate::SessionLayout;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Space {
    pub id: String,
    pub title: String,
    pub project_root: String,
    pub created_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub space_id: String,
    pub title: String,
    pub archived: bool,
    pub created_ms: i64,
    pub updated_ms: i64,
}

/// Stable product binding to an existing herdr-core process. A terminal
/// address is unique *only within a particular herdr-core identity*.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRun {
    pub id: String,
    pub session_id: String,
    pub core_id: String,
    pub terminal_address: String,
    pub parent_run_id: Option<String>,
    pub adapter: String,
    pub created_ms: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DomainSnapshot {
    pub spaces: Vec<Space>,
    pub sessions: Vec<Session>,
    pub agent_runs: Vec<AgentRun>,
    pub layouts: Vec<SessionLayout>,
}
