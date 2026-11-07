//! Command dispatch, event delivery and read-only contribution snapshots.
//!
//! Every command goes through the same authorization order: plugin installed,
//! command owned by that plugin, plugin ready, session scope satisfied, every
//! required capability granted. UI adapters cannot bypass it.
use ukk_plugin_api::{Capability, ClientPlatform, CommandScope, ContributionSlot, PluginId};

use crate::audit::{now_ms, AuditKind, AuditOutcome};
use crate::contributions::{CommandDescriptor, ContributionSnapshot, ViewDescriptor};
use crate::error::HostError;
use crate::grant::is_authorized;
use crate::helper::HelperError;
use crate::in_process::InvokeContext;
use crate::messages::{HostEvent, InvokeOutcome, InvokeRequest, PublishReport};
use crate::state::{read, PluginState, Runtime};
use crate::supervisor::{audit_kind_for_helper_error, session_label, PluginHost};

impl PluginHost {
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
}
