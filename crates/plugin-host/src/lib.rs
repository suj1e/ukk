//! First-party plugin supervisor and capability broker.
//!
//! # Model
//!
//! * [`PluginInstall`] carries a [`ValidatedManifest`](ukk_plugin_api::ValidatedManifest)
//!   plus either a resolved, absolute [`HelperCommand`] or an
//!   [`InProcessPlugin`]. Installation never starts code and never grants.
//! * [`PluginHost::enable`] starts a helper, performs the `plugin.hello`
//!   handshake and validates every contribution the helper claims. Only then
//!   does the plugin become `Ready`. [`PluginHost::disable`] stops the helper
//!   without restarting the host.
//! * [`PluginHost::invoke`] is the only command entry point. It checks that the
//!   command belongs to the named plugin, that the plugin is ready, that the
//!   scope carries a session id where required, and that every capability the
//!   command requires has an active grant in the right scope.
//! * [`PluginHost::supervise`] detects helper exits and moves that one plugin
//!   to `Failed`. Nothing restarts automatically.
//! * Every life-cycle and authorization decision goes through an
//!   [`AuditSink`].
//!
//! # Trust levels
//!
//! `builtin` plugins are trusted code compiled into a ukk release and run
//! in-process (still capability-checked). `first-party` and `third-party`
//! plugins are separate helper processes with a cleared environment, piped
//! stdio, bounded frames, request deadlines and kill-on-disable. OS-level
//! sandboxing is **not** implemented here; a subprocess is not a sandbox.
//!
//! # Integration seam (for GPUI / Host adapters)
//!
//! A native layer only needs four calls:
//!
//! 1. `install(...)` once per configured plugin — the embedding host owns
//!    discovery, signature/origin checks and the absolute helper path.
//! 2. `session_snapshot(platform)` to render Session actions, tabs, overlays,
//!    context panels and settings sections.
//! 3. `invoke(...)` / `invoke_view(...)` when the user activates a
//!    contribution.
//! 4. `publish_event(...)` when the Host produces a typed session/agent event,
//!    and `supervise()` on a timer or before rendering.
//!
//! Not wired in this crate, deliberately: no Host RPC method, no SQLite grant
//! persistence, no GPUI types, no OS sandbox/entitlements, no automatic
//! restart, and no capture/input implementation. See `docs/PLUGINS.md`.
mod audit;
mod contributions;
mod grant;
mod helper;
mod in_process;
mod supervisor;

pub use audit::{AuditKind, AuditOutcome, AuditRecord, AuditSink, MemoryAuditSink, NullAuditSink};
pub use contributions::{CommandDescriptor, ContributionSnapshot, ViewDescriptor};
pub use grant::{
    is_authorized, Grant, GrantError, GrantRequest, GrantScope, GrantStore, InMemoryGrantStore,
    MAX_GRANT_TTL_MS, MAX_SENSITIVE_GRANT_TTL_MS,
};
pub use helper::{HelperCommand, HelperError, HelperLimits, HelperProcess, ShutdownReport};
pub use in_process::{EchoInProcessPlugin, InProcessPlugin, InvokeContext, PluginFailure};
pub use supervisor::{
    HostConfig, HostError, HostEvent, InvokeOutcome, InvokeRequest, PluginHost, PluginInstall,
    PluginState, PluginStatus, PublishReport,
};
