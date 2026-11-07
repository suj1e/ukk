//! Plugin supervisor: installation, life cycle, command dispatch, event
//! delivery and audit.
//!
//! Invariants enforced here:
//!
//! * only a [`ValidatedManifest`] can be installed;
//! * command ids are unique across installed plugins, so a plugin cannot
//!   shadow another's action;
//! * a command runs only if the manifest declares it, the plugin is `Ready`,
//!   the scope matches, and every capability the command requires has an
//!   active, matching grant;
//! * helpers are separate processes with cleared environments, bounded frames,
//!   request deadlines and kill-on-disable;
//! * a crash moves exactly one plugin to `Failed`, never the host.
use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::Duration;

use serde_json::Value;
use ukk_plugin_api::wire::HelloResult;
use ukk_plugin_api::{
    limits, Capability, ClientPlatform, CommandScope, ContributionSlot, EventKind, PluginId,
    TrustLevel, ValidatedManifest, HELPER_PROTOCOL_VERSION,
};

use crate::audit::{now_ms, AuditKind, AuditOutcome, AuditRecord, AuditSink};
use crate::contributions::{CommandDescriptor, ContributionSnapshot, ViewDescriptor};
use crate::grant::{is_authorized, Grant, GrantError, GrantRequest, GrantScope, GrantStore};
use crate::helper::{HelperCommand, HelperError, HelperLimits, HelperProcess, ShutdownReport};
use crate::in_process::{InProcessPlugin, InvokeContext};

/// Supervisor tunables. Defaults are deliberately small; tests shrink them.
#[derive(Debug, Clone)]
pub struct HostConfig {
    pub request_timeout: Duration,
    pub shutdown_grace: Duration,
    pub max_frame_bytes: usize,
    pub event_queue_capacity: usize,
    pub max_plugins: usize,
}

impl Default for HostConfig {
    fn default() -> Self {
        Self {
            request_timeout: Duration::from_secs(2),
            shutdown_grace: Duration::from_millis(750),
            max_frame_bytes: limits::MAX_FRAME_BYTES,
            event_queue_capacity: limits::MAX_EVENT_QUEUE,
            max_plugins: 64,
        }
    }
}

impl HostConfig {
    fn helper_limits(&self) -> HelperLimits {
        HelperLimits {
            request_timeout: self.request_timeout,
            shutdown_grace: self.shutdown_grace,
            max_frame_bytes: self.max_frame_bytes,
            event_queue_capacity: self.event_queue_capacity,
        }
    }
}

/// Lifecycle position of one plugin inside this host process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginState {
    Disabled,
    Starting,
    /// Helper answered `plugin.hello`; contributions may be rendered.
    Ready,
    /// Start-up or runtime failure; the host does not restart automatically.
    Failed(String),
}

impl PluginState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Starting => "starting",
            Self::Ready => "ready",
            Self::Failed(_) => "failed",
        }
    }

    pub fn is_ready(&self) -> bool {
        matches!(self, Self::Ready)
    }
}

impl fmt::Display for PluginState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Failed(reason) => write!(f, "failed: {reason}"),
            other => f.write_str(other.as_str()),
        }
    }
}

/// Snapshot of one plugin's supervision state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginStatus {
    pub plugin_id: PluginId,
    pub trust: TrustLevel,
    pub state: PluginState,
    pub command_count: usize,
    pub view_count: usize,
    pub queued_events: usize,
    pub dropped_events: u64,
}

/// Everything the supervisor needs to install one plugin.
pub struct PluginInstall {
    manifest: ValidatedManifest,
    helper: Option<HelperCommand>,
    in_process: Option<Arc<dyn InProcessPlugin>>,
}

impl fmt::Debug for PluginInstall {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PluginInstall")
            .field("plugin_id", &self.manifest.id())
            .field("trust", &self.manifest.trust())
            .field("helper", &self.helper)
            .field("in_process", &self.in_process.is_some())
            .finish()
    }
}

impl PluginInstall {
    /// Install a helper-backed plugin. Rejects `builtin` manifests: in-process
    /// contributions must be registered explicitly by trusted code.
    pub fn for_helper(
        manifest: ValidatedManifest,
        helper: HelperCommand,
    ) -> Result<Self, HostError> {
        if manifest.trust().runs_in_process() {
            return Err(HostError::TrustMismatch {
                plugin_id: manifest.id().to_string(),
                detail: "builtin manifests must be installed with an in-process implementation"
                    .into(),
            });
        }
        if !helper.program().is_absolute() {
            return Err(HostError::InvalidHelper {
                detail: format!(
                    "helper program `{}` must be an absolute path",
                    helper.program().display()
                ),
            });
        }
        Ok(Self {
            manifest,
            helper: Some(helper),
            in_process: None,
        })
    }

    /// Install a built-in, in-process plugin.
    pub fn for_in_process(
        manifest: ValidatedManifest,
        plugin: Arc<dyn InProcessPlugin>,
    ) -> Result<Self, HostError> {
        if !manifest.trust().runs_in_process() {
            return Err(HostError::TrustMismatch {
                plugin_id: manifest.id().to_string(),
                detail: "only `builtin` manifests may run in-process".into(),
            });
        }
        if manifest.entrypoint().is_some() {
            return Err(HostError::TrustMismatch {
                plugin_id: manifest.id().to_string(),
                detail: "builtin manifests must not declare an entrypoint".into(),
            });
        }
        Ok(Self {
            manifest,
            helper: None,
            in_process: Some(plugin),
        })
    }

    pub fn manifest(&self) -> &ValidatedManifest {
        &self.manifest
    }
}

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

#[derive(Clone)]
enum Runtime {
    Helper(Arc<HelperProcess>),
    InProcess(Arc<dyn InProcessPlugin>),
}

impl Runtime {
    fn same(&self, other: &Runtime) -> bool {
        match (self, other) {
            (Self::Helper(a), Self::Helper(b)) => Arc::ptr_eq(a, b),
            (Self::InProcess(a), Self::InProcess(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }

    fn queued_events(&self) -> usize {
        match self {
            Self::Helper(process) => process.queued_events(),
            Self::InProcess(_) => 0,
        }
    }

    fn dropped_events(&self) -> u64 {
        match self {
            Self::Helper(process) => process.dropped_events(),
            Self::InProcess(_) => 0,
        }
    }
}

struct Slot {
    manifest: Arc<ValidatedManifest>,
    helper: Option<HelperCommand>,
    in_process: Option<Arc<dyn InProcessPlugin>>,
    state: PluginState,
    runtime: Option<Runtime>,
    /// Bumped on every transition; prevents a stale enable from overwriting a
    /// concurrent disable.
    generation: u64,
}

struct HostInner {
    config: HostConfig,
    grants: Arc<dyn GrantStore>,
    audit: Arc<dyn AuditSink>,
    plugins: RwLock<BTreeMap<PluginId, Slot>>,
    command_owners: RwLock<BTreeMap<String, PluginId>>,
    lifecycle: Mutex<()>,
}

impl Drop for HostInner {
    /// Dropping the last host handle stops every helper. Without this, helper
    /// threads hold an `Arc<HelperProcess>` and the child would outlive the
    /// supervisor.
    fn drop(&mut self) {
        let plugins = match self.plugins.get_mut() {
            Ok(plugins) => plugins,
            Err(poisoned) => poisoned.into_inner(),
        };
        for slot in plugins.values_mut() {
            if let Some(runtime) = slot.runtime.take() {
                stop_runtime(runtime);
            }
        }
    }
}

/// The plugin supervisor. Cloneable; all clones share one state.
#[derive(Clone)]
pub struct PluginHost {
    inner: Arc<HostInner>,
}

impl fmt::Debug for PluginHost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PluginHost")
            .field("installed", &read(&self.inner.plugins).len())
            .finish_non_exhaustive()
    }
}

impl PluginHost {
    pub fn new(config: HostConfig, grants: Arc<dyn GrantStore>, audit: Arc<dyn AuditSink>) -> Self {
        Self {
            inner: Arc::new(HostInner {
                config,
                grants,
                audit,
                plugins: RwLock::new(BTreeMap::new()),
                command_owners: RwLock::new(BTreeMap::new()),
                lifecycle: Mutex::new(()),
            }),
        }
    }

    /// Install a plugin in the `Disabled` state. Installation never starts
    /// code and never grants capabilities.
    pub fn install(&self, install: PluginInstall) -> Result<PluginId, HostError> {
        let PluginInstall {
            manifest,
            helper,
            in_process,
        } = install;
        let manifest = Arc::new(manifest);
        let plugin_id = manifest.id().clone();
        let _lifecycle = crate::audit::lock(&self.inner.lifecycle);

        // Defense in depth: re-check the trust/runtime pairing even though the
        // constructors already did.
        let pairing_ok = matches!(
            (
                manifest.trust().runs_in_process(),
                helper.is_some(),
                in_process.is_some(),
            ),
            (true, false, true) | (false, true, false)
        );
        match pairing_ok {
            true => {}
            false => {
                self.record(
                    Some(&plugin_id),
                    AuditKind::ManifestRejected,
                    AuditOutcome::Denied,
                    "trust/runtime pairing is inconsistent",
                );
                return Err(HostError::TrustMismatch {
                    plugin_id: plugin_id.to_string(),
                    detail: "trust level and runtime kind do not match".into(),
                });
            }
        }

        let mut plugins = write(&self.inner.plugins);
        if plugins.contains_key(&plugin_id) {
            drop(plugins);
            self.record(
                Some(&plugin_id),
                AuditKind::DuplicatePlugin,
                AuditOutcome::Denied,
                "plugin id already installed",
            );
            return Err(HostError::DuplicatePlugin {
                plugin_id: plugin_id.to_string(),
            });
        }
        if plugins.len() >= self.inner.config.max_plugins {
            drop(plugins);
            return Err(HostError::TooManyPlugins {
                limit: self.inner.config.max_plugins,
            });
        }
        // Lock order is always `plugins` then `command_owners`, matching the
        // dispatch path, so install and invoke cannot deadlock.
        let mut owners = write(&self.inner.command_owners);
        for command in manifest.commands() {
            if let Some(owner) = owners.get(command.id()) {
                let owner = owner.clone();
                drop(owners);
                drop(plugins);
                self.record(
                    Some(&plugin_id),
                    AuditKind::DuplicateContribution,
                    AuditOutcome::Denied,
                    format!("command `{}` is owned by `{owner}`", command.id()),
                );
                return Err(HostError::DuplicateContribution {
                    plugin_id: plugin_id.to_string(),
                    contribution: command.id().to_owned(),
                });
            }
        }
        for command in manifest.commands() {
            owners.insert(command.id().to_owned(), plugin_id.clone());
        }
        drop(owners);

        let summary = format!(
            "trust={} commands={} views={} events={}",
            manifest.trust(),
            manifest.commands().len(),
            manifest.views().len(),
            manifest.events().len()
        );
        plugins.insert(
            plugin_id.clone(),
            Slot {
                manifest,
                helper,
                in_process,
                state: PluginState::Disabled,
                runtime: None,
                generation: 0,
            },
        );
        drop(plugins);
        self.record(
            Some(&plugin_id),
            AuditKind::PluginInstalled,
            AuditOutcome::Allowed,
            summary,
        );
        Ok(plugin_id)
    }

    /// Remove a plugin, stopping its helper and revoking every grant.
    pub fn uninstall(&self, plugin_id: &PluginId) -> Result<(), HostError> {
        let _lifecycle = crate::audit::lock(&self.inner.lifecycle);
        let slot = write(&self.inner.plugins).remove(plugin_id);
        let Some(slot) = slot else {
            return Err(HostError::UnknownPlugin {
                plugin_id: plugin_id.to_string(),
            });
        };
        if let Some(runtime) = slot.runtime {
            stop_runtime(runtime);
        }
        {
            let mut owners = write(&self.inner.command_owners);
            owners.retain(|_, owner| owner != plugin_id);
        }
        let revoked = self.inner.grants.revoke_all(plugin_id);
        self.record(
            Some(plugin_id),
            AuditKind::PluginUninstalled,
            AuditOutcome::Allowed,
            format!("revoked_grants={revoked}"),
        );
        Ok(())
    }

    /// Start a plugin. Idempotent when already ready.
    pub fn enable(&self, plugin_id: &PluginId) -> Result<PluginStatus, HostError> {
        let _lifecycle = crate::audit::lock(&self.inner.lifecycle);
        let (manifest, helper, in_process) = {
            let plugins = read(&self.inner.plugins);
            let slot = plugins
                .get(plugin_id)
                .ok_or_else(|| HostError::UnknownPlugin {
                    plugin_id: plugin_id.to_string(),
                })?;
            match &slot.state {
                PluginState::Ready => return Ok(status_of(plugin_id, slot)),
                PluginState::Starting => {
                    return Err(HostError::Busy {
                        plugin_id: plugin_id.to_string(),
                    })
                }
                PluginState::Disabled | PluginState::Failed(_) => {}
            }
            (
                Arc::clone(&slot.manifest),
                slot.helper.clone(),
                slot.in_process.clone(),
            )
        };

        let generation = {
            let mut plugins = write(&self.inner.plugins);
            let slot = plugins
                .get_mut(plugin_id)
                .ok_or_else(|| HostError::UnknownPlugin {
                    plugin_id: plugin_id.to_string(),
                })?;
            slot.generation += 1;
            slot.state = PluginState::Starting;
            slot.runtime = None;
            slot.generation
        };

        let runtime = if manifest.trust().requires_helper() {
            let Some(helper) = helper else {
                return Err(self.fail_enable(
                    plugin_id,
                    generation,
                    AuditKind::EnableFailed,
                    "helper plugin has no resolved helper command".into(),
                ));
            };
            let process = match HelperProcess::spawn(
                plugin_id.as_str(),
                &helper,
                self.inner.config.helper_limits(),
            ) {
                Ok(process) => process,
                Err(error) => {
                    return Err(self.fail_enable(
                        plugin_id,
                        generation,
                        AuditKind::EnableFailed,
                        error.to_string(),
                    ))
                }
            };
            let hello = match process.hello(plugin_id.as_str()) {
                Ok(hello) => hello,
                Err(error) => {
                    let _ = process.shutdown();
                    return Err(self.fail_enable(
                        plugin_id,
                        generation,
                        audit_kind_for_helper_error(&error),
                        error.to_string(),
                    ));
                }
            };
            if let Err(detail) = validate_hello(&manifest, &hello) {
                let _ = process.shutdown();
                return Err(self.fail_enable(
                    plugin_id,
                    generation,
                    AuditKind::ProtocolViolation,
                    detail,
                ));
            }
            Runtime::Helper(process)
        } else {
            let Some(plugin) = in_process else {
                return Err(self.fail_enable(
                    plugin_id,
                    generation,
                    AuditKind::EnableFailed,
                    "builtin plugin has no in-process implementation".into(),
                ));
            };
            let hello = plugin.hello();
            if let Err(detail) = validate_hello(&manifest, &hello) {
                return Err(self.fail_enable(
                    plugin_id,
                    generation,
                    AuditKind::ProtocolViolation,
                    detail,
                ));
            }
            Runtime::InProcess(plugin)
        };

        {
            let mut plugins = write(&self.inner.plugins);
            let slot = plugins
                .get_mut(plugin_id)
                .ok_or_else(|| HostError::UnknownPlugin {
                    plugin_id: plugin_id.to_string(),
                })?;
            if slot.generation != generation {
                drop(plugins);
                stop_runtime(runtime);
                return Err(HostError::Busy {
                    plugin_id: plugin_id.to_string(),
                });
            }
            slot.runtime = Some(runtime);
            slot.state = PluginState::Ready;
            self.record(
                Some(plugin_id),
                AuditKind::PluginEnabled,
                AuditOutcome::Allowed,
                format!("trust={}", manifest.trust()),
            );
            Ok(status_of(plugin_id, slot))
        }
    }

    /// Stop a plugin without restarting the host. Grants are intentionally
    /// kept: disabling is not revocation.
    pub fn disable(&self, plugin_id: &PluginId) -> Result<PluginStatus, HostError> {
        let _lifecycle = crate::audit::lock(&self.inner.lifecycle);
        let runtime = {
            let mut plugins = write(&self.inner.plugins);
            let slot = plugins
                .get_mut(plugin_id)
                .ok_or_else(|| HostError::UnknownPlugin {
                    plugin_id: plugin_id.to_string(),
                })?;
            slot.generation += 1;
            slot.state = PluginState::Disabled;
            slot.runtime.take()
        };
        let detail = match runtime {
            Some(Runtime::Helper(process)) => {
                let ShutdownReport { killed, exit_code } = process.shutdown();
                format!("helper stopped killed={killed} exit={exit_code:?}")
            }
            Some(Runtime::InProcess(_)) => "in-process contribution stopped".to_owned(),
            None => "already stopped".to_owned(),
        };
        self.record(
            Some(plugin_id),
            AuditKind::PluginDisabled,
            AuditOutcome::Allowed,
            detail,
        );
        self.status(plugin_id)
            .ok_or_else(|| HostError::UnknownPlugin {
                plugin_id: plugin_id.to_string(),
            })
    }

    /// Stop a failed/stuck plugin and start it again.
    pub fn restart(&self, plugin_id: &PluginId) -> Result<PluginStatus, HostError> {
        let _ = self.disable(plugin_id)?;
        self.enable(plugin_id)
    }

    /// Check every ready helper and mark unexpected exits as `Failed`.
    ///
    /// Returns the plugins that changed state. There is no automatic restart
    /// loop: recovery is an explicit user or operator action.
    pub fn supervise(&self) -> Vec<PluginId> {
        let _lifecycle = crate::audit::lock(&self.inner.lifecycle);
        let candidates = {
            let plugins = read(&self.inner.plugins);
            plugins
                .iter()
                .filter_map(|(id, slot)| {
                    if !slot.state.is_ready() {
                        return None;
                    }
                    let runtime = slot.runtime.as_ref()?;
                    match runtime {
                        Runtime::Helper(process) if !process.is_alive() => {
                            Some((id.clone(), runtime.clone()))
                        }
                        _ => None,
                    }
                })
                .collect::<Vec<_>>()
        };
        let mut failed = Vec::new();
        for (plugin_id, runtime) in candidates {
            let reason = match &runtime {
                Runtime::Helper(process) => process.last_error(),
                Runtime::InProcess(_) => continue,
            };
            if self.mark_failed(
                &plugin_id,
                &runtime,
                AuditKind::HelperCrashed,
                reason.to_string(),
            ) {
                failed.push(plugin_id);
            }
        }
        failed
    }

    pub fn status(&self, plugin_id: &PluginId) -> Option<PluginStatus> {
        read(&self.inner.plugins)
            .get(plugin_id)
            .map(|slot| status_of(plugin_id, slot))
    }

    pub fn installed(&self) -> Vec<PluginId> {
        read(&self.inner.plugins).keys().cloned().collect()
    }

    /// OS process id of the running helper, when one is running. In-process
    /// plugins return `None`.
    pub fn helper_pid(&self, plugin_id: &PluginId) -> Option<u32> {
        let plugins = read(&self.inner.plugins);
        match plugins.get(plugin_id)?.runtime.as_ref()? {
            Runtime::Helper(process) => Some(process.pid()),
            Runtime::InProcess(_) => None,
        }
    }

    pub fn is_enabled(&self, plugin_id: &PluginId) -> bool {
        read(&self.inner.plugins)
            .get(plugin_id)
            .map(|slot| slot.state.is_ready())
            .unwrap_or(false)
    }

    /// Issue a grant after checking the capability is declared by the plugin.
    pub fn grant(&self, request: GrantRequest) -> Result<Grant, HostError> {
        let plugin_id = request.plugin_id.clone();
        let capability_text = request.capability.to_text();
        {
            let plugins = read(&self.inner.plugins);
            let slot = plugins
                .get(&plugin_id)
                .ok_or_else(|| HostError::UnknownPlugin {
                    plugin_id: plugin_id.to_string(),
                })?;
            if !slot
                .manifest
                .declared_capabilities()
                .contains(&request.capability)
            {
                drop(plugins);
                self.record(
                    Some(&plugin_id),
                    AuditKind::GrantGranted,
                    AuditOutcome::Denied,
                    format!("capability `{capability_text}` is not declared"),
                );
                return Err(HostError::UndeclaredCapability {
                    plugin_id: plugin_id.to_string(),
                    capability: capability_text,
                });
            }
        }
        let scope = request.scope.clone();
        let grant = request.into_grant(now_ms()).map_err(HostError::Grant)?;
        self.inner.grants.upsert(&plugin_id, grant.clone());
        self.record(
            Some(&plugin_id),
            AuditKind::GrantGranted,
            AuditOutcome::Allowed,
            format!("{capability_text} scope={scope}"),
        );
        Ok(grant)
    }

    /// Revoke one grant; returns whether it existed.
    pub fn revoke(
        &self,
        plugin_id: &PluginId,
        capability: &Capability,
        scope: &GrantScope,
    ) -> bool {
        let removed = self.inner.grants.revoke(plugin_id, capability, scope);
        self.record(
            Some(plugin_id),
            AuditKind::GrantRevoked,
            if removed {
                AuditOutcome::Allowed
            } else {
                AuditOutcome::Denied
            },
            format!("{capability} scope={scope}"),
        );
        removed
    }

    /// Remove every grant for a plugin (e.g. a user "deny all" action).
    pub fn revoke_all_grants(&self, plugin_id: &PluginId) -> usize {
        let removed = self.inner.grants.revoke_all(plugin_id);
        self.record(
            Some(plugin_id),
            AuditKind::GrantRevoked,
            AuditOutcome::Allowed,
            format!("all grants revoked ({removed})"),
        );
        removed
    }

    pub fn grants(&self, plugin_id: &PluginId) -> Vec<Grant> {
        self.inner.grants.list(plugin_id)
    }

    pub fn grant_store(&self) -> &Arc<dyn GrantStore> {
        &self.inner.grants
    }

    /// Dispatch one command with ownership, scope and grant checks.
    pub fn invoke(&self, request: InvokeRequest) -> Result<InvokeOutcome, HostError> {
        let InvokeRequest {
            plugin_id,
            command,
            session_id,
            args,
        } = request;

        let (runtime, requires) = {
            let plugins = read(&self.inner.plugins);
            let Some(slot) = plugins.get(&plugin_id) else {
                drop(plugins);
                self.record(
                    Some(&plugin_id),
                    AuditKind::UnknownCommand,
                    AuditOutcome::Denied,
                    format!("plugin is not installed (command `{command}`)"),
                );
                return Err(HostError::UnknownPlugin {
                    plugin_id: plugin_id.to_string(),
                });
            };
            let Some(declared) = slot.manifest.command(&command) else {
                let owner = read(&self.inner.command_owners).get(&command).cloned();
                drop(plugins);
                return Err(self.deny_unknown_command(&plugin_id, &command, owner));
            };
            if !slot.state.is_ready() {
                let state = slot.state.to_string();
                drop(plugins);
                self.record(
                    Some(&plugin_id),
                    AuditKind::CommandDenied,
                    AuditOutcome::Denied,
                    format!("command `{command}` while state={state}"),
                );
                return Err(HostError::NotEnabled {
                    plugin_id: plugin_id.to_string(),
                    state,
                });
            }
            let Some(runtime) = slot.runtime.clone() else {
                drop(plugins);
                return Err(HostError::NotEnabled {
                    plugin_id: plugin_id.to_string(),
                    state: PluginState::Disabled.to_string(),
                });
            };
            if declared.scope() == CommandScope::Session && session_id.is_none() {
                drop(plugins);
                self.record(
                    Some(&plugin_id),
                    AuditKind::CommandDenied,
                    AuditOutcome::Denied,
                    format!("session-scoped command `{command}` without a session id"),
                );
                return Err(HostError::MissingSession {
                    plugin_id: plugin_id.to_string(),
                    command,
                });
            }
            (runtime, declared.requires().clone())
        };

        if !requires.is_empty() {
            let grants = self.inner.grants.list(&plugin_id);
            let now = now_ms();
            for capability in requires.iter() {
                if !is_authorized(&grants, capability, session_id.as_deref(), now) {
                    self.record(
                        Some(&plugin_id),
                        AuditKind::CommandDenied,
                        AuditOutcome::Denied,
                        format!(
                            "command `{command}` missing `{capability}` for scope={}",
                            session_label(session_id.as_deref())
                        ),
                    );
                    return Err(HostError::CapabilityDenied {
                        plugin_id: plugin_id.to_string(),
                        capability: capability.to_text(),
                        session_id,
                    });
                }
            }
        }

        let result = match &runtime {
            Runtime::Helper(process) => process.invoke(&command, session_id.as_deref(), args),
            Runtime::InProcess(plugin) => {
                let context = InvokeContext {
                    plugin_id: plugin_id.clone(),
                    command: command.clone(),
                    session_id: session_id.clone(),
                    args,
                };
                plugin
                    .invoke(&context)
                    .map_err(|failure| HelperError::Remote {
                        code: failure.code,
                        message: failure.message,
                    })
            }
        };

        match result {
            Ok(value) => {
                self.record(
                    Some(&plugin_id),
                    AuditKind::CommandInvoked,
                    AuditOutcome::Allowed,
                    format!("command `{command}`"),
                );
                Ok(InvokeOutcome {
                    plugin_id,
                    command,
                    value,
                })
            }
            Err(error) => {
                let kind = audit_kind_for_helper_error(&error);
                let fatal = matches!(
                    error,
                    HelperError::Timeout { .. }
                        | HelperError::Crashed { .. }
                        | HelperError::Protocol { .. }
                        | HelperError::FrameTooLarge { .. }
                        | HelperError::Io { .. }
                );
                if fatal {
                    self.mark_failed(&plugin_id, &runtime, kind, error.to_string());
                } else {
                    self.record(
                        Some(&plugin_id),
                        AuditKind::CommandDenied,
                        AuditOutcome::Failed,
                        format!("command `{command}`: {error}"),
                    );
                }
                Err(match error {
                    HelperError::Remote { code, message } => HostError::PluginFailed {
                        plugin_id: plugin_id.to_string(),
                        code,
                        message,
                    },
                    other => HostError::Helper(other),
                })
            }
        }
    }

    /// Resolve a view id to its command and invoke it.
    pub fn invoke_view(
        &self,
        plugin_id: &PluginId,
        view_id: &str,
        session_id: Option<&str>,
    ) -> Result<InvokeOutcome, HostError> {
        let command = {
            let plugins = read(&self.inner.plugins);
            let slot = plugins
                .get(plugin_id)
                .ok_or_else(|| HostError::UnknownPlugin {
                    plugin_id: plugin_id.to_string(),
                })?;
            let view = slot
                .manifest
                .view(view_id)
                .ok_or_else(|| HostError::UnknownView {
                    plugin_id: plugin_id.to_string(),
                    view: view_id.to_owned(),
                })?;
            view.command().map(str::to_owned)
        };
        let Some(command) = command else {
            return Err(HostError::UnknownCommand {
                plugin_id: plugin_id.to_string(),
                command: view_id.to_owned(),
            });
        };
        let mut request = InvokeRequest::new(plugin_id.clone(), command);
        if let Some(session) = session_id {
            request = request.in_session(session);
        }
        self.invoke(request)
    }

    /// Offer one host event to subscribed plugins. Never blocks on a plugin:
    /// each helper has a bounded queue and drops are counted.
    pub fn publish_event(&self, event: HostEvent) -> PublishReport {
        let mut report = PublishReport::default();
        let plugins = read(&self.inner.plugins);
        for (plugin_id, slot) in plugins.iter() {
            if !slot.state.is_ready() || !slot.manifest.subscribes_to(event.kind) {
                continue;
            }
            let Some(runtime) = slot.runtime.as_ref() else {
                continue;
            };
            let authorized = is_authorized(
                &self.inner.grants.list(plugin_id),
                &Capability::SessionObserve,
                event.session_id.as_deref(),
                now_ms(),
            );
            if !authorized {
                report.skipped += 1;
                continue;
            }
            match runtime {
                Runtime::Helper(process) => {
                    match process.push_event(event.kind.as_str(), event.payload.clone()) {
                        Ok(true) => report.delivered += 1,
                        Ok(false) => {
                            report.dropped += 1;
                            self.record(
                                Some(plugin_id),
                                AuditKind::EventDropped,
                                AuditOutcome::Failed,
                                format!("event `{}` dropped: queue full", event.kind),
                            );
                        }
                        Err(_) => report.skipped += 1,
                    }
                }
                // In-process plugins have no event sink in v1.
                Runtime::InProcess(_) => report.skipped += 1,
            }
        }
        report
    }

    /// Read-only UI contributions from ready plugins for one platform.
    pub fn session_snapshot(&self, platform: ClientPlatform) -> ContributionSnapshot {
        let mut snapshot = ContributionSnapshot::default();
        let plugins = read(&self.inner.plugins);
        for (plugin_id, slot) in plugins.iter() {
            if !slot.state.is_ready() {
                continue;
            }
            for view in slot.manifest.views() {
                if !view.supports(platform) {
                    continue;
                }
                let descriptor = ViewDescriptor::from_view(plugin_id, view);
                match view.slot() {
                    ContributionSlot::SessionAction => snapshot.session_actions.push(descriptor),
                    ContributionSlot::SessionTab => snapshot.session_tabs.push(descriptor),
                    ContributionSlot::SessionOverlay => snapshot.session_overlays.push(descriptor),
                    ContributionSlot::SessionContextPanel => {
                        snapshot.context_panels.push(descriptor)
                    }
                    ContributionSlot::SettingsSection => {
                        snapshot.settings_sections.push(descriptor)
                    }
                }
            }
            for command in slot.manifest.commands() {
                snapshot.commands.push(CommandDescriptor::new(
                    plugin_id,
                    command.id(),
                    command.title(),
                    command.scope(),
                    command.requires().iter().map(Capability::to_text).collect(),
                ));
            }
        }
        snapshot
    }

    fn deny_unknown_command(
        &self,
        plugin_id: &PluginId,
        command: &str,
        owner: Option<PluginId>,
    ) -> HostError {
        match owner {
            Some(owner) => {
                self.record(
                    Some(plugin_id),
                    AuditKind::ForgedRequest,
                    AuditOutcome::Denied,
                    format!("command `{command}` is owned by `{owner}`"),
                );
                HostError::ForgedCommand {
                    plugin_id: plugin_id.to_string(),
                    command: command.to_owned(),
                    owner: owner.to_string(),
                }
            }
            None => {
                self.record(
                    Some(plugin_id),
                    AuditKind::UnknownCommand,
                    AuditOutcome::Denied,
                    format!("command `{command}` is not declared"),
                );
                HostError::UnknownCommand {
                    plugin_id: plugin_id.to_string(),
                    command: command.to_owned(),
                }
            }
        }
    }

    /// Transition to `Failed` only when the runtime that failed is still the
    /// installed one and the plugin is ready; returns whether it happened.
    fn mark_failed(
        &self,
        plugin_id: &PluginId,
        runtime: &Runtime,
        kind: AuditKind,
        detail: String,
    ) -> bool {
        let mut changed = false;
        {
            let mut plugins = write(&self.inner.plugins);
            if let Some(slot) = plugins.get_mut(plugin_id) {
                let same_runtime = slot
                    .runtime
                    .as_ref()
                    .map(|current| current.same(runtime))
                    .unwrap_or(false);
                if same_runtime && slot.state.is_ready() {
                    slot.generation += 1;
                    slot.state = PluginState::Failed(detail.clone());
                    // Dropping the runtime kills the helper.
                    slot.runtime = None;
                    changed = true;
                }
            }
        }
        if changed {
            self.record(Some(plugin_id), kind, AuditOutcome::Failed, detail);
        }
        changed
    }

    fn fail_enable(
        &self,
        plugin_id: &PluginId,
        generation: u64,
        kind: AuditKind,
        detail: String,
    ) -> HostError {
        {
            let mut plugins = write(&self.inner.plugins);
            if let Some(slot) = plugins.get_mut(plugin_id) {
                if slot.generation == generation {
                    slot.state = PluginState::Failed(detail.clone());
                    slot.runtime = None;
                }
            }
        }
        self.record(Some(plugin_id), kind, AuditOutcome::Failed, detail.clone());
        HostError::PluginFailed {
            plugin_id: plugin_id.to_string(),
            code: kind.as_str().to_owned(),
            message: detail,
        }
    }

    fn record(
        &self,
        plugin_id: Option<&PluginId>,
        kind: AuditKind,
        outcome: AuditOutcome,
        detail: impl Into<String>,
    ) {
        self.inner.audit.record(&AuditRecord::new(
            plugin_id.map(PluginId::as_str),
            kind,
            outcome,
            detail,
        ));
    }
}

fn status_of(plugin_id: &PluginId, slot: &Slot) -> PluginStatus {
    PluginStatus {
        plugin_id: plugin_id.clone(),
        trust: slot.manifest.trust(),
        state: slot.state.clone(),
        command_count: slot.manifest.commands().len(),
        view_count: slot.manifest.views().len(),
        queued_events: slot
            .runtime
            .as_ref()
            .map(Runtime::queued_events)
            .unwrap_or(0),
        dropped_events: slot
            .runtime
            .as_ref()
            .map(Runtime::dropped_events)
            .unwrap_or(0),
    }
}

fn session_label(session_id: Option<&str>) -> String {
    match session_id {
        Some(id) => format!("session:{id}"),
        None => "host".to_owned(),
    }
}

fn audit_kind_for_helper_error(error: &HelperError) -> AuditKind {
    match error {
        HelperError::Timeout { .. } => AuditKind::HelperTimeout,
        HelperError::Crashed { .. } => AuditKind::HelperCrashed,
        HelperError::Protocol { .. } | HelperError::FrameTooLarge { .. } => {
            AuditKind::ProtocolViolation
        }
        _ => AuditKind::CommandDenied,
    }
}

fn stop_runtime(runtime: Runtime) {
    if let Runtime::Helper(process) = runtime {
        let _ = process.shutdown();
    }
}

fn validate_hello(manifest: &ValidatedManifest, hello: &HelloResult) -> Result<(), String> {
    if hello.protocol != HELPER_PROTOCOL_VERSION {
        return Err(format!(
            "helper speaks protocol {} but host requires {}",
            hello.protocol, HELPER_PROTOCOL_VERSION
        ));
    }
    if hello.plugin_id != manifest.id().as_str() {
        return Err(format!(
            "helper identified as `{}` but manifest is `{}`",
            hello.plugin_id,
            manifest.id()
        ));
    }
    for command in &hello.contributions.commands {
        if manifest.command(command).is_none() {
            return Err(format!("helper declared undeclared command `{command}`"));
        }
    }
    for view in &hello.contributions.views {
        if manifest.view(view).is_none() {
            return Err(format!("helper declared undeclared view `{view}`"));
        }
    }
    Ok(())
}

fn read<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn write<T>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    lock.write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
