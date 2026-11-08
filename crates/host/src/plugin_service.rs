//! Host-owned plugin service: explicit registry, one supervisor, plugin RPC.
//!
//! The Host never scans directories for plugins. It installs only what an
//! operator listed in one registry file (`~/.ukk/plugins/registry.json` or
//! `UKK_PLUGIN_REGISTRY`). Each entry names a plugin id, an absolute manifest
//! path, an absolute helper path and the startup enabled flag.
//!
//! Consequences that are deliberate:
//!
//! * a missing registry file means "no plugins", not an error;
//! * a malformed, insecure or ambiguous registry refuses **all** entries
//!   (fail closed) and exposes a diagnostic in the read-only snapshot;
//! * helper programs must be regular, executable, non-symlink absolute files;
//! * manifests must validate and identify the same plugin id as the entry;
//! * enable/disable/invoke require the separate, default-off `plugin.control`
//!   token. `plugin.snapshot` is read-only and needs no capability.
//!
//! The service has no handle to herdr-core or the runtime API, so plugin work
//! cannot start, stop or signal an Agent. Enable/disable only owns the plugin
//! helper process.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use ukk_domain::new_id;
use ukk_plugin_api::{ClientPlatform, PluginId, ValidatedManifest};
use ukk_plugin_host::{
    redact, AuditKind, AuditOutcome, AuditRecord, AuditSink, CommandDescriptor,
    ContributionSnapshot, HelperCommand, HostConfig, HostError, InMemoryGrantStore, InvokeOutcome,
    InvokeRequest, PluginHost, PluginInstall, PluginState, PluginStatus, ViewDescriptor,
};

use crate::plugin_audit::BoundedAuditSink;
use crate::plugin_control::PluginControlAuth;

/// One registry file. `version` is exact; adding fields needs a new schema.
const REGISTRY_VERSION: u32 = 1;
const MAX_REGISTRY_BYTES: u64 = 1024 * 1024;
const MAX_MANIFEST_BYTES: u64 = 256 * 1024;
const MAX_REGISTRY_ENTRIES: usize = 64;

/// The plugin service used by the Host process. Cloneable; all clones share one
/// supervisor and one registry view.
#[derive(Clone)]
pub(crate) struct PluginService {
    inner: Arc<ServiceInner>,
}

struct ServiceInner {
    host: PluginHost,
    audit: Arc<BoundedAuditSink>,
    registry_path: PathBuf,
    control_path: PathBuf,
    registry: Mutex<RegistryState>,
    /// Serializes enable/disable with their registry rewrite so the persisted
    /// flag cannot interleave with a concurrent transition.
    operations: Mutex<()>,
    /// Registry-flagged plugins are enabled once, on a background thread, so a
    /// hung helper cannot delay the Host accept loop.
    startup_started: AtomicBool,
}

#[derive(Default)]
struct RegistryState {
    loaded: bool,
    present: bool,
    error: Option<String>,
    entries: BTreeMap<PluginId, RegistryEntryState>,
}

struct RegistryEntryState {
    enabled: bool,
    manifest: PathBuf,
    helper: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistryFile {
    version: u32,
    plugins: Vec<RegistryEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistryEntry {
    id: String,
    manifest: PathBuf,
    helper: PathBuf,
    enabled: bool,
}

struct LoadedEntry {
    id: PluginId,
    enabled: bool,
    manifest_path: PathBuf,
    helper_path: PathBuf,
    manifest: ValidatedManifest,
    helper: HelperCommand,
}

#[derive(Debug, Clone)]
struct RegistryError {
    kind: &'static str,
    message: String,
}

impl RegistryError {
    fn new(kind: &'static str, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: redact(&message.into()),
        }
    }

    fn render(&self) -> String {
        format!("{}: {}", self.kind, self.message)
    }
}

/// A typed RPC failure. `rpc_code` is a JSON-RPC server code; `kind` is the
/// stable capability/lifecycle reason the Desktop can render.
#[derive(Debug)]
pub(crate) struct PluginRpcError {
    pub(crate) rpc_code: i32,
    pub(crate) kind: &'static str,
    pub(crate) message: String,
}

impl PluginRpcError {
    fn new(rpc_code: i32, kind: &'static str, message: impl Into<String>) -> Self {
        Self {
            rpc_code,
            kind,
            message: redact(&message.into()),
        }
    }

    pub(crate) fn value(&self) -> Value {
        json!({
            "code": self.rpc_code,
            "message": self.message,
            "data": {"kind": self.kind},
        })
    }
}

/// Default registry path. `UKK_PLUGIN_REGISTRY` is explicit and may point
/// elsewhere; the Host still validates the file it finds.
pub(crate) fn registry_path_from_env() -> PathBuf {
    std::env::var_os("UKK_PLUGIN_REGISTRY")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            default_registry_path()
                .unwrap_or_else(|| PathBuf::from("ukk-plugin-registry-path-unavailable"))
        })
}

#[cfg(unix)]
fn default_registry_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| {
        PathBuf::from(home)
            .join(".ukk")
            .join("plugins")
            .join("registry.json")
    })
}

#[cfg(windows)]
fn default_registry_path() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .map(|root| root.join("ukk").join("plugins").join("registry.json"))
}

#[cfg(not(any(unix, windows)))]
fn default_registry_path() -> Option<PathBuf> {
    None
}

impl PluginService {
    #[cfg(test)]
    pub(crate) fn test_empty() -> Self {
        Self::load(
            PathBuf::from("/nonexistent/ukk-plugin-registry.json"),
            PathBuf::from("/nonexistent/ukk-plugin-control"),
        )
    }

    pub(crate) fn from_env() -> Self {
        Self::load(registry_path_from_env(), PluginControlAuth::default_path())
    }

    /// Load one explicit registry. A missing file yields an empty service.
    /// Installation is synchronous (no process starts); registry-flagged
    /// plugins are enabled later by [`Self::start_enabled_plugins`] on a
    /// background thread so a hung helper cannot delay the accept loop.
    pub(crate) fn load(registry_path: PathBuf, control_path: PathBuf) -> Self {
        let audit = Arc::new(BoundedAuditSink::default());
        let host = PluginHost::new(
            HostConfig::default(),
            Arc::new(InMemoryGrantStore::new()),
            audit.clone(),
        );
        let mut state = RegistryState::default();
        match load_registry(&registry_path) {
            Ok((present, loaded)) => {
                state.loaded = true;
                state.present = present;
                match install_loaded(&host, &loaded) {
                    Ok(()) => {
                        state.entries = loaded
                            .iter()
                            .map(|entry| {
                                (
                                    entry.id.clone(),
                                    RegistryEntryState {
                                        enabled: entry.enabled,
                                        manifest: entry.manifest_path.clone(),
                                        helper: entry.helper_path.clone(),
                                    },
                                )
                            })
                            .collect();
                    }
                    Err(error) => {
                        state.loaded = false;
                        state.error = Some(error.render());
                        audit.record(&AuditRecord::new(
                            None,
                            AuditKind::ManifestRejected,
                            AuditOutcome::Denied,
                            error.render(),
                        ));
                    }
                }
            }
            Err(error) => {
                state.error = Some(error.render());
                audit.record(&AuditRecord::new(
                    None,
                    AuditKind::ManifestRejected,
                    AuditOutcome::Denied,
                    error.render(),
                ));
            }
        }
        Self {
            inner: Arc::new(ServiceInner {
                host,
                audit,
                registry_path,
                control_path,
                registry: Mutex::new(state),
                operations: Mutex::new(()),
                startup_started: AtomicBool::new(false),
            }),
        }
    }

    /// Start registry-flagged plugins exactly once. The caller invokes this
    /// after the local endpoint is accepting connections; failures stay
    /// visible as `failed` in the snapshot and never block RPC.
    pub(crate) fn start_enabled_plugins(&self) {
        if self.inner.startup_started.swap(true, Ordering::SeqCst) {
            return;
        }
        let ids: Vec<PluginId> = {
            let registry = self.registry();
            registry
                .entries
                .iter()
                .filter(|(_, entry)| entry.enabled)
                .map(|(id, _)| id.clone())
                .collect()
        };
        if ids.is_empty() {
            return;
        }
        let service = self.clone();
        std::thread::spawn(move || {
            for id in ids {
                if let Err(error) = service.inner.host.enable(&id) {
                    // Start-up failure is runtime state, not a registry error;
                    // supervision/snapshot expose it as `failed`.
                    eprintln!(
                        "ukk Host plugin `{}` failed to enable: {}",
                        id,
                        error.code()
                    );
                }
            }
        });
    }

    pub(crate) fn control_auth(&self) -> PluginControlAuth {
        PluginControlAuth::load(&self.inner.control_path)
    }

    fn registry(&self) -> MutexGuard<'_, RegistryState> {
        self.inner
            .registry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub(crate) fn record(
        &self,
        plugin_id: Option<&str>,
        kind: AuditKind,
        outcome: AuditOutcome,
        detail: impl Into<String>,
    ) {
        self.inner
            .audit
            .record(&AuditRecord::new(plugin_id, kind, outcome, detail));
    }

    /// Read-only in the capability sense. It requires no token and starts no
    /// plugin, but it does call `supervise()` (inspect child processes) so a
    /// crashed helper is reported as `failed`. Helper output is never included.
    pub(crate) fn snapshot(&self) -> Value {
        // Reflect unexpected helper exits before reporting state. This only
        // inspects child processes; it starts nothing.
        let _ = self.inner.host.supervise();
        let control = self.control_auth();
        let registry = self.registry();
        let (audit_retained, audit_dropped) = self.inner.audit.summary();
        let mut plugins = Vec::new();
        for id in self.inner.host.installed() {
            let Some(status) = self.inner.host.status(&id) else {
                continue;
            };
            let failure = match &status.state {
                PluginState::Failed(reason) => Some(redact(reason)),
                _ => None,
            };
            plugins.push(json!({
                "plugin_id": id.as_str(),
                "trust": status.trust.as_str(),
                "state": status.state.as_str(),
                "installed": true,
                "enabled_by_registry": registry
                    .entries
                    .get(&id)
                    .map(|entry| entry.enabled)
                    .unwrap_or(false),
                "helper_pid": self.inner.host.helper_pid(&id),
                "command_count": status.command_count,
                "view_count": status.view_count,
                "queued_events": status.queued_events,
                "dropped_events": status.dropped_events,
                "failure": failure,
            }));
        }
        let contributions = self.inner.host.session_snapshot(ClientPlatform::Desktop);
        json!({
            "capability": "separate-plugin-control",
            "control_enabled": control.is_enabled(),
            "control_problem": control.problem().map(redact),
            "registry": {
                "path": redact(&self.inner.registry_path.display().to_string()),
                "loaded": registry.loaded,
                "present": registry.present,
                "error": registry.error.as_deref().map(redact),
            },
            "audit": {
                "retained": audit_retained,
                "dropped": audit_dropped,
                "durable": false,
            },
            "plugins": plugins,
            "contributions": contributions_json(&contributions),
        })
    }

    /// Start one installed plugin. The registry's enabled flag is persisted so
    /// a later Host start keeps the operator's explicit choice.
    pub(crate) fn enable(&self, id: &PluginId) -> Result<Value, PluginRpcError> {
        let _operation = self
            .inner
            .operations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.require_registry_entry(id)?;
        let status = self.inner.host.enable(id).map_err(plugin_error)?;
        if let Err(error) = self.persist_enabled(id, true) {
            let _ = self.inner.host.disable(id);
            return Err(registry_error(error));
        }
        Ok(self.status_json(&status, id))
    }

    /// Stop one plugin without restarting the Host. Grants are intentionally
    /// not revoked: disabling a plugin is not capability revocation.
    pub(crate) fn disable(&self, id: &PluginId) -> Result<Value, PluginRpcError> {
        let _operation = self
            .inner
            .operations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.require_registry_entry(id)?;
        if let Err(error) = self.persist_enabled(id, false) {
            return Err(registry_error(error));
        }
        match self.inner.host.disable(id) {
            Ok(status) => Ok(self.status_json(&status, id)),
            Err(error) => {
                // Keep the registry consistent with the still-running plugin.
                let _ = self.persist_enabled(id, true);
                Err(plugin_error(error))
            }
        }
    }

    pub(crate) fn invoke(
        &self,
        id: PluginId,
        command: String,
        session_id: Option<String>,
        args: Value,
    ) -> Result<InvokeOutcome, PluginRpcError> {
        let mut request = InvokeRequest::new(id, command).with_args(args);
        if let Some(session_id) = session_id {
            request = request.in_session(session_id);
        }
        self.inner.host.invoke(request).map_err(plugin_error)
    }

    fn require_registry_entry(&self, id: &PluginId) -> Result<(), PluginRpcError> {
        let registry = self.registry();
        if let Some(error) = registry.error.as_deref() {
            self.record(
                Some(id.as_str()),
                AuditKind::ManifestRejected,
                AuditOutcome::Denied,
                format!("registry unavailable: {error}"),
            );
            return Err(PluginRpcError::new(
                -32066,
                "registry_error",
                format!("plugin registry is unavailable: {error}"),
            ));
        }
        if !registry.entries.contains_key(id) {
            self.record(
                Some(id.as_str()),
                AuditKind::CommandDenied,
                AuditOutcome::Denied,
                "plugin is not in the explicit registry",
            );
            return Err(plugin_error(HostError::UnknownPlugin {
                plugin_id: id.to_string(),
            }));
        }
        Ok(())
    }

    fn status_json(&self, status: &PluginStatus, id: &PluginId) -> Value {
        let enabled = self
            .registry()
            .entries
            .get(id)
            .map(|entry| entry.enabled)
            .unwrap_or(false);
        json!({
            "plugin_id": id.as_str(),
            "state": status.state.as_str(),
            "installed": true,
            "enabled_by_registry": enabled,
        })
    }

    /// Rewrite the registry atomically with one changed enabled flag. The file
    /// is re-read and revalidated first, so a concurrent operator edit is
    /// preserved instead of being overwritten with stale startup state. The
    /// in-memory flag is only advanced after the on-disk write succeeds.
    fn persist_enabled(&self, id: &PluginId, enabled: bool) -> Result<(), RegistryError> {
        let mut registry = self.registry();
        if let Some(error) = registry.error.as_deref() {
            return Err(RegistryError::new("registry_unavailable", error));
        }
        if !registry.entries.contains_key(id) {
            return Err(RegistryError::new(
                "unknown_plugin",
                format!("plugin `{id}` is not in the registry"),
            ));
        }
        // Re-read with the same validation as startup. If another process
        // added, removed or made an entry unsafe, refuse the rewrite.
        let (_, current) = load_registry(&self.inner.registry_path)?;
        if !current.iter().any(|entry| &entry.id == id) {
            return Err(RegistryError::new(
                "unknown_plugin",
                format!("plugin `{id}` is no longer in the registry"),
            ));
        }
        let mut entries = current
            .iter()
            .map(|entry| RegistryEntry {
                id: entry.id.as_str().to_owned(),
                manifest: entry.manifest_path.clone(),
                helper: entry.helper_path.clone(),
                enabled: if &entry.id == id {
                    enabled
                } else {
                    entry.enabled
                },
            })
            .collect::<Vec<_>>();
        // `BTreeMap` iteration is already sorted, but sort explicitly so the
        // file format is stable even if the implementation changes.
        entries.sort_by(|left, right| left.id.cmp(&right.id));
        write_registry(&self.inner.registry_path, &entries)?;
        // Keep installed entries' cached flags and paths aligned with the
        // re-read file; newly added entries are installed only on restart.
        for (plugin_id, state) in registry.entries.iter_mut() {
            if let Some(current) = current.iter().find(|entry| &entry.id == plugin_id) {
                state.enabled = current.enabled;
                state.manifest = current.manifest_path.clone();
                state.helper = current.helper_path.clone();
            }
        }
        if let Some(entry) = registry.entries.get_mut(id) {
            entry.enabled = enabled;
        }
        Ok(())
    }
}

/// Dispatch one plugin RPC. `plugin.snapshot` is read-only; every other method
/// requires the distinct `plugin.control` value in the `control` field.
pub(crate) fn dispatch(
    method: &str,
    params: &Value,
    service: &PluginService,
) -> Result<Value, PluginRpcError> {
    if method == "plugin.snapshot" {
        return Ok(service.snapshot());
    }
    if !matches!(method, "plugin.enable" | "plugin.disable" | "plugin.invoke") {
        return Err(PluginRpcError::new(
            -32601,
            "unknown_method",
            format!("unknown plugin method `{method}`"),
        ));
    }

    let control = service.control_auth();
    let hint = params.get("plugin_id").and_then(Value::as_str);
    if !control.is_enabled() {
        let error = match control.problem() {
            Some(problem) => PluginRpcError::new(
                -32063,
                "plugin_control_insecure",
                format!("plugin-control capability is disabled: {problem}"),
            ),
            None => PluginRpcError::new(
                -32061,
                "plugin_control_disabled",
                "plugin-control capability is disabled; create its separate token file first",
            ),
        };
        service.record(
            hint,
            AuditKind::PluginControlDenied,
            AuditOutcome::Denied,
            format!(
                "method={method} kind={} (capability unavailable)",
                error.kind
            ),
        );
        return Err(error);
    }
    // Deliberately only `control`. `auth` (workspace metadata) and any
    // agent-control value are never accepted for plugin control.
    if !control.authorized(params.get("control").and_then(Value::as_str)) {
        service.record(
            hint,
            AuditKind::PluginControlDenied,
            AuditOutcome::Denied,
            format!("method={method} kind=plugin_control_unauthorized"),
        );
        return Err(PluginRpcError::new(
            -32062,
            "plugin_control_unauthorized",
            "plugin-control capability is not authorized",
        ));
    }

    let plugin_id = match parse_plugin_id(params) {
        Ok(plugin_id) => plugin_id,
        Err(error) => {
            service.record(
                hint,
                AuditKind::CommandDenied,
                AuditOutcome::Denied,
                format!("method={method} kind=invalid_params"),
            );
            return Err(error);
        }
    };
    match method {
        "plugin.enable" => service.enable(&plugin_id),
        "plugin.disable" => service.disable(&plugin_id),
        "plugin.invoke" => {
            let command = params
                .get("command")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| {
                    PluginRpcError::new(-32602, "invalid_params", "missing or invalid command")
                })?
                .to_owned();
            let session_id = match params.get("session_id") {
                None | Some(Value::Null) => None,
                Some(Value::String(session)) if !session.trim().is_empty() => Some(session.clone()),
                Some(_) => {
                    return Err(PluginRpcError::new(
                        -32602,
                        "invalid_params",
                        "session_id must be a non-empty string",
                    ))
                }
            };
            let args = params.get("args").cloned().unwrap_or(Value::Null);
            let outcome = service.invoke(plugin_id, command, session_id, args)?;
            Ok(json!({
                "plugin_id": outcome.plugin_id.as_str(),
                "command": outcome.command,
                "provenance": outcome.provenance.as_str(),
                "value": outcome.value,
            }))
        }
        _ => unreachable!("method was checked above"),
    }
}

fn parse_plugin_id(params: &Value) -> Result<PluginId, PluginRpcError> {
    let text = params
        .get("plugin_id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            PluginRpcError::new(-32602, "invalid_params", "missing or invalid plugin_id")
        })?;
    PluginId::parse(text)
        .map_err(|error| PluginRpcError::new(-32602, "invalid_params", error.to_string()))
}

fn plugin_error(error: HostError) -> PluginRpcError {
    PluginRpcError::new(-32065, error.code(), error.to_string())
}

fn registry_error(error: RegistryError) -> PluginRpcError {
    PluginRpcError::new(-32066, error.kind, error.message)
}

fn contributions_json(snapshot: &ContributionSnapshot) -> Value {
    json!({
        "session_actions": snapshot.session_actions.iter().map(view_json).collect::<Vec<_>>(),
        "session_tabs": snapshot.session_tabs.iter().map(view_json).collect::<Vec<_>>(),
        "session_overlays": snapshot.session_overlays.iter().map(view_json).collect::<Vec<_>>(),
        "commands": snapshot.commands.iter().map(command_json).collect::<Vec<_>>(),
    })
}

fn view_json(view: &ViewDescriptor) -> Value {
    json!({
        "plugin_id": view.plugin_id().as_str(),
        "view_id": view.view_id(),
        "slot": view.slot().as_str(),
        "title": redact(view.title()),
        "command": view.command(),
        "overlay": view.overlay().map(|overlay| json!({
            "anchor": overlay.anchor().as_str(),
            "max_width": overlay.max_width(),
            "max_height": overlay.max_height(),
        })),
    })
}

fn command_json(command: &CommandDescriptor) -> Value {
    json!({
        "plugin_id": command.plugin_id().as_str(),
        "command_id": command.command_id(),
        "title": redact(command.title()),
        "scope": command.scope().as_str(),
        "requires": command.requires(),
    })
}

// ---------------------------------------------------------------------------
// Registry loading and path validation
// ---------------------------------------------------------------------------

fn load_registry(path: &Path) -> Result<(bool, Vec<LoadedEntry>), RegistryError> {
    let Some(registry_path) =
        inspect_regular_file(path, "registry", Some(MAX_REGISTRY_BYTES), false)?
    else {
        return Ok((false, Vec::new()));
    };
    let text = fs::read_to_string(&registry_path)
        .map_err(|error| RegistryError::new("registry_unreadable", error.to_string()))?;
    let file: RegistryFile = serde_json::from_str(&text).map_err(|error| {
        RegistryError::new(
            "registry_invalid",
            format!("registry is not valid JSON schema v1: {error}"),
        )
    })?;
    if file.version != REGISTRY_VERSION {
        return Err(RegistryError::new(
            "registry_invalid",
            format!(
                "registry version {} is not supported (expected {REGISTRY_VERSION})",
                file.version
            ),
        ));
    }
    if file.plugins.len() > MAX_REGISTRY_ENTRIES {
        return Err(RegistryError::new(
            "registry_invalid",
            format!(
                "registry lists {} plugins; the limit is {MAX_REGISTRY_ENTRIES}",
                file.plugins.len()
            ),
        ));
    }

    let mut ids = BTreeSet::new();
    let mut manifests = BTreeSet::new();
    let mut helpers = BTreeSet::new();
    let mut commands = BTreeSet::new();
    let mut loaded = Vec::with_capacity(file.plugins.len());

    for entry in file.plugins {
        let id = PluginId::parse(&entry.id)
            .map_err(|error| RegistryError::new("registry_invalid", error.to_string()))?;
        let manifest_path =
            inspect_regular_file(&entry.manifest, "manifest", Some(MAX_MANIFEST_BYTES), false)?
                .ok_or_else(|| {
                    RegistryError::new("registry_invalid", "manifest file is missing")
                })?;
        let helper_path = inspect_regular_file(&entry.helper, "helper", None, true)?
            .ok_or_else(|| RegistryError::new("registry_invalid", "helper file is missing"))?;

        if !ids.insert(id.clone()) {
            return Err(RegistryError::new(
                "registry_ambiguous",
                format!("plugin id `{id}` is listed more than once"),
            ));
        }
        if !manifests.insert(manifest_path.clone()) {
            return Err(RegistryError::new(
                "registry_ambiguous",
                format!(
                    "manifest path is listed more than once: {}",
                    manifest_path.display()
                ),
            ));
        }
        if !helpers.insert(helper_path.clone()) {
            return Err(RegistryError::new(
                "registry_ambiguous",
                format!(
                    "helper path is listed more than once: {}",
                    helper_path.display()
                ),
            ));
        }

        let manifest = ValidatedManifest::from_path(&manifest_path).map_err(|error| {
            RegistryError::new(
                "registry_manifest",
                format!("manifest {} is invalid: {error}", manifest_path.display()),
            )
        })?;
        if manifest.id() != &id {
            return Err(RegistryError::new(
                "registry_mismatch",
                format!(
                    "registry entry `{id}` points at a manifest for `{}`",
                    manifest.id()
                ),
            ));
        }
        if !manifest.trust().requires_helper() {
            return Err(RegistryError::new(
                "registry_manifest",
                format!("plugin `{id}` is builtin and cannot be loaded from a helper registry"),
            ));
        }
        let declared = manifest.entrypoint().unwrap_or_default();
        let actual = helper_path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_default();
        if actual != declared {
            return Err(RegistryError::new(
                "registry_mismatch",
                format!(
                    "helper `{}` does not match manifest entrypoint `{}`",
                    actual, declared
                ),
            ));
        }
        for command in manifest.commands() {
            if !commands.insert(command.id().to_owned()) {
                return Err(RegistryError::new(
                    "registry_ambiguous",
                    format!(
                        "command id `{}` is claimed by more than one registry entry",
                        command.id()
                    ),
                ));
            }
        }

        loaded.push(LoadedEntry {
            id: id.clone(),
            enabled: entry.enabled,
            manifest_path,
            helper_path: helper_path.clone(),
            manifest,
            helper: HelperCommand::new(helper_path),
        });
    }
    Ok((true, loaded))
}

fn install_loaded(host: &PluginHost, loaded: &[LoadedEntry]) -> Result<(), RegistryError> {
    // Validate every install object before touching the supervisor so a bad
    // entry cannot leave a half-installed registry behind.
    let mut installs = Vec::with_capacity(loaded.len());
    for entry in loaded {
        let install = PluginInstall::for_helper(entry.manifest.clone(), entry.helper.clone())
            .map_err(|error| RegistryError::new("registry_mismatch", error.to_string()))?;
        installs.push((entry.id.clone(), install));
    }
    let mut installed = Vec::new();
    for (id, install) in installs {
        match host.install(install) {
            Ok(_) => installed.push(id),
            Err(error) => {
                for installed_id in installed {
                    let _ = host.uninstall(&installed_id);
                }
                return Err(RegistryError::new("registry_invalid", error.to_string()));
            }
        }
    }
    Ok(())
}

/// Validate one path and return its canonical absolute form.
fn inspect_regular_file(
    path: &Path,
    label: &str,
    max_bytes: Option<u64>,
    executable: bool,
) -> Result<Option<PathBuf>, RegistryError> {
    if !path.is_absolute() {
        return Err(RegistryError::new(
            "registry_invalid",
            format!("{label} path must be absolute: {}", path.display()),
        ));
    }
    if path
        .components()
        .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return Err(RegistryError::new(
            "registry_invalid",
            format!(
                "{label} path must not contain `.` or `..`: {}",
                path.display()
            ),
        ));
    }
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(RegistryError::new(
                "registry_unreadable",
                format!("cannot inspect {label} {}: {error}", path.display()),
            ))
        }
    };
    if metadata.file_type().is_symlink() {
        return Err(RegistryError::new(
            "registry_insecure",
            format!("{label} must not be a symlink: {}", path.display()),
        ));
    }
    if !metadata.file_type().is_file() {
        return Err(RegistryError::new(
            "registry_invalid",
            format!("{label} must be a regular file: {}", path.display()),
        ));
    }
    if metadata.len() == 0 {
        return Err(RegistryError::new(
            "registry_invalid",
            format!("{label} is empty: {}", path.display()),
        ));
    }
    if let Some(limit) = max_bytes {
        if metadata.len() > limit {
            return Err(RegistryError::new(
                "registry_invalid",
                format!("{label} is larger than {limit} bytes: {}", path.display()),
            ));
        }
    }
    // Windows has no executable bit; a regular file is the available check.
    #[cfg(not(unix))]
    let _ = executable;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = metadata.permissions().mode();
        if mode & 0o022 != 0 {
            return Err(RegistryError::new(
                "registry_insecure",
                format!(
                    "{label} must not be group/other writable (mode {:o}): {}",
                    mode & 0o777,
                    path.display()
                ),
            ));
        }
        if executable && mode & 0o111 == 0 {
            return Err(RegistryError::new(
                "registry_insecure",
                format!("helper is not executable: {}", path.display()),
            ));
        }
    }
    let canonical = path.canonicalize().map_err(|error| {
        RegistryError::new(
            "registry_unreadable",
            format!("cannot resolve {label} {}: {error}", path.display()),
        )
    })?;
    #[cfg(unix)]
    verify_same_owner_and_private_parent(&canonical, &metadata, label)?;
    Ok(Some(canonical))
}

/// Require the file and its real parent directory to belong to the Host user,
/// and the parent to have no group/other write bits. Without this, a registry
/// or helper under `/tmp` could be swapped by another local user between Host
/// starts even though the file itself is not group-writable.
#[cfg(unix)]
fn verify_same_owner_and_private_parent(
    canonical: &Path,
    metadata: &fs::Metadata,
    label: &str,
) -> Result<(), RegistryError> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let euid = unsafe { libc::geteuid() };
    if metadata.uid() != euid {
        return Err(RegistryError::new(
            "registry_insecure",
            format!(
                "{label} is owned by uid {} but the Host runs as uid {euid}: {}",
                metadata.uid(),
                canonical.display()
            ),
        ));
    }
    let parent = canonical.parent().ok_or_else(|| {
        RegistryError::new(
            "registry_insecure",
            format!("{label} has no parent directory: {}", canonical.display()),
        )
    })?;
    let parent_metadata = fs::metadata(parent).map_err(|error| {
        RegistryError::new(
            "registry_unreadable",
            format!(
                "cannot inspect {label} parent {}: {error}",
                parent.display()
            ),
        )
    })?;
    if !parent_metadata.is_dir() {
        return Err(RegistryError::new(
            "registry_insecure",
            format!("{label} parent is not a directory: {}", parent.display()),
        ));
    }
    if parent_metadata.uid() != euid {
        return Err(RegistryError::new(
            "registry_insecure",
            format!(
                "{label} parent is owned by uid {} but the Host runs as uid {euid}: {}",
                parent_metadata.uid(),
                parent.display()
            ),
        ));
    }
    let mode = parent_metadata.permissions().mode();
    if mode & 0o022 != 0 {
        return Err(RegistryError::new(
            "registry_insecure",
            format!(
                "{label} parent must not be group/other writable (mode {:o}): {}",
                mode & 0o777,
                parent.display()
            ),
        ));
    }
    Ok(())
}

fn write_registry(path: &Path, entries: &[RegistryEntry]) -> Result<(), RegistryError> {
    let parent = path.parent().ok_or_else(|| {
        RegistryError::new("registry_unwritable", "registry has no parent directory")
    })?;
    if !parent.is_dir() {
        return Err(RegistryError::new(
            "registry_unwritable",
            format!("registry directory does not exist: {}", parent.display()),
        ));
    }
    let file = RegistryFile {
        version: REGISTRY_VERSION,
        plugins: entries.to_vec(),
    };
    let mut bytes = serde_json::to_vec_pretty(&file)
        .map_err(|error| RegistryError::new("registry_unwritable", error.to_string()))?;
    bytes.push(b'\n');

    let mut name = path.as_os_str().to_os_string();
    name.push(format!(".tmp.{}", new_id()));
    let temporary = PathBuf::from(name);
    let result = (|| -> std::io::Result<()> {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))?;
        }
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.map_err(|error| {
        RegistryError::new(
            "registry_unwritable",
            format!("cannot write registry {}: {error}", path.display()),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn scratch(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "ukk-plugin-service-{name}-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        }
        root
    }

    /// The real sample helper is built by cargo for this package's tests.
    fn sample_helper() -> PathBuf {
        let test_exe = std::env::current_exe().unwrap();
        let profile = test_exe.parent().unwrap().parent().unwrap();
        let name = if cfg!(windows) {
            "ukk-sample-session-helper.exe"
        } else {
            "ukk-sample-session-helper"
        };
        let candidate = profile.join(name);
        if !candidate.is_file() {
            let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
            let mut command = std::process::Command::new(cargo);
            command
                .args(["build", "-p", "ukk-plugin-host", "--bins"])
                .current_dir(env!("CARGO_MANIFEST_DIR"));
            if !cfg!(debug_assertions) {
                command.arg("--release");
            }
            assert!(command.status().unwrap().success(), "build sample helper");
        }
        candidate
            .canonicalize()
            .expect("sample helper binary must exist after cargo build")
    }

    fn sample_manifest() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../plugins/sample-session-action/plugin.toml")
            .canonicalize()
            .unwrap()
    }

    fn registry_json(entries: &[RegistryEntry]) -> String {
        serde_json::to_string(&RegistryFile {
            version: REGISTRY_VERSION,
            plugins: entries.to_vec(),
        })
        .unwrap()
    }

    fn write_registry_file(root: &Path, contents: &str) -> PathBuf {
        let path = root.join("registry.json");
        fs::write(&path, contents).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        path
    }

    fn sample_entry() -> RegistryEntry {
        RegistryEntry {
            id: "dev.ukk.sample.session-action".into(),
            manifest: sample_manifest(),
            helper: sample_helper(),
            enabled: false,
        }
    }

    fn service_for(entries: &[RegistryEntry]) -> (PluginService, PathBuf) {
        let root = scratch("registry");
        let path = write_registry_file(&root, &registry_json(entries));
        let service = PluginService::load(path.clone(), root.join("plugin-control"));
        (service, root)
    }

    fn token_file(root: &Path, insecure: bool) -> (PathBuf, String) {
        let path = root.join("plugin-control-token");
        let secret = format!("{}:{}", new_id(), new_id());
        fs::write(&path, format!("{secret}\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = if insecure { 0o644 } else { 0o600 };
            fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
        }
        let _ = insecure;
        (path, secret)
    }

    #[cfg(unix)]
    fn process_alive(pid: u32) -> bool {
        std::process::Command::new("kill")
            .arg("-0")
            .arg(pid.to_string())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    #[cfg(windows)]
    fn process_alive(pid: u32) -> bool {
        let filter = format!("PID eq {pid}");
        std::process::Command::new("tasklist")
            .args(["/FI", &filter, "/NH"])
            .output()
            .map(|output| String::from_utf8_lossy(&output.stdout).contains(&pid.to_string()))
            .unwrap_or(false)
    }

    fn assert_process_dead(pid: u32) {
        for _ in 0..50 {
            if !process_alive(pid) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        panic!("helper process {pid} was not reaped");
    }

    #[test]
    fn valid_registry_installs_but_does_not_enable() {
        let (service, _root) = service_for(&[sample_entry()]);
        let snapshot = service.snapshot();
        assert_eq!(snapshot["registry"]["loaded"], true);
        assert_eq!(snapshot["registry"]["present"], true);
        assert!(snapshot["registry"]["error"].is_null());
        assert_eq!(snapshot["plugins"].as_array().unwrap().len(), 1);
        assert_eq!(snapshot["plugins"][0]["state"], "disabled");
        assert_eq!(snapshot["plugins"][0]["installed"], true);
        assert_eq!(snapshot["plugins"][0]["enabled_by_registry"], false);
        let contributions = &snapshot["contributions"];
        assert_eq!(
            contributions["session_actions"].as_array().unwrap().len(),
            0
        );
        assert_eq!(contributions["session_tabs"].as_array().unwrap().len(), 0);
        assert_eq!(
            contributions["session_overlays"].as_array().unwrap().len(),
            0
        );
        assert!(contributions.get("context_panels").is_none());
        assert!(contributions.get("settings_sections").is_none());
    }

    #[test]
    fn missing_registry_is_empty_not_an_error() {
        let root = scratch("missing");
        let service = PluginService::load(root.join("registry.json"), root.join("token"));
        let snapshot = service.snapshot();
        assert_eq!(snapshot["registry"]["loaded"], true);
        assert_eq!(snapshot["registry"]["present"], false);
        assert!(snapshot["registry"]["error"].is_null());
        assert!(snapshot["plugins"].as_array().unwrap().is_empty());
    }

    #[test]
    fn unknown_fields_and_wrong_version_are_refused() {
        let root = scratch("unknown-field");
        let path = write_registry_file(&root, r#"{"version":1,"plugins":[],"surprise":true}"#);
        let service = PluginService::load(path, root.join("token"));
        let snapshot = service.snapshot();
        assert_eq!(snapshot["registry"]["loaded"], false);
        assert!(snapshot["registry"]["error"]
            .as_str()
            .unwrap()
            .contains("registry_invalid"));

        let root = scratch("wrong-version");
        let path = write_registry_file(&root, r#"{"version":2,"plugins":[]}"#);
        let service = PluginService::load(path, root.join("token"));
        assert!(service.snapshot()["registry"]["error"]
            .as_str()
            .unwrap()
            .contains("version 2"));
    }

    #[test]
    fn relative_paths_and_duplicate_ids_are_refused() {
        let root = scratch("relative");
        let mut entry = sample_entry();
        entry.helper = PathBuf::from("ukk-sample-session-helper");
        let path = write_registry_file(&root, &registry_json(&[entry]));
        let service = PluginService::load(path, root.join("token"));
        assert!(service.snapshot()["registry"]["error"]
            .as_str()
            .unwrap()
            .contains("must be absolute"));

        let root = scratch("duplicate");
        let entry = sample_entry();
        let path = write_registry_file(&root, &registry_json(&[entry.clone(), entry]));
        let service = PluginService::load(path, root.join("token"));
        assert!(service.snapshot()["registry"]["error"]
            .as_str()
            .unwrap()
            .contains("more than once"));
    }

    #[test]
    fn id_manifest_and_helper_mismatches_are_refused() {
        // Registry id != manifest id.
        let root = scratch("id-mismatch");
        let mut entry = sample_entry();
        entry.id = "dev.ukk.sample.other".into();
        let path = write_registry_file(&root, &registry_json(&[entry]));
        let service = PluginService::load(path, root.join("token"));
        assert!(service.snapshot()["registry"]["error"]
            .as_str()
            .unwrap()
            .contains("manifest for"));

        // Helper program stem != manifest entrypoint.
        let root = scratch("helper-mismatch");
        let mut entry = sample_entry();
        entry.helper = std::env::current_exe().unwrap();
        let path = write_registry_file(&root, &registry_json(&[entry]));
        let service = PluginService::load(path, root.join("token"));
        assert!(service.snapshot()["registry"]["error"]
            .as_str()
            .unwrap()
            .contains("does not match manifest entrypoint"));
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_or_writable_registry_is_refused() {
        use std::os::unix::fs::PermissionsExt;

        let root = scratch("symlink-registry");
        let real = write_registry_file(&root, &registry_json(&[]));
        let link = root.join("link.json");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let service = PluginService::load(link, root.join("token"));
        assert!(service.snapshot()["registry"]["error"]
            .as_str()
            .unwrap()
            .contains("symlink"));

        let root = scratch("writable-registry");
        let path = write_registry_file(&root, &registry_json(&[]));
        fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap();
        let service = PluginService::load(path, root.join("token"));
        assert!(service.snapshot()["registry"]["error"]
            .as_str()
            .unwrap()
            .contains("group/other writable"));
    }

    #[cfg(windows)]
    #[test]
    fn windows_helper_is_a_regular_file_with_an_exe_stem() {
        // Windows has no executable permission bit. The registry accepts a
        // regular file and the `.exe` suffix is dropped for the entrypoint
        // comparison, matching `PluginInstall::for_helper`.
        let root = scratch("windows-helper");
        let helper = root.join("ukk-sample-session-helper.exe");
        fs::write(&helper, b"MZ").unwrap();
        let resolved = inspect_regular_file(&helper, "helper", None, true)
            .unwrap()
            .expect("regular file must be accepted");
        assert_eq!(
            resolved.file_stem().and_then(|stem| stem.to_str()),
            Some("ukk-sample-session-helper")
        );
    }

    #[test]
    fn snapshot_never_contains_helper_output() {
        let (service, _root) = service_for(&[sample_entry()]);
        let id = PluginId::parse("dev.ukk.sample.session-action").unwrap();
        service.enable(&id).expect("enable sample helper");
        let outcome = service
            .invoke(
                id,
                "sample.session.ping".into(),
                Some("session-1".into()),
                Value::Null,
            )
            .expect("invoke sample command");
        assert_eq!(outcome.value["message"], "pong");
        let text = service.snapshot().to_string();
        assert!(
            !text.contains("pong"),
            "helper output must not enter snapshot"
        );
        assert!(!text.contains("capabilities_used"));
        assert!(service.snapshot()["plugins"][0]["helper_pid"].is_u64());
        drop(service);
    }

    #[test]
    fn malformed_registry_fails_closed_with_registry_error() {
        let root = scratch("malformed-registry");
        let path = write_registry_file(&root, r#"{"version":1,"plugins":["#);
        let (token, secret) = token_file(&root, false);
        let service = PluginService::load(path, token);
        let error = dispatch(
            "plugin.enable",
            &json!({"plugin_id": "dev.ukk.sample.session-action", "control": secret}),
            &service,
        )
        .unwrap_err();
        assert_eq!(error.rpc_code, -32066);
        assert_eq!(error.kind, "registry_error");
        assert!(service
            .inner
            .audit
            .records()
            .iter()
            .any(|record| record.kind == AuditKind::ManifestRejected
                && record.outcome == AuditOutcome::Denied));
    }

    #[cfg(unix)]
    #[test]
    fn insecure_token_path_is_reported_and_audited() {
        let root = scratch("insecure-token-rpc");
        let path = write_registry_file(&root, &registry_json(&[sample_entry()]));
        let (token, secret) = token_file(&root, true);
        let service = PluginService::load(path, token);
        assert!(!service.snapshot()["control_enabled"].as_bool().unwrap());
        let error = dispatch(
            "plugin.enable",
            &json!({"plugin_id": "dev.ukk.sample.session-action", "control": secret}),
            &service,
        )
        .unwrap_err();
        assert_eq!(error.rpc_code, -32063);
        assert_eq!(error.kind, "plugin_control_insecure");
        assert!(service
            .inner
            .audit
            .records()
            .iter()
            .any(|record| record.kind == AuditKind::PluginControlDenied
                && record.outcome == AuditOutcome::Denied));
    }

    #[test]
    fn dropping_the_service_reaps_the_helper() {
        let (service, _root) = service_for(&[sample_entry()]);
        let id = PluginId::parse("dev.ukk.sample.session-action").unwrap();
        service.enable(&id).expect("enable sample helper");
        let pid = service
            .inner
            .host
            .helper_pid(&id)
            .expect("running helper pid");
        assert!(process_alive(pid), "helper {pid} should be running");
        drop(service);
        assert_process_dead(pid);
    }

    #[test]
    fn registry_rewrite_preserves_concurrent_entries() {
        let root = scratch("concurrent-registry");
        let sample = sample_entry();
        let path = write_registry_file(&root, &registry_json(std::slice::from_ref(&sample)));
        let service = PluginService::load(path.clone(), root.join("plugin-control"));

        // Another operator adds a second entry while the Host is running.
        let other_helper = std::env::current_exe().unwrap().canonicalize().unwrap();
        let stem = other_helper
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap()
            .to_owned();
        let other_manifest = root.join("other-plugin.toml");
        fs::write(
            &other_manifest,
            format!(
                r#"manifest_version = 1
id = "dev.ukk.other.plugin"
name = "Other plugin"
version = "0.1.0"
api = ">=0.1,<0.2"
trust = "first-party"
entrypoint = "{stem}"
"#
            ),
        )
        .unwrap();
        let other = RegistryEntry {
            id: "dev.ukk.other.plugin".into(),
            manifest: other_manifest.canonicalize().unwrap(),
            helper: other_helper,
            enabled: false,
        };
        fs::write(&path, registry_json(&[sample.clone(), other])).unwrap();

        service
            .persist_enabled(
                &PluginId::parse("dev.ukk.sample.session-action").unwrap(),
                true,
            )
            .expect("persist enabled flag");
        let written: RegistryFile =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(written.plugins.len(), 2);
        assert!(written
            .plugins
            .iter()
            .any(|entry| entry.id == "dev.ukk.other.plugin"));
        assert!(written
            .plugins
            .iter()
            .any(|entry| entry.id == sample.id && entry.enabled));
    }
}
