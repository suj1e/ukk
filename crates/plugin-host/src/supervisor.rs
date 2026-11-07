//! Plugin supervisor: installation, life cycle and helper supervision.
//!
//! Invariants enforced here:
//!
//! * only a [`ValidatedManifest`] can be installed;
//! * command ids are unique across installed plugins, so a plugin cannot
//!   shadow another's action;
//! * a helper is a separate process with a cleared environment, bounded frames,
//!   request deadlines and kill-on-disable;
//! * a crash moves exactly one plugin to `Failed`, never the host.
//!
//! Dispatch lives in [`crate::dispatch`], grants in [`crate::grant`].
use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex, RwLock};

use ukk_plugin_api::wire::HelloResult;
use ukk_plugin_api::{PluginId, ValidatedManifest, HELPER_PROTOCOL_VERSION};

use crate::audit::AuditSink;
use crate::audit::{AuditKind, AuditOutcome, AuditRecord};
use crate::config::HostConfig;
use crate::error::HostError;
use crate::grant::GrantStore;
use crate::helper::{HelperCommand, HelperError, HelperProcess, ShutdownReport};
use crate::in_process::InProcessPlugin;
use crate::redact::redact;
use crate::state::{
    read, status_of, stop_runtime, write, HostInner, PluginState, PluginStatus, Runtime, Slot,
};

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
        // The manifest names the program; the installer supplies its absolute
        // path. Refuse a mismatch so a manifest cannot describe one helper and
        // have another executed (file stem drops `.exe` on Windows).
        let declared = manifest.entrypoint().unwrap_or_default();
        let actual = helper
            .program()
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_default();
        if actual != declared {
            return Err(HostError::InvalidHelper {
                detail: format!(
                    "helper `{}` does not match manifest entrypoint `{}`",
                    redact(actual),
                    declared
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

/// The plugin supervisor. Cloneable; all clones share one state.
#[derive(Clone)]
pub struct PluginHost {
    pub(crate) inner: Arc<HostInner>,
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
                dropped_audited: std::sync::atomic::AtomicU64::new(0),
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
            // A fresh helper starts with an empty drop counter; reset the
            // coalescing watermark so its first overflow is audited again.
            slot.dropped_audited
                .store(0, std::sync::atomic::Ordering::Relaxed);
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

    /// Transition to `Failed` only when the runtime that failed is still the
    /// installed one and the plugin is ready. Returns whether it happened.
    ///
    /// The runtime handle is *taken out of the slot and stopped*, not merely
    /// dropped: helper reader/writer threads hold an `Arc<HelperProcess>`, so a
    /// dropped slot reference would leave the child running and untracked.
    pub(crate) fn mark_failed(
        &self,
        plugin_id: &PluginId,
        runtime: &Runtime,
        kind: AuditKind,
        detail: String,
    ) -> bool {
        let (changed, taken) = {
            let mut plugins = write(&self.inner.plugins);
            match plugins.get_mut(plugin_id) {
                Some(slot) => {
                    let same_runtime = slot
                        .runtime
                        .as_ref()
                        .map(|current| current.same(runtime))
                        .unwrap_or(false);
                    if same_runtime && slot.state.is_ready() {
                        slot.generation += 1;
                        slot.state = PluginState::Failed(detail.clone());
                        (true, slot.runtime.take())
                    } else {
                        (false, None)
                    }
                }
                None => (false, None),
            }
        };
        if changed {
            self.record(Some(plugin_id), kind, AuditOutcome::Failed, detail);
            if let Some(runtime) = taken {
                stop_runtime(runtime);
            }
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

    pub(crate) fn record(
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
pub(crate) fn session_label(session_id: Option<&str>) -> String {
    match session_id {
        Some(id) => format!("session:{id}"),
        None => "host".to_owned(),
    }
}

pub(crate) fn audit_kind_for_helper_error(error: &HelperError) -> AuditKind {
    match error {
        HelperError::Timeout { .. } => AuditKind::HelperTimeout,
        HelperError::Crashed { .. } => AuditKind::HelperCrashed,
        HelperError::Protocol { .. } | HelperError::FrameTooLarge { .. } => {
            AuditKind::ProtocolViolation
        }
        _ => AuditKind::CommandDenied,
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
            redact(&hello.plugin_id),
            manifest.id()
        ));
    }
    for command in &hello.contributions.commands {
        if manifest.command(command).is_none() {
            return Err(format!(
                "helper declared undeclared command `{}`",
                redact(command)
            ));
        }
    }
    for view in &hello.contributions.views {
        if manifest.view(view).is_none() {
            return Err(format!(
                "helper declared undeclared view `{}`",
                redact(view)
            ));
        }
    }
    Ok(())
}
