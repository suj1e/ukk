//! Supervisor errors with stable machine-readable codes.
use std::fmt;

use crate::grant::GrantError;
use crate::helper::HelperError;

/// Every supervisor failure. Codes are stable strings for audit and UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostError {
    UnknownPlugin {
        plugin_id: String,
    },
    DuplicatePlugin {
        plugin_id: String,
    },
    TooManyPlugins {
        limit: usize,
    },
    DuplicateContribution {
        plugin_id: String,
        contribution: String,
    },
    TrustMismatch {
        plugin_id: String,
        detail: String,
    },
    InvalidHelper {
        detail: String,
    },
    UnknownCommand {
        plugin_id: String,
        command: String,
    },
    ForgedCommand {
        plugin_id: String,
        command: String,
        owner: String,
    },
    UnknownView {
        plugin_id: String,
        view: String,
    },
    NotEnabled {
        plugin_id: String,
        state: String,
    },
    Busy {
        plugin_id: String,
    },
    MissingSession {
        plugin_id: String,
        command: String,
    },
    CapabilityDenied {
        plugin_id: String,
        capability: String,
        session_id: Option<String>,
    },
    UndeclaredCapability {
        plugin_id: String,
        capability: String,
    },
    Grant(GrantError),
    Helper(HelperError),
    PluginFailed {
        plugin_id: String,
        code: String,
        message: String,
    },
}

impl HostError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::UnknownPlugin { .. } => "unknown_plugin",
            Self::DuplicatePlugin { .. } => "duplicate_plugin",
            Self::TooManyPlugins { .. } => "too_many_plugins",
            Self::DuplicateContribution { .. } => "duplicate_contribution",
            Self::TrustMismatch { .. } => "trust_mismatch",
            Self::InvalidHelper { .. } => "invalid_helper",
            Self::UnknownCommand { .. } => "unknown_command",
            Self::ForgedCommand { .. } => "forged_command",
            Self::UnknownView { .. } => "unknown_view",
            Self::NotEnabled { .. } => "not_enabled",
            Self::Busy { .. } => "busy",
            Self::MissingSession { .. } => "missing_session",
            Self::CapabilityDenied { .. } => "capability_denied",
            Self::UndeclaredCapability { .. } => "undeclared_capability",
            Self::Grant(_) => "grant_rejected",
            Self::Helper(error) => error.code(),
            Self::PluginFailed { .. } => "plugin_failed",
        }
    }
}

impl fmt::Display for HostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownPlugin { plugin_id } => write!(f, "plugin `{plugin_id}` is not installed"),
            Self::DuplicatePlugin { plugin_id } => {
                write!(f, "plugin `{plugin_id}` is already installed")
            }
            Self::TooManyPlugins { limit } => {
                write!(f, "at most {limit} plugins may be installed")
            }
            Self::DuplicateContribution {
                plugin_id,
                contribution,
            } => write!(
                f,
                "plugin `{plugin_id}` declares `{contribution}`, which another plugin already owns"
            ),
            Self::TrustMismatch { plugin_id, detail } => {
                write!(f, "plugin `{plugin_id}`: {detail}")
            }
            Self::InvalidHelper { detail } => write!(f, "invalid helper: {detail}"),
            Self::UnknownCommand { plugin_id, command } => {
                write!(f, "plugin `{plugin_id}` does not declare command `{command}`")
            }
            Self::ForgedCommand {
                plugin_id,
                command,
                owner,
            } => write!(
                f,
                "command `{command}` belongs to plugin `{owner}`, not `{plugin_id}`"
            ),
            Self::UnknownView { plugin_id, view } => {
                write!(f, "plugin `{plugin_id}` does not declare view `{view}`")
            }
            Self::NotEnabled { plugin_id, state } => {
                write!(f, "plugin `{plugin_id}` is not ready (state: {state})")
            }
            Self::Busy { plugin_id } => write!(f, "plugin `{plugin_id}` is changing state"),
            Self::MissingSession { plugin_id, command } => write!(
                f,
                "command `{command}` of plugin `{plugin_id}` is session-scoped and needs a session id"
            ),
            Self::CapabilityDenied {
                plugin_id,
                capability,
                session_id,
            } => match session_id {
                Some(session) => write!(
                    f,
                    "plugin `{plugin_id}` has no `{capability}` grant for session `{session}`"
                ),
                None => write!(f, "plugin `{plugin_id}` has no `{capability}` grant"),
            },
            Self::UndeclaredCapability {
                plugin_id,
                capability,
            } => write!(
                f,
                "plugin `{plugin_id}` never declared capability `{capability}`"
            ),
            Self::Grant(error) => write!(f, "grant rejected: {error}"),
            Self::Helper(error) => write!(f, "{error}"),
            Self::PluginFailed {
                plugin_id,
                code,
                message,
            } => write!(f, "plugin `{plugin_id}` failed (`{code}`): {message}"),
        }
    }
}

impl std::error::Error for HostError {}
