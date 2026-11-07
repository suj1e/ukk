//! Device grant issuance on top of the S1 trust registry.
//!
//! Pairing never implies a blanket permission set. The owning user approves an
//! explicit subset of the scopes the device requested, every grant carries an
//! absolute expiry, and `device.manage` is not grantable through the pairing
//! flow. Revocation and key rotation are delegated to
//! [`ukk_protocol::TrustRegistry`], whose live lookup invalidates existing
//! sessions on their next command.

use crate::error::PairingError;
use crate::invite::HostId;
use crate::keys::PublicKey;
use serde::{Deserialize, Serialize};
use ukk_protocol::{
    AuthenticatedDevice, Denial, DeviceClaim, DeviceId, DeviceStatus, IdentityGeneration,
    KeyFingerprint, Scope, ScopeSet, TrustRegistry,
};

/// Default lifetime granted to a newly paired device: 30 days.
pub const DEFAULT_GRANT_TTL_MS: i64 = 30 * 24 * 60 * 60 * 1000;
/// Maximum lifetime any pairing grant may have: 90 days.
pub const MAX_GRANT_TTL_MS: i64 = 90 * 24 * 60 * 60 * 1000;

/// Scopes the local user may approve during pairing.
///
/// `device.manage` is deliberately excluded: a remotely paired device must not
/// be able to enroll, rotate or revoke other devices. That authority stays with
/// the local desktop owner.
pub const PAIRING_GRANTABLE_SCOPES: &[Scope] = &[
    Scope::SessionObserve,
    Scope::SessionManage,
    Scope::AgentSend,
    Scope::ApprovalRespond,
    Scope::ScreenView,
    Scope::ScreenControl,
    Scope::PluginInvoke,
];

/// Returns whether a scope can be granted by the pairing flow.
#[must_use]
pub fn is_pairing_grantable(scope: Scope) -> bool {
    PAIRING_GRANTABLE_SCOPES.contains(&scope)
}

/// Validates a requested or approved scope list without widening it.
pub fn validate_pairing_scopes(scopes: &[Scope]) -> Result<ScopeSet, PairingError> {
    let mut set = ScopeSet::empty();
    for scope in scopes {
        if !is_pairing_grantable(*scope) {
            return Err(PairingError::ScopeNotGrantable);
        }
        set.insert(*scope);
    }
    Ok(set)
}

/// Validates an absolute grant lifetime.
pub fn validate_grant_ttl(ttl_ms: i64) -> Result<(), PairingError> {
    if ttl_ms <= 0 || ttl_ms > MAX_GRANT_TTL_MS {
        return Err(PairingError::InvalidTtl);
    }
    Ok(())
}

/// A paired device plus the grants issued at confirmation time.
///
/// This is the authenticated payload returned to the phone over the Noise
/// transport channel after the local user confirms the short code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairedDevice {
    pub host_id: HostId,
    pub device_id: DeviceId,
    pub public_key: PublicKey,
    pub fingerprint: KeyFingerprint,
    pub generation: IdentityGeneration,
    pub scopes: ScopeSet,
    pub grant_expires_at_ms: i64,
    pub paired_at_ms: i64,
}

impl PairedDevice {
    #[must_use]
    pub fn is_grant_expired_at(&self, now_ms: i64) -> bool {
        now_ms >= self.grant_expires_at_ms
    }
}

/// Host-side device trust table and pairing grant policy.
#[derive(Debug, Default)]
pub struct DeviceTrust {
    registry: TrustRegistry,
}

impl DeviceTrust {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn registry(&self) -> &TrustRegistry {
        &self.registry
    }

    /// Registers a device and issues the approved scopes at one absolute
    /// expiry. Registration and grants are separate protocol operations; a
    /// failure after registration revokes the fresh registration.
    pub fn approve(
        &mut self,
        host_id: &HostId,
        device_id: DeviceId,
        public_key: PublicKey,
        scopes: &[Scope],
        ttl_ms: i64,
        now_ms: i64,
    ) -> Result<PairedDevice, PairingError> {
        validate_grant_ttl(ttl_ms)?;
        let approved = validate_pairing_scopes(scopes)?;
        let fingerprint = public_key.fingerprint()?;
        let expires_at_ms = now_ms.saturating_add(ttl_ms);
        let device = PairedDevice {
            host_id: host_id.clone(),
            device_id,
            public_key,
            fingerprint,
            generation: IdentityGeneration::FIRST,
            scopes: approved,
            grant_expires_at_ms: expires_at_ms,
            paired_at_ms: now_ms,
        };
        self.commit(&device, now_ms)?;
        Ok(device)
    }

    /// Commits a prospective paired-device record to the live trust registry.
    ///
    /// This is the only place the pairing flow turns a prospective
    /// `PairedDevice` into authority. It is called after the phone returns the
    /// encrypted completion acknowledgement, so a dropped success frame cannot
    /// leave the Host granting a device the phone never accepted.
    pub fn commit(&mut self, device: &PairedDevice, now_ms: i64) -> Result<(), PairingError> {
        if device.generation != IdentityGeneration::FIRST {
            return Err(PairingError::StaleIdentity);
        }
        if device.fingerprint != device.public_key.fingerprint()? {
            return Err(PairingError::KeyFingerprintMismatch);
        }
        if now_ms >= device.grant_expires_at_ms {
            return Err(PairingError::GrantExpired);
        }
        if device.grant_expires_at_ms.saturating_sub(now_ms) > MAX_GRANT_TTL_MS {
            return Err(PairingError::InvalidTtl);
        }
        for scope in device.scopes.iter() {
            if !is_pairing_grantable(scope) {
                return Err(PairingError::ScopeNotGrantable);
            }
        }
        let generation = self
            .registry
            .register(device.device_id.clone(), device.fingerprint.clone())
            .map_err(map_registry_error)?;
        for scope in device.scopes.iter() {
            if let Err(error) = self.registry.grant(
                &device.device_id,
                scope,
                generation,
                device.grant_expires_at_ms,
            ) {
                // The device was just registered; a grant failure is an
                // internal inconsistency. Roll the registration back rather
                // than leave a partially authorized device behind.
                let _ = self.registry.revoke(&device.device_id);
                return Err(map_registry_error(error));
            }
        }
        Ok(())
    }

    /// Verifies a handshake claim against the live registry.
    pub fn authenticate(
        &self,
        claim: &DeviceClaim,
        now_ms: i64,
    ) -> Result<AuthenticatedDevice, Denial> {
        self.registry.authenticate(claim, now_ms)
    }

    /// Re-reads grants for an existing session. Revocation, rotation and
    /// expiry take effect here without touching the session object.
    pub fn live_grants(
        &self,
        device: &AuthenticatedDevice,
        now_ms: i64,
    ) -> Result<(ScopeSet, ScopeSet), Denial> {
        self.registry.live_grants(device, now_ms)
    }

    /// Revokes a device and clears every grant.
    ///
    /// Only the local Desktop owner (or a holder of `device.manage`, which the
    /// pairing flow never grants) may call this. The method itself has no
    /// locality parameter; the transport/UI layer must enforce that.
    pub fn revoke(&mut self, device_id: &DeviceId) -> Result<(), PairingError> {
        self.registry.revoke(device_id).map_err(map_registry_error)
    }

    /// Rotates a device key, bumps its generation and clears every grant.
    ///
    /// Same authority rule as [`DeviceTrust::revoke`]: local owner only for the
    /// current pairing model. A revoked device cannot re-register under the
    /// same id; recovery requires a fresh `device_id` and invitation.
    pub fn rotate_key(
        &mut self,
        device_id: &DeviceId,
        new_public_key: PublicKey,
    ) -> Result<IdentityGeneration, PairingError> {
        let fingerprint = new_public_key.fingerprint()?;
        self.registry
            .rotate_key(device_id, fingerprint)
            .map_err(map_registry_error)
    }

    #[must_use]
    pub fn current_generation(&self, device_id: &DeviceId) -> Option<IdentityGeneration> {
        self.registry.current_generation(device_id)
    }

    #[must_use]
    pub fn status(&self, device_id: &DeviceId) -> Option<DeviceStatus> {
        self.registry.status(device_id)
    }
}

fn map_registry_error(error: ukk_protocol::RegistryError) -> PairingError {
    match error {
        ukk_protocol::RegistryError::AlreadyRegistered => PairingError::DeviceAlreadyRegistered,
        ukk_protocol::RegistryError::UnknownDevice => PairingError::UnknownDevice,
        ukk_protocol::RegistryError::Revoked => PairingError::DeviceRevoked,
        ukk_protocol::RegistryError::StaleGrantGeneration => PairingError::StaleIdentity,
        ukk_protocol::RegistryError::SameKey => PairingError::SameKey,
        ukk_protocol::RegistryError::GenerationOverflow => PairingError::GenerationOverflow,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host_id() -> HostId {
        HostId::new("host-1").unwrap()
    }

    fn device_id() -> DeviceId {
        DeviceId::new("device-1").unwrap()
    }

    #[test]
    fn device_manage_is_not_grantable_at_pairing() {
        assert!(!is_pairing_grantable(Scope::DeviceManage));
        assert_eq!(
            validate_pairing_scopes(&[Scope::DeviceManage]),
            Err(PairingError::ScopeNotGrantable)
        );
    }

    #[test]
    fn grant_ttl_is_bounded() {
        assert_eq!(validate_grant_ttl(0), Err(PairingError::InvalidTtl));
        assert_eq!(validate_grant_ttl(-1), Err(PairingError::InvalidTtl));
        assert_eq!(
            validate_grant_ttl(MAX_GRANT_TTL_MS + 1),
            Err(PairingError::InvalidTtl)
        );
        assert!(validate_grant_ttl(MAX_GRANT_TTL_MS).is_ok());
        assert!(validate_grant_ttl(DEFAULT_GRANT_TTL_MS).is_ok());
    }

    #[test]
    fn approval_never_widens_a_scope_set() {
        let allowed = validate_pairing_scopes(&[Scope::SessionObserve]).unwrap();
        assert!(allowed.contains(Scope::SessionObserve));
        assert!(!allowed.contains(Scope::AgentSend));
        assert!(!allowed.contains(Scope::ApprovalRespond));
        assert!(!allowed.contains(Scope::ScreenControl));
    }

    #[test]
    fn revocation_and_rotation_invalidate_existing_sessions() {
        let mut trust = DeviceTrust::new();
        let key = PublicKey::from_bytes([7_u8; 32]);
        let paired = trust
            .approve(
                &host_id(),
                device_id(),
                key,
                &[Scope::SessionObserve, Scope::AgentSend],
                DEFAULT_GRANT_TTL_MS,
                1_000,
            )
            .unwrap();
        let claim = DeviceClaim {
            device_id: paired.device_id.clone(),
            key_fingerprint: paired.fingerprint.clone(),
            generation: paired.generation,
        };
        let authenticated = trust.authenticate(&claim, 2_000).unwrap();
        assert!(trust
            .live_grants(&authenticated, 2_000)
            .unwrap()
            .0
            .contains(Scope::AgentSend));

        trust.revoke(&paired.device_id).unwrap();
        assert_eq!(
            trust.authenticate(&claim, 2_000),
            Err(Denial::DeviceRevoked)
        );
        assert_eq!(
            trust.live_grants(&authenticated, 2_000),
            Err(Denial::DeviceRevoked)
        );
    }

    #[test]
    fn rotation_bumps_generation_clears_grants_and_rejects_the_old_claim() {
        let mut trust = DeviceTrust::new();
        let old_key = PublicKey::from_bytes([8_u8; 32]);
        let paired = trust
            .approve(
                &host_id(),
                device_id(),
                old_key,
                &[Scope::AgentSend],
                DEFAULT_GRANT_TTL_MS,
                1_000,
            )
            .unwrap();
        let authenticated = trust
            .authenticate(
                &DeviceClaim {
                    device_id: paired.device_id.clone(),
                    key_fingerprint: paired.fingerprint.clone(),
                    generation: paired.generation,
                },
                2_000,
            )
            .unwrap();

        let new_key = PublicKey::from_bytes([9_u8; 32]);
        let generation = trust.rotate_key(&paired.device_id, new_key).unwrap();
        assert_eq!(generation.raw(), 2);
        assert_eq!(
            trust.live_grants(&authenticated, 2_000),
            Err(Denial::StaleIdentity)
        );
        assert_eq!(
            trust.authenticate(
                &DeviceClaim {
                    device_id: paired.device_id.clone(),
                    key_fingerprint: paired.fingerprint,
                    generation: paired.generation,
                },
                2_000
            ),
            Err(Denial::StaleIdentity)
        );
        let fresh = trust
            .authenticate(
                &DeviceClaim {
                    device_id: paired.device_id.clone(),
                    key_fingerprint: new_key.fingerprint().unwrap(),
                    generation,
                },
                2_000,
            )
            .unwrap();
        assert!(trust.live_grants(&fresh, 2_000).unwrap().0.is_empty());
    }

    #[test]
    fn grants_expire_at_their_absolute_deadline() {
        let mut trust = DeviceTrust::new();
        let key = PublicKey::from_bytes([6_u8; 32]);
        let paired = trust
            .approve(
                &host_id(),
                device_id(),
                key,
                &[Scope::SessionObserve],
                DEFAULT_GRANT_TTL_MS,
                1_000,
            )
            .unwrap();
        let claim = DeviceClaim {
            device_id: paired.device_id.clone(),
            key_fingerprint: paired.fingerprint.clone(),
            generation: paired.generation,
        };
        let authenticated = trust.authenticate(&claim, 2_000).unwrap();
        let (valid, expired) = trust
            .live_grants(&authenticated, paired.grant_expires_at_ms)
            .unwrap();
        assert!(valid.is_empty());
        assert!(expired.contains(Scope::SessionObserve));
    }
}
