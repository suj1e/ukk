//! Shared host state: plugin slots, runtime handles and lock helpers.
//!
//! Lock order is always `plugins` then `command_owners`; the life-cycle mutex
//! serializes enable/disable/install/uninstall so two transitions cannot
//! overlap. Dispatch reads the map without the life-cycle lock.
use std::collections::BTreeMap;
use std::fmt;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard};

use ukk_plugin_api::{PluginId, TrustLevel, ValidatedManifest};

use crate::audit::AuditSink;
use crate::config::HostConfig;
use crate::grant::GrantStore;
use crate::helper::{HelperCommand, HelperProcess};
use crate::in_process::InProcessPlugin;

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

pub(crate) fn status_of(plugin_id: &PluginId, slot: &Slot) -> PluginStatus {
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

pub(crate) fn stop_runtime(runtime: Runtime) {
    if let Runtime::Helper(process) = runtime {
        if process.is_closed() {
            // Already failed (crash, protocol violation or missed deadline):
            // skip the graceful round trip and reap immediately.
            let _ = process.kill_now();
        } else {
            let _ = process.shutdown();
        }
    }
}

pub(crate) fn read<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub(crate) fn write<T>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    lock.write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[derive(Clone)]
pub(crate) enum Runtime {
    Helper(Arc<HelperProcess>),
    InProcess(Arc<dyn InProcessPlugin>),
}

impl Runtime {
    pub(crate) fn same(&self, other: &Runtime) -> bool {
        match (self, other) {
            (Self::Helper(a), Self::Helper(b)) => Arc::ptr_eq(a, b),
            (Self::InProcess(a), Self::InProcess(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }

    pub(crate) fn queued_events(&self) -> usize {
        match self {
            Self::Helper(process) => process.queued_events(),
            Self::InProcess(_) => 0,
        }
    }

    pub(crate) fn dropped_events(&self) -> u64 {
        match self {
            Self::Helper(process) => process.dropped_events(),
            Self::InProcess(_) => 0,
        }
    }
}

pub(crate) struct Slot {
    pub(crate) manifest: Arc<ValidatedManifest>,
    pub(crate) helper: Option<HelperCommand>,
    pub(crate) in_process: Option<Arc<dyn InProcessPlugin>>,
    pub(crate) state: PluginState,
    pub(crate) runtime: Option<Runtime>,
    /// Bumped on every transition; prevents a stale enable from overwriting a
    /// concurrent disable.
    pub(crate) generation: u64,
    /// Highest dropped-event count already written to the audit sink; keeps a
    /// hostile plugin from turning queue overflow into unbounded log growth.
    pub(crate) dropped_audited: AtomicU64,
}

pub(crate) struct HostInner {
    pub(crate) config: HostConfig,
    pub(crate) grants: Arc<dyn GrantStore>,
    pub(crate) audit: Arc<dyn AuditSink>,
    pub(crate) plugins: RwLock<BTreeMap<PluginId, Slot>>,
    pub(crate) command_owners: RwLock<BTreeMap<String, PluginId>>,
    pub(crate) lifecycle: Mutex<()>,
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
