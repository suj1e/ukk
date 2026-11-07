//! Requests and reports exchanged with UI and Host adapters.
use serde_json::Value;

use ukk_plugin_api::{EventKind, PluginId};

/// A host event offered to subscribed plugins.
#[derive(Debug, Clone)]
pub struct HostEvent {
    pub kind: EventKind,
    pub session_id: Option<String>,
    pub payload: Value,
}

impl HostEvent {
    pub fn new(kind: EventKind, payload: Value) -> Self {
        Self {
            kind,
            session_id: None,
            payload,
        }
    }

    pub fn in_session(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }
}

/// Outcome of offering one event to all subscribed plugins.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PublishReport {
    /// Frames queued for delivery.
    pub delivered: usize,
    /// Frames dropped because a plugin's bounded queue was full.
    pub dropped: usize,
    /// Plugins that subscribe but are not authorized or not ready.
    pub skipped: usize,
}

/// A command dispatch request from a UI or host adapter.
#[derive(Debug, Clone, PartialEq)]
pub struct InvokeRequest {
    pub plugin_id: PluginId,
    pub command: String,
    pub session_id: Option<String>,
    pub args: Value,
}

impl InvokeRequest {
    pub fn new(plugin_id: PluginId, command: impl Into<String>) -> Self {
        Self {
            plugin_id,
            command: command.into(),
            session_id: None,
            args: Value::Null,
        }
    }

    pub fn in_session(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    pub fn with_args(mut self, args: Value) -> Self {
        self.args = args;
        self
    }
}

/// Successful command result.
#[derive(Debug, Clone, PartialEq)]
pub struct InvokeOutcome {
    pub plugin_id: PluginId,
    pub command: String,
    pub value: Value,
}
