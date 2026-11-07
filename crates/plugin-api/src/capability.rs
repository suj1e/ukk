//! Brokered capability vocabulary.
//!
//! Declaring a capability in a manifest only describes intent. Nothing here
//! grants anything: the host grant store is the sole authority and starts
//! empty for every plugin.
use std::collections::BTreeSet;
use std::fmt;

/// One brokered capability. Network access carries the target domain so a
/// grant cannot silently widen into "any host".
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Capability {
    WorkspaceRead,
    SessionObserve,
    AgentSend,
    AgentApprove,
    FilesystemRead,
    FilesystemWrite,
    GitWrite,
    ScreenView,
    ScreenControl,
    ScreenAudio,
    NotificationsSend,
    /// `network.connect(<domain>)`; `*.example.com` is the only wildcard form.
    NetworkConnect(String),
}

/// How dangerous a capability is, used by UI and audit decisions. Ordering is
/// intentional: `Sensitive` is the strongest class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RiskLevel {
    /// Read-only observation of ukk state.
    Observe,
    /// Changes user-visible state; requires an explicit grant.
    Mutate,
    /// Touches private OS resources (approvals, screen, input); requires a
    /// fresh, separately approved grant and must never be on by default.
    Sensitive,
}

/// Every capability this build understands. Unknown strings are rejected
/// during manifest validation instead of being ignored.
pub const ALL_CAPABILITIES: &[&str] = &[
    "workspace.read",
    "session.observe",
    "agent.send",
    "agent.approve",
    "filesystem.read",
    "filesystem.write",
    "git.write",
    "screen.view",
    "screen.control",
    "screen.audio",
    "notifications.send",
    "network.connect",
];

impl Capability {
    /// Canonical text form, round-trippable through [`Capability::parse`].
    pub fn to_text(&self) -> String {
        match self {
            Self::WorkspaceRead => "workspace.read".into(),
            Self::SessionObserve => "session.observe".into(),
            Self::AgentSend => "agent.send".into(),
            Self::AgentApprove => "agent.approve".into(),
            Self::FilesystemRead => "filesystem.read".into(),
            Self::FilesystemWrite => "filesystem.write".into(),
            Self::GitWrite => "git.write".into(),
            Self::ScreenView => "screen.view".into(),
            Self::ScreenControl => "screen.control".into(),
            Self::ScreenAudio => "screen.audio".into(),
            Self::NotificationsSend => "notifications.send".into(),
            Self::NetworkConnect(domain) => format!("network.connect({domain})"),
        }
    }

    /// Parse a manifest capability string.
    ///
    /// `network.connect` without a parenthesised domain is rejected rather than
    /// defaulting to "all networks".
    pub fn parse(text: &str) -> Result<Self, CapabilityParseError> {
        let text = text.trim();
        let capability = match text {
            "workspace.read" => Self::WorkspaceRead,
            "session.observe" => Self::SessionObserve,
            "agent.send" => Self::AgentSend,
            "agent.approve" => Self::AgentApprove,
            "filesystem.read" => Self::FilesystemRead,
            "filesystem.write" => Self::FilesystemWrite,
            "git.write" => Self::GitWrite,
            "screen.view" => Self::ScreenView,
            "screen.control" => Self::ScreenControl,
            "screen.audio" => Self::ScreenAudio,
            "notifications.send" => Self::NotificationsSend,
            "network.connect" => {
                return Err(CapabilityParseError::MissingDomain);
            }
            other => {
                let Some(domain) = other
                    .strip_prefix("network.connect(")
                    .and_then(|rest| rest.strip_suffix(')'))
                else {
                    return Err(CapabilityParseError::UnknownName(other.to_owned()));
                };
                if !is_valid_domain(domain) {
                    return Err(CapabilityParseError::InvalidDomain(domain.to_owned()));
                }
                Self::NetworkConnect(domain.to_ascii_lowercase())
            }
        };
        Ok(capability)
    }

    pub fn risk(&self) -> RiskLevel {
        match self {
            Self::WorkspaceRead | Self::SessionObserve => RiskLevel::Observe,
            Self::AgentSend
            | Self::FilesystemRead
            | Self::FilesystemWrite
            | Self::GitWrite
            | Self::NotificationsSend
            | Self::NetworkConnect(_) => RiskLevel::Mutate,
            Self::AgentApprove | Self::ScreenView | Self::ScreenControl | Self::ScreenAudio => {
                RiskLevel::Sensitive
            }
        }
    }

    /// Whether the capability touches private OS resources or user authority.
    pub fn is_sensitive(&self) -> bool {
        self.risk() == RiskLevel::Sensitive
    }
}

impl fmt::Display for Capability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_text())
    }
}

fn is_valid_domain(domain: &str) -> bool {
    let body = domain.strip_prefix("*.").unwrap_or(domain);
    if body.is_empty() || body.len() > 253 {
        return false;
    }
    let mut labels = body.split('.');
    let mut count = 0;
    for label in labels.by_ref() {
        count += 1;
        if label.is_empty() || label.len() > 63 {
            return false;
        }
        if !label
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return false;
        }
        if label.starts_with('-') || label.ends_with('-') {
            return false;
        }
    }
    count >= 2
}

/// Rejection reason for [`Capability::parse`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapabilityParseError {
    UnknownName(String),
    MissingDomain,
    InvalidDomain(String),
}

impl fmt::Display for CapabilityParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownName(name) => write!(f, "unknown capability `{name}`"),
            Self::MissingDomain => {
                f.write_str("`network.connect` requires an explicit domain, e.g. network.connect(api.example.com)")
            }
            Self::InvalidDomain(domain) => write!(f, "invalid network domain `{domain}`"),
        }
    }
}

impl std::error::Error for CapabilityParseError {}

/// Ordered, duplicate-free capability set used for declarations and grants.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CapabilitySet {
    inner: BTreeSet<Capability>,
}

impl CapabilitySet {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a capability, returning `false` when it was already present.
    pub fn insert(&mut self, capability: Capability) -> bool {
        self.inner.insert(capability)
    }

    pub fn contains(&self, capability: &Capability) -> bool {
        self.inner.contains(capability)
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Capability> {
        self.inner.iter()
    }

    /// Whether every capability in `required` is present in `self`.
    pub fn contains_all(&self, required: &CapabilitySet) -> bool {
        required.inner.is_subset(&self.inner)
    }
}

impl FromIterator<Capability> for CapabilitySet {
    fn from_iter<T: IntoIterator<Item = Capability>>(iter: T) -> Self {
        Self {
            inner: iter.into_iter().collect(),
        }
    }
}

impl<'a> IntoIterator for &'a CapabilitySet {
    type Item = &'a Capability;
    type IntoIter = std::collections::btree_set::Iter<'a, Capability>;

    fn into_iter(self) -> Self::IntoIter {
        self.inner.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_every_named_capability() {
        for text in ALL_CAPABILITIES {
            if *text == "network.connect" {
                assert!(Capability::parse(text).is_err());
                continue;
            }
            let capability = Capability::parse(text).unwrap();
            assert_eq!(capability.to_text(), *text);
        }
    }

    #[test]
    fn network_capability_requires_a_domain() {
        assert_eq!(
            Capability::parse("network.connect"),
            Err(CapabilityParseError::MissingDomain)
        );
        assert_eq!(
            Capability::parse("network.connect()").unwrap_err(),
            CapabilityParseError::InvalidDomain(String::new())
        );
        let capability = Capability::parse("network.connect(API.Example.com)").unwrap();
        assert_eq!(
            capability,
            Capability::NetworkConnect("api.example.com".into())
        );
        assert!(Capability::parse("network.connect(*.example.com)").is_ok());
        assert!(Capability::parse("network.connect(*.*)").is_err());
        assert!(Capability::parse("network.connect(example)").is_err());
    }

    #[test]
    fn unknown_capability_is_rejected() {
        assert_eq!(
            Capability::parse("screen.everything").unwrap_err(),
            CapabilityParseError::UnknownName("screen.everything".into())
        );
    }

    #[test]
    fn sensitive_classification_covers_screen_and_approval() {
        for text in [
            "agent.approve",
            "screen.view",
            "screen.control",
            "screen.audio",
        ] {
            assert!(Capability::parse(text).unwrap().is_sensitive(), "{text}");
        }
        for text in ["workspace.read", "session.observe", "agent.send"] {
            assert!(!Capability::parse(text).unwrap().is_sensitive(), "{text}");
        }
    }
}
