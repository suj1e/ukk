//! Structured manifest validation errors.
//!
//! Every failure carries a stable [`ManifestErrorCode`] so tests, audit records
//! and UI can branch on the reason instead of matching English text.
use std::fmt;

/// Stable reason code for a rejected manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ManifestErrorCode {
    /// The manifest file could not be read.
    Unreadable,
    /// The document is not valid TOML or contains unknown fields.
    InvalidToml,
    /// `manifest_version` is not the schema version this build supports.
    UnsupportedManifestVersion,
    /// A required field is missing.
    MissingField,
    /// `id` is not a valid plugin identifier.
    InvalidId,
    /// `version` is not a valid plugin version.
    InvalidVersion,
    /// `api` is not a parseable range.
    InvalidApiRange,
    /// `api` excludes the host's API version.
    IncompatibleApi,
    /// `trust` is not a known trust level or is not enabled in this build.
    UnknownTrustLevel,
    /// A helper entrypoint is missing, or present for an in-process plugin.
    EntrypointInvalid,
    /// `[capabilities]` names a capability this build does not know.
    UnknownCapability,
    /// The same capability is declared twice.
    DuplicateCapability,
    /// A command requires a capability the manifest never declared.
    UndeclaredCapability,
    /// A view slot is not a known slot.
    UnknownSlot,
    /// A command scope is not a known scope.
    UnknownScope,
    /// An event subscription names a host event kind this build does not emit.
    UnknownEventKind,
    /// A view targets a client platform this build does not render.
    UnknownPlatform,
    /// Two commands share an id.
    DuplicateCommand,
    /// Two views share an id.
    DuplicateView,
    /// Two event subscriptions share an id.
    DuplicateEventSubscription,
    /// One subscription lists the same event kind twice.
    DuplicateEventKind,
    /// A view binds to a command that the manifest does not declare.
    UnknownCommandReference,
    /// A session-scoped view slot binds a command with a different scope.
    CommandScopeMismatch,
    /// A `session.overlay` view has no overlay descriptor.
    OverlayMissing,
    /// A non-overlay view carries an overlay descriptor.
    OverlayNotAllowed,
    /// Overlay size or anchor is outside the allowed range.
    OverlayBounds,
    /// An event subscription declares no kinds.
    EmptyEventKinds,
    /// A contribution list exceeds its documented limit.
    LimitExceeded,
    /// A contribution id is malformed.
    InvalidContributionId,
    /// A contribution title is empty or too long.
    InvalidTitle,
}

impl ManifestErrorCode {
    /// Machine-readable, snake_case code used in audit records and tests.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unreadable => "unreadable",
            Self::InvalidToml => "invalid_toml",
            Self::UnsupportedManifestVersion => "unsupported_manifest_version",
            Self::MissingField => "missing_field",
            Self::InvalidId => "invalid_id",
            Self::InvalidVersion => "invalid_version",
            Self::InvalidApiRange => "invalid_api_range",
            Self::IncompatibleApi => "incompatible_api",
            Self::UnknownTrustLevel => "unknown_trust_level",
            Self::EntrypointInvalid => "entrypoint_invalid",
            Self::UnknownCapability => "unknown_capability",
            Self::DuplicateCapability => "duplicate_capability",
            Self::UndeclaredCapability => "undeclared_capability",
            Self::UnknownSlot => "unknown_slot",
            Self::UnknownScope => "unknown_scope",
            Self::UnknownEventKind => "unknown_event_kind",
            Self::UnknownPlatform => "unknown_platform",
            Self::DuplicateCommand => "duplicate_command",
            Self::DuplicateView => "duplicate_view",
            Self::DuplicateEventSubscription => "duplicate_event_subscription",
            Self::DuplicateEventKind => "duplicate_event_kind",
            Self::UnknownCommandReference => "unknown_command_reference",
            Self::CommandScopeMismatch => "command_scope_mismatch",
            Self::OverlayMissing => "overlay_missing",
            Self::OverlayNotAllowed => "overlay_not_allowed",
            Self::OverlayBounds => "overlay_bounds",
            Self::EmptyEventKinds => "empty_event_kinds",
            Self::LimitExceeded => "limit_exceeded",
            Self::InvalidContributionId => "invalid_contribution_id",
            Self::InvalidTitle => "invalid_title",
        }
    }
}

impl fmt::Display for ManifestErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A rejected manifest: code plus redaction-safe detail.
///
/// Detail never includes file contents or user paths beyond the manifest the
/// caller already supplied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestError {
    code: ManifestErrorCode,
    detail: String,
    plugin_id: Option<String>,
}

impl ManifestError {
    pub fn new(code: ManifestErrorCode, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: detail.into(),
            plugin_id: None,
        }
    }

    /// Attach the plugin id when it was parseable, for audit correlation.
    pub fn with_plugin_id(mut self, id: impl Into<String>) -> Self {
        self.plugin_id = Some(id.into());
        self
    }

    pub fn code(&self) -> ManifestErrorCode {
        self.code
    }

    pub fn detail(&self) -> &str {
        &self.detail
    }

    pub fn plugin_id(&self) -> Option<&str> {
        self.plugin_id.as_deref()
    }
}

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "manifest {}: {}", self.code, self.detail)?;
        if let Some(id) = &self.plugin_id {
            write!(f, " (plugin `{id}`)")?;
        }
        Ok(())
    }
}

impl std::error::Error for ManifestError {}

impl From<toml::de::Error> for ManifestError {
    fn from(error: toml::de::Error) -> Self {
        let message = error.message().to_owned();
        // serde_toml reports absent required fields; keep that distinguishable
        // from a genuinely malformed document.
        let code = if message.starts_with("missing field") {
            ManifestErrorCode::MissingField
        } else {
            ManifestErrorCode::InvalidToml
        };
        Self::new(code, message)
    }
}
