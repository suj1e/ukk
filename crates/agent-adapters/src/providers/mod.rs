//! Provider adapters and capability detection.

mod claude;
mod codex;

pub use claude::ClaudeStreamJsonAdapter;
pub use codex::CodexAppServerAdapter;

use crate::adapter::{AdapterConfig, ProviderAdapter};
use crate::capability::{
    claude_version_status, codex_version_status, major_version, CapabilityDescriptor,
    DetectionError, DetectionFailure, ProviderHandshake, ProviderKind, VersionStatus,
};

pub const CODEX_REQUIRED_METHODS: &[&str] = &[
    "thread/start",
    "turn/start",
    "item/started",
    "item/completed",
];

pub const CLAUDE_REQUIRED_METHODS: &[&str] = &["system", "assistant", "user", "result"];

pub fn detect_adapter(
    provider: &str,
    handshake: &ProviderHandshake,
    config: AdapterConfig,
) -> Result<Box<dyn ProviderAdapter>, DetectionFailure> {
    let kind = ProviderKind::parse(provider).ok_or_else(|| {
        DetectionFailure::new(DetectionError::UnknownProvider {
            provider: provider.to_owned(),
        })
    })?;

    match kind {
        ProviderKind::CodexAppServer => {
            if let Some(wire_version) = handshake.wire_version.as_deref() {
                if !matches!(wire_version, "v2" | "2") {
                    return Err(DetectionFailure::new(DetectionError::UnsupportedVersion {
                        provider: kind,
                        version: wire_version.to_owned(),
                        supported: "codex app-server wire v2".to_owned(),
                    }));
                }
            }
            for method in CODEX_REQUIRED_METHODS {
                if !handshake.has_method(method) {
                    return Err(DetectionFailure::new(DetectionError::MissingMethod {
                        provider: kind,
                        method: (*method).to_owned(),
                    }));
                }
            }
            let descriptor =
                CapabilityDescriptor::codex_app_server_v2(codex_version_status(handshake));
            Ok(Box::new(CodexAppServerAdapter::new(config, descriptor)))
        }
        ProviderKind::ClaudeStreamJson => {
            if let Some(version) = handshake.declared_version.as_deref() {
                match major_version(version) {
                    Some(2) => {}
                    _ => {
                        return Err(DetectionFailure::new(DetectionError::UnsupportedVersion {
                            provider: kind,
                            version: version.to_owned(),
                            supported: "Claude stream-json 2.x documented schema".to_owned(),
                        }));
                    }
                }
            } else if let Some(wire_version) = handshake.wire_version.as_deref() {
                if wire_version != "stream-json" {
                    return Err(DetectionFailure::new(DetectionError::UnsupportedVersion {
                        provider: kind,
                        version: wire_version.to_owned(),
                        supported: "Claude stream-json".to_owned(),
                    }));
                }
            }
            for method in CLAUDE_REQUIRED_METHODS {
                if !handshake.has_method(method) {
                    return Err(DetectionFailure::new(DetectionError::MissingMethod {
                        provider: kind,
                        method: (*method).to_owned(),
                    }));
                }
            }
            let descriptor =
                CapabilityDescriptor::claude_stream_json(claude_version_status(handshake));
            Ok(Box::new(ClaudeStreamJsonAdapter::new(config, descriptor)))
        }
    }
}

/// Construct an adapter with the documented baseline descriptor.
///
/// Callers that have a provider handshake should use [`detect_adapter`] so an
/// unsupported version degrades explicitly instead of assuming compatibility.
pub fn adapter_for(kind: ProviderKind, config: AdapterConfig) -> Box<dyn ProviderAdapter> {
    match kind {
        ProviderKind::CodexAppServer => Box::new(CodexAppServerAdapter::new(
            config,
            CapabilityDescriptor::codex_app_server_v2(VersionStatus::AssumedDocumentedBaseline {
                note: "Codex app-server v2 documented baseline; no handshake supplied".to_owned(),
            }),
        )),
        ProviderKind::ClaudeStreamJson => Box::new(ClaudeStreamJsonAdapter::new(
            config,
            CapabilityDescriptor::claude_stream_json(VersionStatus::AssumedDocumentedBaseline {
                note: "Claude stream-json documented baseline; no handshake supplied".to_owned(),
            }),
        )),
    }
}
