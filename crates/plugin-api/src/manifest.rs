//! `plugin.toml` schema v1: parsing, validation and typed contribution
//! descriptors.
//!
//! Parsing is strict: unknown fields, unknown capability names, unknown slots
//! and unknown event kinds are rejected rather than silently ignored. A
//! manifest can only become a [`ValidatedManifest`] after every check passes,
//! and the host supervisor accepts nothing else.
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::Path;
use std::str::FromStr;

use serde::Deserialize;

use crate::capability::{Capability, CapabilitySet};
use crate::error::{ManifestError, ManifestErrorCode};
use crate::limits;
use crate::version::ApiRange;
use crate::{HOST_API_VERSION, SUPPORTED_MANIFEST_VERSION};

// ---------------------------------------------------------------------------
// Identifiers
// ---------------------------------------------------------------------------

/// Reverse-DNS style plugin identity, e.g. `dev.ukk.sample.session-action`.
///
/// Deliberately narrow: lowercase ASCII, digits, `-` and `.`; at least two
/// dot-separated components; no path separators, no whitespace, no unicode.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PluginId(String);

/// Why a [`PluginId`] was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginIdError(pub String);

impl fmt::Display for PluginIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PluginIdError {}

impl PluginId {
    pub fn parse(text: &str) -> Result<Self, PluginIdError> {
        if text.is_empty() {
            return Err(PluginIdError("plugin id is empty".into()));
        }
        if text.len() > limits::MAX_ID_LEN {
            return Err(PluginIdError(format!(
                "plugin id is longer than {} characters",
                limits::MAX_ID_LEN
            )));
        }
        let components: Vec<&str> = text.split('.').collect();
        if components.len() < 2 {
            return Err(PluginIdError(
                "plugin id needs at least two dot-separated components".into(),
            ));
        }
        for component in &components {
            if component.is_empty() {
                return Err(PluginIdError(
                    "plugin id contains an empty component".into(),
                ));
            }
            if component.len() > 32 {
                return Err(PluginIdError(
                    "plugin id component is longer than 32 characters".into(),
                ));
            }
            if !component
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
            {
                return Err(PluginIdError(format!(
                    "plugin id component `{component}` may only contain lowercase ASCII letters, digits and '-'"
                )));
            }
            let first = component.as_bytes()[0];
            let last = component.as_bytes()[component.len() - 1];
            if first == b'-' || last == b'-' {
                return Err(PluginIdError(format!(
                    "plugin id component `{component}` may not start or end with '-'"
                )));
            }
        }
        Ok(Self(text.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PluginId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for PluginId {
    type Err = PluginIdError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::parse(text)
    }
}

/// Plugin package version: `major.minor.patch`, all numeric.
///
/// Pre-release and build metadata are intentionally unsupported in schema v1 so
/// that ordering and compatibility checks stay unambiguous.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PluginVersion {
    major: u32,
    minor: u32,
    patch: u32,
}

/// Why a [`PluginVersion`] was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginVersionError(pub String);

impl fmt::Display for PluginVersionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PluginVersionError {}

impl PluginVersion {
    pub fn parse(text: &str) -> Result<Self, PluginVersionError> {
        let core = text.trim();
        if core.is_empty() {
            return Err(PluginVersionError("plugin version is empty".into()));
        }
        if core.contains('-') || core.contains('+') {
            return Err(PluginVersionError(
                "plugin version must be numeric major.minor.patch in schema v1".into(),
            ));
        }
        let mut parts = core.split('.');
        let mut next = || -> Result<u32, PluginVersionError> {
            let part = parts.next().ok_or_else(|| {
                PluginVersionError(format!("plugin version `{core}` needs major.minor.patch"))
            })?;
            if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(PluginVersionError(format!(
                    "plugin version component `{part}` is not numeric"
                )));
            }
            part.parse::<u32>().map_err(|_| {
                PluginVersionError(format!("plugin version component `{part}` overflows u32"))
            })
        };
        let major = next()?;
        let minor = next()?;
        let patch = next()?;
        if parts.next().is_some() {
            return Err(PluginVersionError(format!(
                "plugin version `{core}` has too many components"
            )));
        }
        Ok(Self {
            major,
            minor,
            patch,
        })
    }

    pub fn major(&self) -> u32 {
        self.major
    }

    pub fn minor(&self) -> u32 {
        self.minor
    }

    pub fn patch(&self) -> u32 {
        self.patch
    }
}

impl fmt::Display for PluginVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

impl FromStr for PluginVersion {
    type Err = PluginVersionError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::parse(text)
    }
}

// ---------------------------------------------------------------------------
// Trust
// ---------------------------------------------------------------------------

/// Where a plugin's code runs and how much the host trusts it.
///
/// Only [`TrustLevel::Builtin`] may run in the host process. Everything else is
/// a supervised child process with a restricted environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TrustLevel {
    /// Compiled into a trusted ukk release; runs in-process, still capability-checked.
    Builtin,
    /// First-party helper process with explicit entitlements, spawned separately.
    FirstParty,
    /// Third-party local helper process, no special entitlements.
    ThirdParty,
    /// Declared sandboxed WASM runtime; **not implemented** in this build.
    SandboxedWasm,
}

impl TrustLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Builtin => "builtin",
            Self::FirstParty => "first-party",
            Self::ThirdParty => "third-party",
            Self::SandboxedWasm => "sandboxed-wasm",
        }
    }

    pub fn parse(text: &str) -> Result<Self, ManifestError> {
        match text {
            "builtin" => Ok(Self::Builtin),
            "first-party" => Ok(Self::FirstParty),
            "third-party" => Ok(Self::ThirdParty),
            "sandboxed-wasm" => Err(ManifestError::new(
                ManifestErrorCode::UnknownTrustLevel,
                "trust level `sandboxed-wasm` is declared in the schema but no WASM runtime ships in this build",
            )),
            other => Err(ManifestError::new(
                ManifestErrorCode::UnknownTrustLevel,
                format!("unknown trust level `{other}`"),
            )),
        }
    }

    /// Whether the contribution runs inside the privileged host process.
    pub fn runs_in_process(self) -> bool {
        matches!(self, Self::Builtin)
    }

    /// Whether this trust level requires an external helper executable.
    pub fn requires_helper(self) -> bool {
        !self.runs_in_process()
    }
}

impl fmt::Display for TrustLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------
// Contribution descriptors
// ---------------------------------------------------------------------------

/// Where in the native UI a plugin may contribute a view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContributionSlot {
    /// A row in the Session action menu; must bind to a session-scoped command.
    SessionAction,
    /// A tab in the Session pane area.
    SessionTab,
    /// A floating overlay above the Session, with bounded size.
    SessionOverlay,
    /// A read-only panel in the Session context area.
    SessionContextPanel,
    /// A section inside settings.
    SettingsSection,
}

impl ContributionSlot {
    /// Slots that render inside one Session and therefore require a
    /// session-scoped command when they bind one.
    pub fn is_session_scoped(self) -> bool {
        matches!(
            self,
            Self::SessionAction
                | Self::SessionTab
                | Self::SessionOverlay
                | Self::SessionContextPanel
        )
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::SessionAction => "session.action",
            Self::SessionTab => "session.tab",
            Self::SessionOverlay => "session.overlay",
            Self::SessionContextPanel => "session.context-panel",
            Self::SettingsSection => "settings.section",
        }
    }

    fn parse(text: &str) -> Result<Self, ManifestError> {
        match text {
            "session.action" => Ok(Self::SessionAction),
            "session.tab" => Ok(Self::SessionTab),
            "session.overlay" => Ok(Self::SessionOverlay),
            "session.context-panel" => Ok(Self::SessionContextPanel),
            "settings.section" => Ok(Self::SettingsSection),
            other => Err(ManifestError::new(
                ManifestErrorCode::UnknownSlot,
                format!("unknown view slot `{other}`"),
            )),
        }
    }
}

impl fmt::Display for ContributionSlot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Client platform a view was designed for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ClientPlatform {
    Desktop,
    Ios,
}

impl ClientPlatform {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Desktop => "desktop",
            Self::Ios => "ios",
        }
    }

    fn parse(text: &str) -> Result<Self, ManifestError> {
        match text {
            "desktop" => Ok(Self::Desktop),
            "ios" => Ok(Self::Ios),
            other => Err(ManifestError::new(
                ManifestErrorCode::UnknownPlatform,
                format!("unknown client platform `{other}`"),
            )),
        }
    }
}

impl fmt::Display for ClientPlatform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Command scope. Plugins cannot declare host-global mutations in schema v1;
/// `Global` is only usable for read-only, session-independent commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CommandScope {
    Session,
    Workspace,
    Global,
}

impl CommandScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Workspace => "workspace",
            Self::Global => "global",
        }
    }

    fn parse(text: &str) -> Result<Self, ManifestError> {
        match text {
            "session" => Ok(Self::Session),
            "workspace" => Ok(Self::Workspace),
            "global" => Ok(Self::Global),
            other => Err(ManifestError::new(
                ManifestErrorCode::UnknownScope,
                format!("unknown command scope `{other}`"),
            )),
        }
    }
}

impl fmt::Display for CommandScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Where a floating overlay is anchored inside the Session view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OverlayAnchor {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
    Center,
}

impl OverlayAnchor {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TopLeft => "top-left",
            Self::TopRight => "top-right",
            Self::BottomLeft => "bottom-left",
            Self::BottomRight => "bottom-right",
            Self::Center => "center",
        }
    }

    fn parse(text: &str) -> Result<Self, ManifestError> {
        match text {
            "top-left" => Ok(Self::TopLeft),
            "top-right" => Ok(Self::TopRight),
            "bottom-left" => Ok(Self::BottomLeft),
            "bottom-right" => Ok(Self::BottomRight),
            "center" => Ok(Self::Center),
            other => Err(ManifestError::new(
                ManifestErrorCode::OverlayBounds,
                format!("unknown overlay anchor `{other}`"),
            )),
        }
    }
}

impl fmt::Display for OverlayAnchor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Bounded geometry for a `session.overlay` view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OverlayDescriptor {
    anchor: OverlayAnchor,
    max_width: u32,
    max_height: u32,
}

impl OverlayDescriptor {
    pub fn anchor(&self) -> OverlayAnchor {
        self.anchor
    }

    pub fn max_width(&self) -> u32 {
        self.max_width
    }

    pub fn max_height(&self) -> u32 {
        self.max_height
    }
}

/// Host event kinds a plugin may subscribe to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EventKind {
    WorkspaceChanged,
    SessionUpdated,
    AgentStatus,
}

impl EventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WorkspaceChanged => "workspace.changed",
            Self::SessionUpdated => "session.updated",
            Self::AgentStatus => "agent.status",
        }
    }

    pub fn parse(text: &str) -> Result<Self, ManifestError> {
        match text {
            "workspace.changed" => Ok(Self::WorkspaceChanged),
            "session.updated" => Ok(Self::SessionUpdated),
            "agent.status" => Ok(Self::AgentStatus),
            other => Err(ManifestError::new(
                ManifestErrorCode::UnknownEventKind,
                format!("unknown host event kind `{other}`"),
            )),
        }
    }
}

impl fmt::Display for EventKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A declared user action contributed by a plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandContribution {
    id: String,
    title: String,
    description: Option<String>,
    scope: CommandScope,
    requires: CapabilitySet,
}

impl CommandContribution {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    pub fn scope(&self) -> CommandScope {
        self.scope
    }

    /// Capabilities a grant store must hold before this command may run.
    pub fn requires(&self) -> &CapabilitySet {
        &self.requires
    }
}

/// A declared native UI contribution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiViewContribution {
    id: String,
    slot: ContributionSlot,
    title: String,
    command: Option<String>,
    platforms: Vec<ClientPlatform>,
    overlay: Option<OverlayDescriptor>,
}

impl UiViewContribution {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn slot(&self) -> ContributionSlot {
        self.slot
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    /// Command invoked when the user activates this view, if any.
    pub fn command(&self) -> Option<&str> {
        self.command.as_deref()
    }

    pub fn platforms(&self) -> &[ClientPlatform] {
        &self.platforms
    }

    pub fn overlay(&self) -> Option<OverlayDescriptor> {
        self.overlay
    }

    pub fn supports(&self, platform: ClientPlatform) -> bool {
        self.platforms.contains(&platform)
    }
}

/// A declared read-only event subscription.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventContribution {
    id: String,
    kinds: Vec<EventKind>,
}

impl EventContribution {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn kinds(&self) -> &[EventKind] {
        &self.kinds
    }
}

// ---------------------------------------------------------------------------
// Raw schema (strictly deserialized)
// ---------------------------------------------------------------------------

/// Schema-level manifest: syntactically parsed but not yet validated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginManifest {
    manifest_version: u32,
    id: String,
    name: String,
    version: String,
    api: String,
    trust: String,
    entrypoint: Option<String>,
    required_capabilities: Vec<String>,
    optional_capabilities: Vec<String>,
    commands: Vec<RawCommand>,
    views: Vec<RawView>,
    events: Vec<RawEvent>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawManifest {
    manifest_version: u32,
    id: String,
    name: String,
    version: String,
    api: String,
    trust: String,
    #[serde(default)]
    entrypoint: Option<String>,
    #[serde(default)]
    capabilities: RawCapabilities,
    #[serde(default)]
    contributions: RawContributions,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCapabilities {
    #[serde(default)]
    required: Vec<String>,
    #[serde(default)]
    optional: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawContributions {
    #[serde(default)]
    commands: Vec<RawCommand>,
    #[serde(default)]
    views: Vec<RawView>,
    #[serde(default)]
    events: Vec<RawEvent>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCommand {
    id: String,
    title: String,
    #[serde(default)]
    description: Option<String>,
    scope: String,
    #[serde(default)]
    requires: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawView {
    id: String,
    slot: String,
    title: String,
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    platforms: Vec<String>,
    #[serde(default)]
    overlay: Option<RawOverlay>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawOverlay {
    anchor: String,
    max_width: u32,
    max_height: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEvent {
    id: String,
    kinds: Vec<String>,
}

impl PluginManifest {
    /// Parse TOML into the schema shape. Does not check semantics.
    pub fn parse_str(text: &str) -> Result<Self, ManifestError> {
        let raw: RawManifest = toml::from_str(text)?;
        let RawManifest {
            manifest_version,
            id,
            name,
            version,
            api,
            trust,
            entrypoint,
            capabilities,
            contributions,
        } = raw;
        Ok(Self {
            manifest_version,
            id,
            name,
            version,
            api,
            trust,
            entrypoint,
            required_capabilities: capabilities.required,
            optional_capabilities: capabilities.optional,
            commands: contributions.commands,
            views: contributions.views,
            events: contributions.events,
        })
    }

    /// Read and parse a manifest file. The caller names the path explicitly;
    /// nothing is discovered by scanning directories.
    pub fn parse_path(path: &Path) -> Result<Self, ManifestError> {
        let text = std::fs::read_to_string(path).map_err(|error| {
            ManifestError::new(
                ManifestErrorCode::Unreadable,
                format!("cannot read `{}`: {}", path.display(), error.kind()),
            )
        })?;
        Self::parse_str(&text)
    }

    pub fn manifest_version(&self) -> u32 {
        self.manifest_version
    }

    /// Unvalidated plugin id text as written in the manifest.
    pub fn id_text(&self) -> &str {
        &self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn version_text(&self) -> &str {
        &self.version
    }

    pub fn api_text(&self) -> &str {
        &self.api
    }

    pub fn trust_text(&self) -> &str {
        &self.trust
    }

    pub fn entrypoint(&self) -> Option<&str> {
        self.entrypoint.as_deref()
    }

    /// Validate every semantic rule and produce the type the supervisor accepts.
    pub fn validate(self) -> Result<ValidatedManifest, ManifestError> {
        let plugin_id_hint = self.id.clone();
        let result = self.validate_inner();
        result.map_err(|error| match error.plugin_id() {
            Some(_) => error,
            None => error.with_plugin_id(plugin_id_hint),
        })
    }

    fn validate_inner(self) -> Result<ValidatedManifest, ManifestError> {
        if self.manifest_version != SUPPORTED_MANIFEST_VERSION {
            return Err(ManifestError::new(
                ManifestErrorCode::UnsupportedManifestVersion,
                format!(
                    "manifest_version {} is not supported; this build reads version {}",
                    self.manifest_version, SUPPORTED_MANIFEST_VERSION
                ),
            ));
        }
        let id = PluginId::parse(&self.id)
            .map_err(|error| ManifestError::new(ManifestErrorCode::InvalidId, error.0))?;
        let name = self.name.trim().to_owned();
        if name.is_empty() || name.len() > limits::MAX_TITLE_LEN {
            return Err(ManifestError::new(
                ManifestErrorCode::InvalidTitle,
                format!(
                    "plugin name must be 1..={} characters",
                    limits::MAX_TITLE_LEN
                ),
            ));
        }
        let version = PluginVersion::parse(&self.version)
            .map_err(|error| ManifestError::new(ManifestErrorCode::InvalidVersion, error.0))?;
        let api = ApiRange::parse(&self.api).map_err(|error| {
            ManifestError::new(ManifestErrorCode::InvalidApiRange, error.to_string())
        })?;
        if !api.contains(HOST_API_VERSION) {
            return Err(ManifestError::new(
                ManifestErrorCode::IncompatibleApi,
                format!("plugin requires api `{api}` but host implements {HOST_API_VERSION}"),
            ));
        }
        let trust = TrustLevel::parse(&self.trust)?;
        let entrypoint = match (trust, self.entrypoint.as_deref()) {
            (TrustLevel::Builtin, Some(_)) => {
                return Err(ManifestError::new(
                    ManifestErrorCode::EntrypointInvalid,
                    "builtin plugins run in-process and must not declare an entrypoint",
                ));
            }
            (TrustLevel::Builtin, None) => None,
            (_, None) => {
                return Err(ManifestError::new(
                    ManifestErrorCode::EntrypointInvalid,
                    "helper plugins must declare an entrypoint file name",
                ));
            }
            (_, Some(name)) => {
                validate_entrypoint(name)?;
                Some(name.to_owned())
            }
        };

        if self.required_capabilities.len() > limits::MAX_CAPABILITIES
            || self.optional_capabilities.len() > limits::MAX_CAPABILITIES
        {
            return Err(ManifestError::new(
                ManifestErrorCode::LimitExceeded,
                format!(
                    "capability lists are limited to {} entries",
                    limits::MAX_CAPABILITIES
                ),
            ));
        }
        let mut declared = CapabilitySet::new();
        let mut required = CapabilitySet::new();
        for text in &self.required_capabilities {
            let capability = parse_capability(text)?;
            if !required.insert(capability.clone()) {
                return Err(ManifestError::new(
                    ManifestErrorCode::DuplicateCapability,
                    format!("capability `{capability}` is declared twice in `required`"),
                ));
            }
            if !declared.insert(capability.clone()) {
                return Err(ManifestError::new(
                    ManifestErrorCode::DuplicateCapability,
                    format!("capability `{capability}` is declared twice"),
                ));
            }
        }
        let mut optional = CapabilitySet::new();
        for text in &self.optional_capabilities {
            let capability = parse_capability(text)?;
            if !optional.insert(capability.clone()) {
                return Err(ManifestError::new(
                    ManifestErrorCode::DuplicateCapability,
                    format!("capability `{capability}` is declared twice in `optional`"),
                ));
            }
            if !declared.insert(capability.clone()) {
                return Err(ManifestError::new(
                    ManifestErrorCode::DuplicateCapability,
                    format!("capability `{capability}` is declared twice"),
                ));
            }
        }

        if self.commands.len() > limits::MAX_COMMANDS {
            return Err(ManifestError::new(
                ManifestErrorCode::LimitExceeded,
                format!(
                    "a plugin may declare at most {} commands",
                    limits::MAX_COMMANDS
                ),
            ));
        }
        let mut commands = Vec::with_capacity(self.commands.len());
        let mut command_ids = BTreeSet::new();
        for raw in self.commands {
            validate_contribution_id("command", &raw.id)?;
            validate_title("command", &raw.title)?;
            if !command_ids.insert(raw.id.clone()) {
                return Err(ManifestError::new(
                    ManifestErrorCode::DuplicateCommand,
                    format!("command `{}` is declared twice", raw.id),
                ));
            }
            let scope = CommandScope::parse(&raw.scope)?;
            if let Some(description) = &raw.description {
                if description.chars().count() > 240 {
                    return Err(ManifestError::new(
                        ManifestErrorCode::InvalidTitle,
                        "command description is longer than 240 characters",
                    ));
                }
            }
            let mut requires = CapabilitySet::new();
            for text in &raw.requires {
                let capability = parse_capability(text)?;
                if !declared.contains(&capability) {
                    return Err(ManifestError::new(
                        ManifestErrorCode::UndeclaredCapability,
                        format!(
                            "command `{}` requires `{capability}` which is not declared in [capabilities]",
                            raw.id
                        ),
                    ));
                }
                if !requires.insert(capability) {
                    return Err(ManifestError::new(
                        ManifestErrorCode::DuplicateCapability,
                        format!("command `{}` requires `{text}` twice", raw.id),
                    ));
                }
            }
            commands.push(CommandContribution {
                id: raw.id,
                title: raw.title,
                description: raw.description,
                scope,
                requires,
            });
        }

        let command_scopes: BTreeMap<&str, CommandScope> = commands
            .iter()
            .map(|command| (command.id(), command.scope()))
            .collect();

        if self.views.len() > limits::MAX_VIEWS {
            return Err(ManifestError::new(
                ManifestErrorCode::LimitExceeded,
                format!("a plugin may declare at most {} views", limits::MAX_VIEWS),
            ));
        }
        let mut views = Vec::with_capacity(self.views.len());
        let mut view_ids = BTreeSet::new();
        for raw in self.views {
            validate_contribution_id("view", &raw.id)?;
            validate_title("view", &raw.title)?;
            if !view_ids.insert(raw.id.clone()) {
                return Err(ManifestError::new(
                    ManifestErrorCode::DuplicateView,
                    format!("view `{}` is declared twice", raw.id),
                ));
            }
            let slot = ContributionSlot::parse(&raw.slot)?;
            if let Some(command) = &raw.command {
                let Some(scope) = command_scopes.get(command.as_str()) else {
                    return Err(ManifestError::new(
                        ManifestErrorCode::UnknownCommandReference,
                        format!("view `{}` binds to undeclared command `{command}`", raw.id),
                    ));
                };
                if slot.is_session_scoped() && *scope != CommandScope::Session {
                    return Err(ManifestError::new(
                        ManifestErrorCode::CommandScopeMismatch,
                        format!(
                            "view `{}` uses slot `{slot}` and must bind a session-scoped command, but `{command}` is `{scope}`",
                            raw.id
                        ),
                    ));
                }
            }
            let platforms = if raw.platforms.is_empty() {
                vec![ClientPlatform::Desktop]
            } else {
                let mut parsed = Vec::with_capacity(raw.platforms.len());
                for text in &raw.platforms {
                    let platform = ClientPlatform::parse(text)?;
                    if parsed.contains(&platform) {
                        return Err(ManifestError::new(
                            ManifestErrorCode::UnknownPlatform,
                            format!("view `{}` lists platform `{text}` twice", raw.id),
                        ));
                    }
                    parsed.push(platform);
                }
                parsed
            };
            let overlay = match (slot, raw.overlay) {
                (ContributionSlot::SessionOverlay, None) => {
                    return Err(ManifestError::new(
                        ManifestErrorCode::OverlayMissing,
                        format!("overlay view `{}` needs an [overlay] descriptor", raw.id),
                    ));
                }
                (ContributionSlot::SessionOverlay, Some(overlay)) => {
                    Some(validate_overlay(&raw.id, overlay)?)
                }
                (_, Some(_)) => {
                    return Err(ManifestError::new(
                        ManifestErrorCode::OverlayNotAllowed,
                        format!(
                            "view `{}` uses slot `{slot}` and must not carry an overlay descriptor",
                            raw.id
                        ),
                    ));
                }
                (_, None) => None,
            };
            views.push(UiViewContribution {
                id: raw.id,
                slot,
                title: raw.title,
                command: raw.command,
                platforms,
                overlay,
            });
        }

        if !self.events.is_empty() && !declared.contains(&Capability::SessionObserve) {
            return Err(ManifestError::new(
                ManifestErrorCode::UndeclaredCapability,
                "event subscriptions require the `session.observe` capability to be declared",
            ));
        }

        if self.events.len() > limits::MAX_EVENT_SUBSCRIPTIONS {
            return Err(ManifestError::new(
                ManifestErrorCode::LimitExceeded,
                format!(
                    "a plugin may declare at most {} event subscriptions",
                    limits::MAX_EVENT_SUBSCRIPTIONS
                ),
            ));
        }
        let mut events = Vec::with_capacity(self.events.len());
        let mut event_ids = BTreeSet::new();
        for raw in self.events {
            validate_contribution_id("event subscription", &raw.id)?;
            if !event_ids.insert(raw.id.clone()) {
                return Err(ManifestError::new(
                    ManifestErrorCode::DuplicateEventSubscription,
                    format!("event subscription `{}` is declared twice", raw.id),
                ));
            }
            if raw.kinds.is_empty() {
                return Err(ManifestError::new(
                    ManifestErrorCode::EmptyEventKinds,
                    format!("event subscription `{}` declares no kinds", raw.id),
                ));
            }
            let mut kinds = Vec::with_capacity(raw.kinds.len());
            for text in &raw.kinds {
                let kind = EventKind::parse(text)?;
                if kinds.contains(&kind) {
                    return Err(ManifestError::new(
                        ManifestErrorCode::DuplicateEventKind,
                        format!("event subscription `{}` lists `{text}` twice", raw.id),
                    ));
                }
                kinds.push(kind);
            }
            events.push(EventContribution { id: raw.id, kinds });
        }

        Ok(ValidatedManifest {
            id,
            name,
            version,
            api,
            trust,
            entrypoint,
            required,
            optional,
            declared,
            commands,
            views,
            events,
        })
    }
}

fn parse_capability(text: &str) -> Result<Capability, ManifestError> {
    Capability::parse(text).map_err(|error| {
        ManifestError::new(ManifestErrorCode::UnknownCapability, error.to_string())
    })
}

fn validate_entrypoint(name: &str) -> Result<(), ManifestError> {
    if name.is_empty() || name.len() > limits::MAX_ENTRYPOINT_LEN {
        return Err(ManifestError::new(
            ManifestErrorCode::EntrypointInvalid,
            format!(
                "entrypoint must be 1..={} characters",
                limits::MAX_ENTRYPOINT_LEN
            ),
        ));
    }
    if name == "." || name == ".." {
        return Err(ManifestError::new(
            ManifestErrorCode::EntrypointInvalid,
            "entrypoint must name a file",
        ));
    }
    // A bare file name only: the host resolves it to an absolute path at
    // install time, and must never hand a manifest-supplied path to the OS.
    let valid = name
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
    if !valid {
        return Err(ManifestError::new(
            ManifestErrorCode::EntrypointInvalid,
            format!("entrypoint `{name}` must be a bare file name (no path separators)"),
        ));
    }
    Ok(())
}

fn validate_contribution_id(kind: &str, id: &str) -> Result<(), ManifestError> {
    let valid_len = (3..=limits::MAX_ID_LEN).contains(&id.len());
    let first_ok = id
        .bytes()
        .next()
        .map(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        .unwrap_or(false);
    let chars_ok = id.bytes().all(|byte| {
        byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
    });
    if !valid_len || !first_ok || !chars_ok {
        return Err(ManifestError::new(
            ManifestErrorCode::InvalidContributionId,
            format!(
                "{kind} id `{id}` must be {}-{} lowercase ASCII characters",
                3,
                limits::MAX_ID_LEN
            ),
        ));
    }
    Ok(())
}

fn validate_title(kind: &str, title: &str) -> Result<(), ManifestError> {
    let trimmed = title.trim();
    if trimmed.is_empty() || trimmed.chars().count() > limits::MAX_TITLE_LEN {
        return Err(ManifestError::new(
            ManifestErrorCode::InvalidTitle,
            format!(
                "{kind} title must be 1..={} characters",
                limits::MAX_TITLE_LEN
            ),
        ));
    }
    Ok(())
}

fn validate_overlay(view_id: &str, raw: RawOverlay) -> Result<OverlayDescriptor, ManifestError> {
    let anchor = OverlayAnchor::parse(&raw.anchor)?;
    for (field, value) in [("max_width", raw.max_width), ("max_height", raw.max_height)] {
        if !(limits::MIN_OVERLAY_EDGE..=limits::MAX_OVERLAY_EDGE).contains(&value) {
            return Err(ManifestError::new(
                ManifestErrorCode::OverlayBounds,
                format!(
                    "overlay view `{view_id}` {field}={value} is outside {}..={}",
                    limits::MIN_OVERLAY_EDGE,
                    limits::MAX_OVERLAY_EDGE
                ),
            ));
        }
    }
    Ok(OverlayDescriptor {
        anchor,
        max_width: raw.max_width,
        max_height: raw.max_height,
    })
}

// ---------------------------------------------------------------------------
// Validated manifest
// ---------------------------------------------------------------------------

/// A manifest that passed every schema and semantic check.
///
/// Constructing this type is the only way to reach the supervisor, so a
/// half-validated manifest cannot be installed, enabled or dispatched to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedManifest {
    id: PluginId,
    name: String,
    version: PluginVersion,
    api: ApiRange,
    trust: TrustLevel,
    entrypoint: Option<String>,
    required: CapabilitySet,
    optional: CapabilitySet,
    declared: CapabilitySet,
    commands: Vec<CommandContribution>,
    views: Vec<UiViewContribution>,
    events: Vec<EventContribution>,
}

impl ValidatedManifest {
    /// Parse and validate in one step.
    pub fn from_toml_str(text: &str) -> Result<Self, ManifestError> {
        PluginManifest::parse_str(text)?.validate()
    }

    /// Read, parse and validate a manifest file.
    pub fn from_path(path: &Path) -> Result<Self, ManifestError> {
        PluginManifest::parse_path(path)?.validate()
    }

    pub fn id(&self) -> &PluginId {
        &self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn version(&self) -> &PluginVersion {
        &self.version
    }

    pub fn api(&self) -> &ApiRange {
        &self.api
    }

    pub fn trust(&self) -> TrustLevel {
        self.trust
    }

    /// Helper executable name declared by the manifest (never a full path).
    pub fn entrypoint(&self) -> Option<&str> {
        self.entrypoint.as_deref()
    }

    pub fn is_in_process(&self) -> bool {
        self.trust.runs_in_process()
    }

    pub fn required_capabilities(&self) -> &CapabilitySet {
        &self.required
    }

    pub fn optional_capabilities(&self) -> &CapabilitySet {
        &self.optional
    }

    /// Required ∪ optional: the complete set the manifest is allowed to use.
    pub fn declared_capabilities(&self) -> &CapabilitySet {
        &self.declared
    }

    pub fn commands(&self) -> &[CommandContribution] {
        &self.commands
    }

    pub fn command(&self, id: &str) -> Option<&CommandContribution> {
        self.commands.iter().find(|command| command.id == id)
    }

    pub fn views(&self) -> &[UiViewContribution] {
        &self.views
    }

    pub fn view(&self, id: &str) -> Option<&UiViewContribution> {
        self.views.iter().find(|view| view.id == id)
    }

    pub fn events(&self) -> &[EventContribution] {
        &self.events
    }

    /// Whether any subscription covers `kind`.
    pub fn subscribes_to(&self, kind: EventKind) -> bool {
        self.events.iter().any(|event| event.kinds.contains(&kind))
    }

    /// Whether the manifest describes the same plugin as `other`, for update
    /// safety checks (id must match; version changes are allowed explicitly).
    pub fn is_same_plugin(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r#"
manifest_version = 1
id = "dev.ukk.sample.minimal"
name = "Minimal"
version = "0.1.0"
api = ">=0.1,<0.2"
trust = "first-party"
entrypoint = "minimal-helper"
"#;

    #[test]
    fn minimal_manifest_validates() {
        let manifest = ValidatedManifest::from_toml_str(MINIMAL).unwrap();
        assert_eq!(manifest.id().as_str(), "dev.ukk.sample.minimal");
        assert_eq!(manifest.version().to_string(), "0.1.0");
        assert_eq!(manifest.trust(), TrustLevel::FirstParty);
        assert!(manifest.commands().is_empty());
        assert!(manifest.declared_capabilities().is_empty());
        assert_eq!(manifest.entrypoint(), Some("minimal-helper"));
    }

    #[test]
    fn plugin_id_rejects_paths_and_unicode() {
        for bad in [
            "", "one", "dev/Kk", "Dev.ukk", "dev..ukk", "-dev.ukk", "dev.-ukk", "dév.ukk",
        ] {
            assert!(PluginId::parse(bad).is_err(), "{bad} should be rejected");
        }
        for good in ["dev.ukk", "dev.ukk.sample", "a1.b2-c3"] {
            assert!(PluginId::parse(good).is_ok(), "{good} should be accepted");
        }
    }

    #[test]
    fn plugin_version_is_strict_numeric_triple() {
        assert!(PluginVersion::parse("0.1.0").is_ok());
        assert!(PluginVersion::parse("0.1").is_err());
        assert!(PluginVersion::parse("0.1.0-beta").is_err());
        assert!(PluginVersion::parse("1.0.0+build").is_err());
        assert!(PluginVersion::parse("1.0.0.0").is_err());
    }

    #[test]
    fn missing_scope_is_reported_as_missing_field() {
        let text = r#"
manifest_version = 1
id = "dev.ukk.sample.minimal"
name = "Minimal"
version = "0.1.0"
api = ">=0.1,<0.2"
trust = "builtin"

[[contributions.commands]]
id = "sample.run"
title = "Run"
"#;
        let error = ValidatedManifest::from_toml_str(text).unwrap_err();
        assert_eq!(error.code(), ManifestErrorCode::MissingField);
    }

    #[test]
    fn unknown_field_is_rejected() {
        let text = format!("{MINIMAL}\nprivileged = true\n");
        let error = ValidatedManifest::from_toml_str(&text).unwrap_err();
        assert_eq!(error.code(), ManifestErrorCode::InvalidToml);
    }
}
