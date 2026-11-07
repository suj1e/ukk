//! Server-side capability scopes.
//!
//! Scope strings are a stable wire contract shared with the pairing flow and
//! the iOS client. Grants are additive; holding one scope never implies
//! another. Unknown scope strings fail to deserialize rather than being
//! ignored, so a typo or a downgrade attempt cannot silently widen a grant.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fmt;

/// Wire strings for every grantable scope. Transports and pairing code should
/// use these constants instead of duplicating literals.
pub mod scopes {
    /// Read Sessions, agents and their durable events.
    pub const SESSION_OBSERVE: &str = "session.observe";
    /// Create or archive Sessions and Spaces.
    pub const SESSION_MANAGE: &str = "session.manage";
    /// Dispatch text to an Agent run.
    pub const AGENT_SEND: &str = "agent.send";
    /// Answer a structured approval request.
    pub const APPROVAL_RESPOND: &str = "approval.respond";
    /// See that a remote screen is available.
    pub const SCREEN_VIEW: &str = "screen.view";
    /// Inject pointer or keyboard input behind a controller lease.
    pub const SCREEN_CONTROL: &str = "screen.control";
    /// Invoke an installed first-party plugin command.
    pub const PLUGIN_INVOKE: &str = "plugin.invoke";
    /// Pair, rotate or revoke devices sharing this Host.
    pub const DEVICE_MANAGE: &str = "device.manage";

    /// Every scope currently defined by this generation.
    pub const ALL: &[&str] = &[
        SESSION_OBSERVE,
        SESSION_MANAGE,
        AGENT_SEND,
        APPROVAL_RESPOND,
        SCREEN_VIEW,
        SCREEN_CONTROL,
        PLUGIN_INVOKE,
        DEVICE_MANAGE,
    ];
}

/// One capability a device may be granted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Scope {
    #[serde(rename = "session.observe")]
    SessionObserve,
    #[serde(rename = "session.manage")]
    SessionManage,
    #[serde(rename = "agent.send")]
    AgentSend,
    #[serde(rename = "approval.respond")]
    ApprovalRespond,
    #[serde(rename = "screen.view")]
    ScreenView,
    #[serde(rename = "screen.control")]
    ScreenControl,
    #[serde(rename = "plugin.invoke")]
    PluginInvoke,
    #[serde(rename = "device.manage")]
    DeviceManage,
}

impl Scope {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SessionObserve => scopes::SESSION_OBSERVE,
            Self::SessionManage => scopes::SESSION_MANAGE,
            Self::AgentSend => scopes::AGENT_SEND,
            Self::ApprovalRespond => scopes::APPROVAL_RESPOND,
            Self::ScreenView => scopes::SCREEN_VIEW,
            Self::ScreenControl => scopes::SCREEN_CONTROL,
            Self::PluginInvoke => scopes::PLUGIN_INVOKE,
            Self::DeviceManage => scopes::DEVICE_MANAGE,
        }
    }

    /// Parses exactly one known scope string. Unknown values are rejected.
    #[must_use]
    pub fn from_wire(value: &str) -> Option<Self> {
        match value {
            scopes::SESSION_OBSERVE => Some(Self::SessionObserve),
            scopes::SESSION_MANAGE => Some(Self::SessionManage),
            scopes::AGENT_SEND => Some(Self::AgentSend),
            scopes::APPROVAL_RESPOND => Some(Self::ApprovalRespond),
            scopes::SCREEN_VIEW => Some(Self::ScreenView),
            scopes::SCREEN_CONTROL => Some(Self::ScreenControl),
            scopes::PLUGIN_INVOKE => Some(Self::PluginInvoke),
            scopes::DEVICE_MANAGE => Some(Self::DeviceManage),
            _ => None,
        }
    }

    /// Every defined scope, in wire-string order.
    pub fn all() -> impl Iterator<Item = Self> {
        [
            Self::SessionObserve,
            Self::SessionManage,
            Self::AgentSend,
            Self::ApprovalRespond,
            Self::ScreenView,
            Self::ScreenControl,
            Self::PluginInvoke,
            Self::DeviceManage,
        ]
        .into_iter()
    }
}

impl fmt::Display for Scope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Set of scopes held by a peer. An empty set grants no capability.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ScopeSet(BTreeSet<Scope>);

impl ScopeSet {
    #[must_use]
    pub fn empty() -> Self {
        Self(BTreeSet::new())
    }

    pub fn insert(&mut self, scope: Scope) {
        self.0.insert(scope);
    }

    #[must_use]
    pub fn contains(&self, scope: Scope) -> bool {
        self.0.contains(&scope)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = Scope> + '_ {
        self.0.iter().copied()
    }
}

impl FromIterator<Scope> for ScopeSet {
    fn from_iter<T: IntoIterator<Item = Scope>>(iter: T) -> Self {
        Self(iter.into_iter().collect())
    }
}
