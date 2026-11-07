//! Provider identity, capability descriptors and version detection.
//!
//! Capabilities are claims about documented provider interfaces. A capability
//! that this crate has not modelled is `Unsupported` or `Unverified`; it is
//! never silently promoted to `Supported`.

use crate::contract::TerminalFallback;
use crate::error::AdapterError;
use crate::ids::SessionId;
use serde::{Deserialize, Serialize};

/// Provider protocols with a fixture adapter in this crate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    CodexAppServer,
    ClaudeStreamJson,
}

impl ProviderKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CodexAppServer => "codex_app_server",
            Self::ClaudeStreamJson => "claude_stream_json",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "codex" | "codex_app_server" | "codex-app-server" => Some(Self::CodexAppServer),
            "claude" | "claude_stream_json" | "claude-stream-json" => Some(Self::ClaudeStreamJson),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Capability {
    Supported,
    Unsupported { reason: String },
    Unverified { note: String },
}

impl Capability {
    pub fn supported() -> Self {
        Self::Supported
    }

    pub fn unsupported(reason: impl Into<String>) -> Self {
        Self::Unsupported {
            reason: reason.into(),
        }
    }

    pub fn unverified(note: impl Into<String>) -> Self {
        Self::Unverified { note: note.into() }
    }

    pub fn is_supported(&self) -> bool {
        matches!(self, Self::Supported)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    pub structured_turns: Capability,
    pub messages: Capability,
    pub tool_calls: Capability,
    pub tool_results: Capability,
    pub progress: Capability,
    pub plans: Capability,
    pub artifacts: Capability,
    pub approvals: Capability,
    pub resumed_sessions: Capability,
    pub partial_streams: Capability,
    /// Raw terminal attachment is a Host capability. It is always available as
    /// the explicit fallback, but it never supplies structured semantics.
    pub terminal_fallback: Capability,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum VersionStatus {
    /// Version string reported by the integration.
    Detected { version: String },
    /// No version string was supplied; the documented schema baseline is used.
    AssumedDocumentedBaseline { note: String },
    /// The declared version is outside this adapter's verified schema.
    Unsupported { declared: String, supported: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityDescriptor {
    pub contract_version: u32,
    pub provider: ProviderKind,
    pub adapter: String,
    pub wire_protocol: String,
    pub version_status: VersionStatus,
    pub capabilities: Capabilities,
}

impl CapabilityDescriptor {
    pub fn codex_app_server_v2(version_status: VersionStatus) -> Self {
        Self {
            contract_version: crate::contract::CONTRACT_VERSION,
            provider: ProviderKind::CodexAppServer,
            adapter: "ukk-agent-adapters/codex-app-server-v2".to_owned(),
            wire_protocol: "codex-app-server-v2".to_owned(),
            version_status,
            capabilities: Capabilities {
                structured_turns: Capability::supported(),
                messages: Capability::supported(),
                tool_calls: Capability::supported(),
                tool_results: Capability::supported(),
                progress: Capability::supported(),
                plans: Capability::supported(),
                artifacts: Capability::supported(),
                approvals: Capability::supported(),
                resumed_sessions: Capability::supported(),
                partial_streams: Capability::supported(),
                terminal_fallback: Capability::supported(),
            },
        }
    }

    pub fn claude_stream_json(version_status: VersionStatus) -> Self {
        Self {
            contract_version: crate::contract::CONTRACT_VERSION,
            provider: ProviderKind::ClaudeStreamJson,
            adapter: "ukk-agent-adapters/claude-stream-json".to_owned(),
            wire_protocol: "claude-stream-json".to_owned(),
            version_status,
            capabilities: Capabilities {
                structured_turns: Capability::supported(),
                messages: Capability::supported(),
                tool_calls: Capability::supported(),
                tool_results: Capability::supported(),
                progress: Capability::supported(),
                plans: Capability::unsupported(
                    "Claude stream-json does not document a plan event; plan text is not synthesized",
                ),
                artifacts: Capability::unsupported(
                    "Claude stream-json does not expose structured file artifacts; tool input is not reclassified as an artifact",
                ),
                approvals: Capability::unsupported(
                    "Claude stream-json exposes permission denials, not approval requests; approval prompts are an in-process SDK control surface not modelled here",
                ),
                resumed_sessions: Capability::supported(),
                partial_streams: Capability::supported(),
                terminal_fallback: Capability::supported(),
            },
        }
    }
}

/// What the integration observed while connecting to a provider.
#[derive(Clone, Debug, Default)]
pub struct ProviderHandshake {
    /// Provider version string, if the provider reports one.
    pub declared_version: Option<String>,
    /// Wire generation explicitly identified by the integration, for example
    /// `v2` for Codex app-server or `stream-json` for Claude.
    pub wire_version: Option<String>,
    /// Documented methods or top-level message types observed.
    pub observed_methods: Vec<String>,
    /// Capability tokens reported by the provider, when documented.
    pub observed_capabilities: Vec<String>,
}

impl ProviderHandshake {
    pub fn has_method(&self, method: &str) -> bool {
        self.observed_methods.iter().any(|known| known == method)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DetectionError {
    UnknownProvider {
        provider: String,
    },
    UnsupportedVersion {
        provider: ProviderKind,
        version: String,
        supported: String,
    },
    MissingMethod {
        provider: ProviderKind,
        method: String,
    },
}

impl DetectionError {
    fn into_adapter_error(self) -> AdapterError {
        match self {
            Self::UnknownProvider { provider } => AdapterError::UnsupportedProvider { provider },
            Self::UnsupportedVersion {
                provider,
                version,
                supported,
            } => AdapterError::UnsupportedProviderVersion {
                provider: provider.as_str().to_owned(),
                version,
                supported,
            },
            Self::MissingMethod { provider, method } => AdapterError::MissingCapability {
                provider: provider.as_str().to_owned(),
                method,
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DetectionFailure {
    pub error: DetectionError,
    pub fallback: TerminalFallback,
}

impl DetectionFailure {
    pub fn new(error: DetectionError) -> Self {
        let fallback = error.clone().into_adapter_error().fallback();
        Self { error, fallback }
    }
}

impl std::fmt::Display for DetectionFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.fallback.detail)
    }
}

impl std::error::Error for DetectionFailure {}

/// Parses a `MAJOR.MINOR.PATCH` prefix and returns the major number.
pub(crate) fn major_version(version: &str) -> Option<u64> {
    version
        .trim_start_matches('v')
        .split('.')
        .next()
        .and_then(|major| major.parse().ok())
}

pub(crate) fn claude_version_status(handshake: &ProviderHandshake) -> VersionStatus {
    match handshake.declared_version.as_deref() {
        Some(version) => VersionStatus::Detected {
            version: version.to_owned(),
        },
        None => VersionStatus::AssumedDocumentedBaseline {
            note: "Claude stream-json documented baseline; no version string supplied".to_owned(),
        },
    }
}

pub(crate) fn codex_version_status(handshake: &ProviderHandshake) -> VersionStatus {
    match handshake.declared_version.as_deref() {
        Some(version) => VersionStatus::Detected {
            version: version.to_owned(),
        },
        None => VersionStatus::AssumedDocumentedBaseline {
            note: "Codex app-server v2 documented baseline; no version string supplied".to_owned(),
        },
    }
}

/// Session id the integration intends to resume, if any.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResumeHint {
    pub session_id: Option<SessionId>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_names_round_trip() {
        assert_eq!(
            ProviderKind::parse("codex_app_server"),
            Some(ProviderKind::CodexAppServer)
        );
        assert_eq!(
            ProviderKind::parse("claude-stream-json"),
            Some(ProviderKind::ClaudeStreamJson)
        );
        assert_eq!(ProviderKind::parse("unknown"), None);
    }

    #[test]
    fn major_version_parses_prefix() {
        assert_eq!(major_version("2.1.191"), Some(2));
        assert_eq!(major_version("v2"), Some(2));
        assert_eq!(major_version("invalid"), None);
    }
}
