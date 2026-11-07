//! User-scoped capability grants. The store starts empty: manifests declare,
//! users grant, the host enforces.
//!
//! Rules encoded here:
//! * a grant is bound to one plugin, one capability and one scope;
//! * sensitive capabilities (approvals, screen, input) require an explicit,
//!   short-lived expiry — an open-ended screen grant is rejected;
//! * a `Host` grant covers any scope, a `Session` grant covers only that
//!   session, and a `Space` grant covers only that space. Session→Space
//!   inheritance is intentionally not implemented in v1;
//! * revocation is immediate and per-capability; reinstalling a plugin does not
//!   restore grants.
use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;
use std::sync::Mutex;

use ukk_plugin_api::{Capability, PluginId, RiskLevel};

use crate::audit::{lock, now_ms, AuditKind, AuditOutcome};
use crate::error::HostError;
use crate::state::read;
use crate::supervisor::PluginHost;

/// Scope a grant was issued for.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GrantScope {
    /// Any session on this host.
    Host,
    Space(String),
    Session(String),
}

impl fmt::Display for GrantScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Host => f.write_str("host"),
            Self::Space(id) => write!(f, "space:{id}"),
            Self::Session(id) => write!(f, "session:{id}"),
        }
    }
}

/// One issued grant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    capability: Capability,
    scope: GrantScope,
    granted_by: String,
    granted_at_ms: i64,
    expires_at_ms: Option<i64>,
}

impl Grant {
    pub fn capability(&self) -> &Capability {
        &self.capability
    }

    pub fn scope(&self) -> &GrantScope {
        &self.scope
    }

    pub fn granted_by(&self) -> &str {
        &self.granted_by
    }

    pub fn granted_at_ms(&self) -> i64 {
        self.granted_at_ms
    }

    pub fn expires_at_ms(&self) -> Option<i64> {
        self.expires_at_ms
    }

    /// A grant without expiry stays valid until revoked; sensitive grants can
    /// never be created without one.
    pub fn is_active(&self, now_ms: i64) -> bool {
        match self.expires_at_ms {
            Some(expires) => now_ms < expires,
            None => true,
        }
    }
}

/// Request to issue a grant. Constructed by the user-facing surface (desktop
/// settings, mobile approval) — never by a plugin. A request alone grants
/// nothing: only [`crate::PluginHost::grant`] can turn it into authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantRequest {
    pub plugin_id: PluginId,
    pub capability: Capability,
    pub scope: GrantScope,
    pub granted_by: String,
    pub ttl_ms: Option<u64>,
}

/// Maximum lifetime of any single grant (24 hours).
pub const MAX_GRANT_TTL_MS: u64 = 24 * 60 * 60 * 1000;
/// Maximum lifetime of a sensitive grant (1 hour).
pub const MAX_SENSITIVE_GRANT_TTL_MS: u64 = 60 * 60 * 1000;

/// Why a grant request was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrantError {
    /// Sensitive capabilities require an explicit TTL.
    ExpiryRequired { capability: String },
    /// TTL is zero, too long, or overflows.
    ExpiryOutOfRange { capability: String, ttl_ms: u64 },
    /// Granting a capability outside the plugin's declared set.
    UndeclaredCapability { capability: String },
}

impl fmt::Display for GrantError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ExpiryRequired { capability } => write!(
                f,
                "capability `{capability}` is sensitive and requires an explicit expiry"
            ),
            Self::ExpiryOutOfRange { capability, ttl_ms } => write!(
                f,
                "ttl {ttl_ms}ms for capability `{capability}` is outside the allowed range"
            ),
            Self::UndeclaredCapability { capability } => {
                write!(f, "capability `{capability}` is not declared by the plugin")
            }
        }
    }
}

impl std::error::Error for GrantError {}

impl GrantRequest {
    /// Validate the request and materialize a [`Grant`].
    ///
    /// Crate-private: producing a `Grant` value is the first half of issuing
    /// authority, and only [`crate::PluginHost::grant`] may do it.
    pub(crate) fn into_grant(self, now_ms: i64) -> Result<Grant, GrantError> {
        let text = self.capability.to_text();
        let max_ttl = if self.capability.risk() == RiskLevel::Sensitive {
            MAX_SENSITIVE_GRANT_TTL_MS
        } else {
            MAX_GRANT_TTL_MS
        };
        let expires_at_ms = match self.ttl_ms {
            Some(ttl) if ttl == 0 || ttl > max_ttl => {
                return Err(GrantError::ExpiryOutOfRange {
                    capability: text,
                    ttl_ms: ttl,
                });
            }
            Some(ttl) => Some(saturating_add_ms(now_ms, ttl)),
            None if self.capability.is_sensitive() => {
                return Err(GrantError::ExpiryRequired { capability: text });
            }
            None => None,
        };
        Ok(Grant {
            capability: self.capability,
            scope: self.scope,
            granted_by: self.granted_by,
            granted_at_ms: now_ms,
            expires_at_ms,
        })
    }
}

fn saturating_add_ms(now_ms: i64, ttl_ms: u64) -> i64 {
    now_ms.saturating_add(i64::try_from(ttl_ms).unwrap_or(i64::MAX))
}

/// Whether at least one active grant authorizes `capability` in `scope`.
pub fn is_authorized(
    grants: &[Grant],
    capability: &Capability,
    session_id: Option<&str>,
    now_ms: i64,
) -> bool {
    grants.iter().any(|grant| {
        grant.capability == *capability && grant.is_active(now_ms) && grant.scope.allows(session_id)
    })
}

impl GrantScope {
    /// `Host` covers everything; other scopes must match exactly.
    fn allows(&self, session_id: Option<&str>) -> bool {
        match self {
            Self::Host => true,
            Self::Session(id) => session_id == Some(id.as_str()),
            Self::Space(_) => false,
        }
    }
}

/// Proof that a capability was checked against an installed manifest and that
/// the grant passed the TTL rules.
///
/// The token has no public constructor, so external code — including a future
/// GPUI settings surface holding a [`PluginHost`](crate::PluginHost) or the
/// grant store itself — cannot insert authority. Only
/// [`crate::PluginHost::grant`] mints one, after the declared-capability check
/// and before the `GrantGranted` audit record.
#[derive(Debug)]
pub struct AuthorizedGrant(());

impl AuthorizedGrant {
    pub(crate) fn mint() -> Self {
        Self(())
    }
}

/// Persistence boundary for grants. The host only ever calls these methods, so
/// a SQLite-backed store can replace the in-memory one without touching
/// dispatch logic.
///
/// Revocation and listing are safe to expose; `upsert` requires an
/// [`AuthorizedGrant`] token that only the host can mint.
pub trait GrantStore: Send + Sync {
    /// Insert or replace the grant for its (plugin, capability, scope) key.
    /// Requires an [`AuthorizedGrant`] proof.
    fn upsert(&self, plugin_id: &PluginId, grant: Grant, authorized: AuthorizedGrant);
    /// Remove one grant; returns whether it existed.
    fn revoke(&self, plugin_id: &PluginId, capability: &Capability, scope: &GrantScope) -> bool;
    /// Remove every grant for a plugin (used on uninstall); returns the count.
    fn revoke_all(&self, plugin_id: &PluginId) -> usize;
    /// All grants for a plugin, including expired ones, for display/cleanup.
    fn list(&self, plugin_id: &PluginId) -> Vec<Grant>;
}

type GrantKey = (PluginId, Capability, GrantScope);

/// Process-local grant store. Grants do **not** survive a host restart; durable
/// storage is an explicit follow-up, and failing closed on restart is the
/// safer default until it exists.
#[derive(Debug, Default)]
pub struct InMemoryGrantStore {
    grants: Mutex<BTreeMap<GrantKey, Grant>>,
}

impl InMemoryGrantStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl GrantStore for InMemoryGrantStore {
    fn upsert(&self, plugin_id: &PluginId, grant: Grant, _authorized: AuthorizedGrant) {
        let key = (
            plugin_id.clone(),
            grant.capability.clone(),
            grant.scope.clone(),
        );
        lock(&self.grants).insert(key, grant);
    }

    fn revoke(&self, plugin_id: &PluginId, capability: &Capability, scope: &GrantScope) -> bool {
        let key = (plugin_id.clone(), capability.clone(), scope.clone());
        lock(&self.grants).remove(&key).is_some()
    }

    fn revoke_all(&self, plugin_id: &PluginId) -> usize {
        let mut grants = lock(&self.grants);
        let before = grants.len();
        grants.retain(|(id, _, _), _| id != plugin_id);
        before - grants.len()
    }

    fn list(&self, plugin_id: &PluginId) -> Vec<Grant> {
        lock(&self.grants)
            .iter()
            .filter(|((id, _, _), _)| id == plugin_id)
            .map(|(_, grant)| grant.clone())
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Host-facing grant surface
// ---------------------------------------------------------------------------

impl PluginHost {
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
        let granted_by = request.granted_by.clone();
        let grant = request.into_grant(now_ms()).map_err(HostError::Grant)?;
        self.inner
            .grants
            .upsert(&plugin_id, grant.clone(), AuthorizedGrant::mint());
        self.record(
            Some(&plugin_id),
            AuditKind::GrantGranted,
            AuditOutcome::Allowed,
            format!("{capability_text} scope={scope} by={granted_by}"),
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
}
