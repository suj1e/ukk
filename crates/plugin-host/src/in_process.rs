//! Trusted, in-process plugin contributions.
//!
//! Only [`TrustLevel::Builtin`](ukk_plugin_api::TrustLevel::Builtin) plugins may
//! use this path: the code is compiled into a trusted ukk release instead of
//! being loaded from disk. It is still capability-checked and still cannot add
//! contributions beyond its validated manifest.
use std::fmt;

use serde_json::Value;
use ukk_plugin_api::wire::HelloResult;
use ukk_plugin_api::PluginId;

/// Context handed to an in-process command.
#[derive(Debug, Clone, PartialEq)]
pub struct InvokeContext {
    pub plugin_id: PluginId,
    pub command: String,
    pub session_id: Option<String>,
    pub args: Value,
}

/// Structured failure from a plugin command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginFailure {
    pub code: String,
    pub message: String,
}

impl PluginFailure {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for PluginFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "plugin failure `{}`: {}", self.code, self.message)
    }
}

impl std::error::Error for PluginFailure {}

/// A built-in plugin's command surface. Implementations are trusted code, so
/// they run synchronously in the host process and must not block the UI thread;
/// the embedding host calls them off its render path.
pub trait InProcessPlugin: Send + Sync {
    /// Declare what this implementation provides. The supervisor checks the
    /// result against the manifest and rejects any addition.
    fn hello(&self) -> HelloResult;

    /// Run one declared command.
    fn invoke(&self, context: &InvokeContext) -> Result<Value, PluginFailure>;
}

/// Minimal implementation used by tests and by the headless demonstration.
/// It never touches OS resources.
#[derive(Debug, Default)]
pub struct EchoInProcessPlugin {
    plugin_id: String,
}

impl EchoInProcessPlugin {
    pub fn new(plugin_id: impl Into<String>) -> Self {
        Self {
            plugin_id: plugin_id.into(),
        }
    }
}

impl InProcessPlugin for EchoInProcessPlugin {
    fn hello(&self) -> HelloResult {
        HelloResult::new(&self.plugin_id)
    }

    fn invoke(&self, context: &InvokeContext) -> Result<Value, PluginFailure> {
        Ok(serde_json::json!({
            "in_process": true,
            "command": context.command,
            "session_id": context.session_id,
        }))
    }
}
