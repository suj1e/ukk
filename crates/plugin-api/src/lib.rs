//! Plugin contract for ukk: manifest schema v1, capability declarations,
//! contribution descriptors and the helper wire protocol.
//!
//! This crate is pure data and validation. It never spawns processes, never
//! stores grants and never touches the filesystem beyond reading a manifest
//! that the caller names explicitly. Authorization and life-cycle live in
//! `ukk-plugin-host`.
//!
//! Design rule: **a manifest describes, it never grants**. Every capability is
//! inert until a separate, user-scoped grant exists in the host grant store.
mod capability;
mod error;
mod manifest;
mod version;
pub mod wire;

pub use capability::{
    Capability, CapabilityParseError, CapabilitySet, RiskLevel, ALL_CAPABILITIES,
};
pub use error::{ManifestError, ManifestErrorCode};
pub use manifest::{
    ClientPlatform, CommandContribution, CommandScope, ContributionSlot, EventContribution,
    EventKind, OverlayAnchor, OverlayDescriptor, PluginId, PluginIdError, PluginManifest,
    PluginVersion, PluginVersionError, TrustLevel, UiViewContribution, ValidatedManifest,
};
pub use version::{ApiRange, ApiRangeError, ApiVersion};

/// Schema version of `plugin.toml` that this build understands.
pub const SUPPORTED_MANIFEST_VERSION: u32 = 1;

/// Plugin API version implemented by this build.
pub const HOST_API_VERSION: ApiVersion = ApiVersion::new(0, 1, 0);

/// Version of the helper stdio protocol. Independent of the plugin API version.
pub const HELPER_PROTOCOL_VERSION: u32 = 1;

pub mod limits {
    //! Hard limits applied during manifest validation and helper framing.
    //!
    //! They exist so a hostile plugin cannot force unbounded allocation or
    //! flood the UI with contributions. Values are deliberately small: the
    //! first-party surface is narrow by design.

    /// Maximum number of declared commands per plugin.
    pub const MAX_COMMANDS: usize = 32;
    /// Maximum number of declared UI views per plugin.
    pub const MAX_VIEWS: usize = 32;
    /// Maximum number of declared event subscriptions per plugin.
    pub const MAX_EVENT_SUBSCRIPTIONS: usize = 16;
    /// Maximum number of capability entries in one list (`required`/`optional`).
    pub const MAX_CAPABILITIES: usize = 32;
    /// Maximum length of a plugin id / command id / view id.
    pub const MAX_ID_LEN: usize = 64;
    /// Maximum length of a human-readable title.
    pub const MAX_TITLE_LEN: usize = 80;
    /// Maximum length of a helper entrypoint file name.
    pub const MAX_ENTRYPOINT_LEN: usize = 64;
    /// One JSON frame on the helper stdio protocol, including the newline.
    pub const MAX_FRAME_BYTES: usize = 64 * 1024;
    /// Bounded number of queued host events per plugin before drops are counted.
    pub const MAX_EVENT_QUEUE: usize = 64;
    /// Overlay size bounds used by `session.overlay` descriptors.
    pub const MIN_OVERLAY_EDGE: u32 = 120;
    /// Overlay size bounds used by `session.overlay` descriptors.
    pub const MAX_OVERLAY_EDGE: u32 = 1080;
}
