//! Separate view/control grants and the single-controller lease.
//!
//! Viewing and control are different authorities. A control grant can only be
//! issued against a live view grant for the same device, Session and media
//! session, and remote input additionally requires a live controller lease.
//! Issuing, renewing, revoking and expiring all use an injected absolute
//! `now_ms`; the crate never reads the system clock.

use crate::capability::MediaSessionId;
use crate::error::GrantError;
use crate::limits::{
    MAX_ACTIVE_GRANTS, MAX_CONTROLLER_LEASE_MS, MAX_CONTROL_GRANT_MS, MAX_VIEW_GRANT_MS,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use ukk_protocol::{DeviceId, DomainId, LeaseId};

/// Opaque grant identity. Distinct from a controller lease id.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct GrantId(LeaseId);

impl GrantId {
    #[must_use]
    pub fn new_random() -> Self {
        Self(LeaseId::new_random())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Display for GrantId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0.as_str())
    }
}

/// Permission to receive the media stream. It never authorizes input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViewGrant {
    pub id: GrantId,
    pub device_id: DeviceId,
    pub session_id: DomainId,
    pub media_session_id: MediaSessionId,
    pub expires_at_ms: i64,
}

impl ViewGrant {
    #[must_use]
    pub fn is_live_at(&self, now_ms: i64) -> bool {
        now_ms < self.expires_at_ms
    }
}

/// Permission to become the controller. It is issued only from a live
/// [`ViewGrant`] for the same binding and still requires a controller lease.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlGrant {
    pub id: GrantId,
    pub view_grant_id: GrantId,
    pub device_id: DeviceId,
    pub session_id: DomainId,
    pub media_session_id: MediaSessionId,
    pub expires_at_ms: i64,
}

impl ControlGrant {
    #[must_use]
    pub fn is_live_at(&self, now_ms: i64) -> bool {
        now_ms < self.expires_at_ms
    }
}

/// Time-limited, single-controller input authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControllerLease {
    pub id: LeaseId,
    pub control_grant_id: GrantId,
    pub device_id: DeviceId,
    pub session_id: DomainId,
    pub media_session_id: MediaSessionId,
    pub expires_at_ms: i64,
}

impl ControllerLease {
    #[must_use]
    pub fn is_live_at(&self, now_ms: i64) -> bool {
        now_ms < self.expires_at_ms
    }
}

/// In-memory authority for one Host process.
///
/// `stopped` is terminal for a media session id: a new media session must use a
/// new id. Device revocation, explicit stop, lock and pause all withdraw the
/// controller lease; resuming never restores an old input authority.
#[derive(Debug, Default)]
pub struct GrantAuthority {
    view_grants: BTreeMap<GrantId, ViewGrant>,
    control_grants: BTreeMap<GrantId, ControlGrant>,
    leases: BTreeMap<MediaSessionId, ControllerLease>,
    locked: BTreeSet<MediaSessionId>,
    stopped: BTreeSet<MediaSessionId>,
}

impl GrantAuthority {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn validate_ttl(ttl_ms: i64, max: i64) -> Result<(), GrantError> {
        if ttl_ms <= 0 || ttl_ms > max {
            return Err(GrantError::InvalidTtl);
        }
        Ok(())
    }

    pub fn grant_view(
        &mut self,
        device_id: DeviceId,
        session_id: DomainId,
        media_session_id: MediaSessionId,
        now_ms: i64,
        ttl_ms: i64,
    ) -> Result<ViewGrant, GrantError> {
        Self::validate_ttl(ttl_ms, MAX_VIEW_GRANT_MS)?;
        if self.stopped.contains(&media_session_id) {
            return Err(GrantError::SessionClosed);
        }
        self.purge_expired(now_ms);
        if self.duplicate_view(&device_id, &session_id, &media_session_id, now_ms) {
            return Err(GrantError::AlreadyGranted);
        }
        if self.view_grants.len() + self.control_grants.len() >= MAX_ACTIVE_GRANTS {
            return Err(GrantError::Capacity);
        }
        let grant = ViewGrant {
            id: GrantId::new_random(),
            device_id,
            session_id,
            media_session_id,
            expires_at_ms: now_ms.saturating_add(ttl_ms),
        };
        self.view_grants.insert(grant.id.clone(), grant.clone());
        Ok(grant)
    }

    /// Issues control only from a live view grant with the same binding.
    pub fn grant_control(
        &mut self,
        view_grant_id: &GrantId,
        now_ms: i64,
        ttl_ms: i64,
    ) -> Result<ControlGrant, GrantError> {
        Self::validate_ttl(ttl_ms, MAX_CONTROL_GRANT_MS)?;
        let view = self.live_view(view_grant_id, now_ms)?.clone();
        if self.stopped.contains(&view.media_session_id) {
            return Err(GrantError::SessionClosed);
        }
        self.purge_expired(now_ms);
        if self.duplicate_control(
            &view.device_id,
            &view.session_id,
            &view.media_session_id,
            now_ms,
        ) {
            return Err(GrantError::AlreadyGranted);
        }
        if self.view_grants.len() + self.control_grants.len() >= MAX_ACTIVE_GRANTS {
            return Err(GrantError::Capacity);
        }
        let expires_at_ms = now_ms.saturating_add(ttl_ms).min(view.expires_at_ms);
        if expires_at_ms <= now_ms {
            return Err(GrantError::ViewGrantExpired);
        }
        let grant = ControlGrant {
            id: GrantId::new_random(),
            view_grant_id: view.id.clone(),
            device_id: view.device_id,
            session_id: view.session_id,
            media_session_id: view.media_session_id,
            expires_at_ms,
        };
        self.control_grants.insert(grant.id.clone(), grant.clone());
        Ok(grant)
    }

    /// Binds the single controller slot for a media session.
    pub fn claim_controller(
        &mut self,
        control_grant_id: &GrantId,
        now_ms: i64,
        ttl_ms: i64,
    ) -> Result<ControllerLease, GrantError> {
        Self::validate_ttl(ttl_ms, MAX_CONTROLLER_LEASE_MS)?;
        let control = self.live_control(control_grant_id, now_ms)?.clone();
        let view = self.live_view(&control.view_grant_id, now_ms)?.clone();
        if self.stopped.contains(&control.media_session_id) {
            return Err(GrantError::SessionClosed);
        }
        if self.locked.contains(&control.media_session_id) {
            return Err(GrantError::Locked);
        }
        if let Some(existing) = self.leases.get(&control.media_session_id) {
            if existing.is_live_at(now_ms) {
                return Err(GrantError::ControllerAlreadyBound);
            }
        }
        self.purge_expired(now_ms);
        let expires_at_ms = now_ms
            .saturating_add(ttl_ms)
            .min(control.expires_at_ms)
            .min(view.expires_at_ms);
        if expires_at_ms <= now_ms {
            return Err(GrantError::ControlGrantExpired);
        }
        let lease = ControllerLease {
            id: LeaseId::new_random(),
            control_grant_id: control.id,
            device_id: control.device_id,
            session_id: control.session_id,
            media_session_id: control.media_session_id,
            expires_at_ms,
        };
        self.leases
            .insert(lease.media_session_id.clone(), lease.clone());
        Ok(lease)
    }

    /// Extends an existing controller lease for the same device. The lease id
    /// is stable; the expiry is capped by the control and view grants.
    pub fn renew_controller(
        &mut self,
        lease_id: &LeaseId,
        device_id: &DeviceId,
        now_ms: i64,
        ttl_ms: i64,
    ) -> Result<ControllerLease, GrantError> {
        Self::validate_ttl(ttl_ms, MAX_CONTROLLER_LEASE_MS)?;
        let existing = self
            .leases
            .values()
            .find(|lease| &lease.id == lease_id)
            .cloned()
            .ok_or(GrantError::LeaseMissing)?;
        if &existing.device_id != device_id {
            return Err(GrantError::LeaseMismatch);
        }
        if !existing.is_live_at(now_ms) {
            return Err(GrantError::LeaseExpired);
        }
        let control = self
            .live_control(&existing.control_grant_id, now_ms)?
            .clone();
        let view = self.live_view(&control.view_grant_id, now_ms)?.clone();
        if self.locked.contains(&existing.media_session_id) {
            return Err(GrantError::Locked);
        }
        let expires_at_ms = now_ms
            .saturating_add(ttl_ms)
            .min(control.expires_at_ms)
            .min(view.expires_at_ms);
        if expires_at_ms <= now_ms {
            return Err(GrantError::ControlGrantExpired);
        }
        let renewed = ControllerLease {
            expires_at_ms,
            ..existing
        };
        self.leases
            .insert(renewed.media_session_id.clone(), renewed.clone());
        Ok(renewed)
    }

    /// Explicitly releases the controller slot. Only the owning device may do
    /// so.
    pub fn release_controller(
        &mut self,
        lease_id: &LeaseId,
        device_id: &DeviceId,
    ) -> Result<(), GrantError> {
        let lease = self
            .leases
            .values()
            .find(|lease| &lease.id == lease_id)
            .cloned()
            .ok_or(GrantError::LeaseMissing)?;
        if &lease.device_id != device_id {
            return Err(GrantError::LeaseMismatch);
        }
        self.leases.remove(&lease.media_session_id);
        Ok(())
    }

    /// Withdraws the controller lease for a media session (pause, lock,
    /// reconnect). Returns whether a lease was removed.
    pub fn release_controller_for_media(&mut self, media_session_id: &MediaSessionId) -> bool {
        self.leases.remove(media_session_id).is_some()
    }

    pub fn authorize_view(
        &self,
        device_id: &DeviceId,
        session_id: &DomainId,
        media_session_id: &MediaSessionId,
        now_ms: i64,
    ) -> Result<&ViewGrant, GrantError> {
        let mut expired = false;
        for grant in self.view_grants.values() {
            if &grant.device_id == device_id
                && &grant.session_id == session_id
                && &grant.media_session_id == media_session_id
            {
                if grant.is_live_at(now_ms) {
                    return Ok(grant);
                }
                expired = true;
            }
        }
        Err(if expired {
            GrantError::ViewGrantExpired
        } else {
            GrantError::ViewGrantMissing
        })
    }

    /// View + control + lease, in that order. Nothing injects without all
    /// three and an unlocked screen.
    pub fn authorize_control(
        &self,
        device_id: &DeviceId,
        session_id: &DomainId,
        media_session_id: &MediaSessionId,
        now_ms: i64,
    ) -> Result<&ControllerLease, GrantError> {
        self.authorize_view(device_id, session_id, media_session_id, now_ms)?;
        if self.locked.contains(media_session_id) {
            return Err(GrantError::Locked);
        }
        let control = self.live_control_for(device_id, session_id, media_session_id, now_ms)?;
        let lease = self
            .leases
            .get(media_session_id)
            .ok_or(GrantError::LeaseMissing)?;
        if !lease.is_live_at(now_ms) {
            return Err(GrantError::LeaseExpired);
        }
        if &lease.device_id != device_id || &lease.session_id != session_id {
            return Err(GrantError::LeaseMismatch);
        }
        if lease.control_grant_id != control.id {
            return Err(GrantError::ControlGrantMismatch);
        }
        Ok(lease)
    }

    /// Same decision as [`Self::authorize_control`], but the batch must name
    /// the current lease. A stale id from before pause/reconnect is refused.
    pub fn authorize_control_with_lease(
        &self,
        device_id: &DeviceId,
        session_id: &DomainId,
        media_session_id: &MediaSessionId,
        lease_id: &LeaseId,
        now_ms: i64,
    ) -> Result<&ControllerLease, GrantError> {
        let lease = self.authorize_control(device_id, session_id, media_session_id, now_ms)?;
        if &lease.id != lease_id {
            return Err(GrantError::LeaseMismatch);
        }
        Ok(lease)
    }

    /// Locks the screen: paused and no controller lease survives.
    pub fn lock_media(&mut self, media_session_id: &MediaSessionId) -> bool {
        self.locked.insert(media_session_id.clone());
        self.leases.remove(media_session_id).is_some()
    }

    /// Unlocks the screen without resuming media or restoring control.
    pub fn unlock_media(&mut self, media_session_id: &MediaSessionId) {
        self.locked.remove(media_session_id);
    }

    #[must_use]
    pub fn is_locked(&self, media_session_id: &MediaSessionId) -> bool {
        self.locked.contains(media_session_id)
    }

    /// Terminal stop: all grants and the lease for this media session are
    /// withdrawn and no new grant can be issued for the same id.
    pub fn stop_media(&mut self, media_session_id: &MediaSessionId) -> usize {
        self.stopped.insert(media_session_id.clone());
        self.locked.remove(media_session_id);
        let removed_views: Vec<_> = self
            .view_grants
            .iter()
            .filter(|(_, grant)| &grant.media_session_id == media_session_id)
            .map(|(id, _)| id.clone())
            .collect();
        let mut removed = 0;
        for id in removed_views {
            if self.revoke_view(&id) {
                removed += 1;
            }
        }
        let removed_controls: Vec<_> = self
            .control_grants
            .iter()
            .filter(|(_, grant)| &grant.media_session_id == media_session_id)
            .map(|(id, _)| id.clone())
            .collect();
        for id in removed_controls {
            if self.revoke_control(&id) {
                removed += 1;
            }
        }
        if self.leases.remove(media_session_id).is_some() {
            removed += 1;
        }
        removed
    }

    /// Revokes a view grant and cascades its control grants and lease.
    pub fn revoke_view(&mut self, grant_id: &GrantId) -> bool {
        if self.view_grants.remove(grant_id).is_none() {
            return false;
        }
        let controls: Vec<_> = self
            .control_grants
            .iter()
            .filter(|(_, grant)| &grant.view_grant_id == grant_id)
            .map(|(id, _)| id.clone())
            .collect();
        for control in controls {
            self.revoke_control(&control);
        }
        true
    }

    /// Revokes a control grant and its lease. The view grant is untouched.
    pub fn revoke_control(&mut self, grant_id: &GrantId) -> bool {
        let Some(control) = self.control_grants.remove(grant_id) else {
            return false;
        };
        if self
            .leases
            .get(&control.media_session_id)
            .is_some_and(|lease| &lease.control_grant_id == grant_id)
        {
            self.leases.remove(&control.media_session_id);
        }
        true
    }

    /// Device revocation withdraws every view/control grant and lease it held.
    pub fn revoke_device(&mut self, device_id: &DeviceId) -> usize {
        let views: Vec<_> = self
            .view_grants
            .iter()
            .filter(|(_, grant)| &grant.device_id == device_id)
            .map(|(id, _)| id.clone())
            .collect();
        let mut removed = 0;
        for view in views {
            if self.revoke_view(&view) {
                removed += 1;
            }
        }
        let controls: Vec<_> = self
            .control_grants
            .iter()
            .filter(|(_, grant)| &grant.device_id == device_id)
            .map(|(id, _)| id.clone())
            .collect();
        for control in controls {
            if self.revoke_control(&control) {
                removed += 1;
            }
        }
        let leases: Vec<_> = self
            .leases
            .iter()
            .filter(|(_, lease)| &lease.device_id == device_id)
            .map(|(media, _)| media.clone())
            .collect();
        for media in leases {
            if self.leases.remove(&media).is_some() {
                removed += 1;
            }
        }
        removed
    }

    #[must_use]
    pub fn is_stopped(&self, media_session_id: &MediaSessionId) -> bool {
        self.stopped.contains(media_session_id)
    }

    pub fn view_grants(&self) -> impl Iterator<Item = &ViewGrant> {
        self.view_grants.values()
    }

    pub fn control_grants(&self) -> impl Iterator<Item = &ControlGrant> {
        self.control_grants.values()
    }

    #[must_use]
    pub fn controller_lease(&self, media_session_id: &MediaSessionId) -> Option<&ControllerLease> {
        self.leases.get(media_session_id)
    }

    fn duplicate_view(
        &self,
        device_id: &DeviceId,
        session_id: &DomainId,
        media_session_id: &MediaSessionId,
        now_ms: i64,
    ) -> bool {
        self.view_grants.values().any(|grant| {
            &grant.device_id == device_id
                && &grant.session_id == session_id
                && &grant.media_session_id == media_session_id
                && grant.is_live_at(now_ms)
        })
    }

    fn duplicate_control(
        &self,
        device_id: &DeviceId,
        session_id: &DomainId,
        media_session_id: &MediaSessionId,
        now_ms: i64,
    ) -> bool {
        self.control_grants.values().any(|grant| {
            &grant.device_id == device_id
                && &grant.session_id == session_id
                && &grant.media_session_id == media_session_id
                && grant.is_live_at(now_ms)
        })
    }

    fn live_view(&self, grant_id: &GrantId, now_ms: i64) -> Result<&ViewGrant, GrantError> {
        let grant = self
            .view_grants
            .get(grant_id)
            .ok_or(GrantError::ViewGrantMissing)?;
        if !grant.is_live_at(now_ms) {
            return Err(GrantError::ViewGrantExpired);
        }
        Ok(grant)
    }

    fn live_control(&self, grant_id: &GrantId, now_ms: i64) -> Result<&ControlGrant, GrantError> {
        let grant = self
            .control_grants
            .get(grant_id)
            .ok_or(GrantError::ControlGrantMissing)?;
        if !grant.is_live_at(now_ms) {
            return Err(GrantError::ControlGrantExpired);
        }
        Ok(grant)
    }

    fn live_control_for(
        &self,
        device_id: &DeviceId,
        session_id: &DomainId,
        media_session_id: &MediaSessionId,
        now_ms: i64,
    ) -> Result<&ControlGrant, GrantError> {
        let mut expired = false;
        for grant in self.control_grants.values() {
            if &grant.device_id == device_id
                && &grant.session_id == session_id
                && &grant.media_session_id == media_session_id
            {
                if grant.is_live_at(now_ms) {
                    return Ok(grant);
                }
                expired = true;
            }
        }
        Err(if expired {
            GrantError::ControlGrantExpired
        } else {
            GrantError::ControlGrantMissing
        })
    }

    fn purge_expired(&mut self, now_ms: i64) {
        self.view_grants.retain(|_, grant| grant.is_live_at(now_ms));
        let live_views: BTreeSet<_> = self
            .view_grants
            .iter()
            .filter(|(_, grant)| grant.is_live_at(now_ms))
            .map(|(id, _)| id.clone())
            .collect();
        self.control_grants.retain(|_, grant| {
            grant.is_live_at(now_ms) && live_views.contains(&grant.view_grant_id)
        });
        let live_controls: BTreeSet<_> = self
            .control_grants
            .iter()
            .filter(|(_, grant)| grant.is_live_at(now_ms))
            .map(|(id, _)| id.clone())
            .collect();
        self.leases.retain(|_, lease| {
            lease.is_live_at(now_ms) && live_controls.contains(&lease.control_grant_id)
        });
    }
}
