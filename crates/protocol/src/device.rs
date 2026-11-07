//! Device identity, key generations and scoped grants.
//!
//! A device id alone is not an identity. Authentication binds the claimed
//! device id to a registered public-key fingerprint and to the generation of
//! that key. Key rotation bumps the generation, drops every grant, and
//! invalidates sessions minted before the rotation. Revocation is terminal for
//! that registration.

use crate::authz::Denial;
use crate::id::{DeviceId, KeyFingerprint};
use crate::scope::{Scope, ScopeSet};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;

/// Monotonic key generation starting at 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct IdentityGeneration(u64);

impl IdentityGeneration {
    pub const FIRST: Self = Self(1);

    #[must_use]
    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    #[must_use]
    pub const fn raw(self) -> u64 {
        self.0
    }

    fn next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }
}

impl fmt::Display for IdentityGeneration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "identity generation {}", self.0)
    }
}

/// Device lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceStatus {
    Active,
    Revoked,
}

/// One scoped, expiring grant bound to a key generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopedGrant {
    pub scope: Scope,
    pub identity_generation: IdentityGeneration,
    pub expires_at_ms: i64,
}

impl ScopedGrant {
    #[must_use]
    pub fn is_valid_at(&self, generation: IdentityGeneration, now_ms: i64) -> bool {
        self.identity_generation == generation && now_ms < self.expires_at_ms
    }
}

/// Wire claim presented during the handshake. The key fingerprint is compared
/// against the registered value; it is never trusted on its own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceClaim {
    pub device_id: DeviceId,
    pub key_fingerprint: KeyFingerprint,
    pub generation: IdentityGeneration,
}

/// A verified device plus the grants that were valid at authentication time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthenticatedDevice {
    pub device_id: DeviceId,
    pub generation: IdentityGeneration,
    pub scopes: ScopeSet,
    pub expired_scopes: ScopeSet,
}

/// Typed trust-registry failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistryError {
    AlreadyRegistered,
    UnknownDevice,
    Revoked,
    StaleGrantGeneration,
    SameKey,
    GenerationOverflow,
}

impl fmt::Display for RegistryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyRegistered => formatter.write_str("device is already registered"),
            Self::UnknownDevice => formatter.write_str("device is not registered"),
            Self::Revoked => formatter.write_str("device is revoked"),
            Self::StaleGrantGeneration => {
                formatter.write_str("grant targets a superseded identity generation")
            }
            Self::SameKey => formatter.write_str("rotated key equals the registered key"),
            Self::GenerationOverflow => formatter.write_str("identity generation overflow"),
        }
    }
}

impl Error for RegistryError {}

#[derive(Debug)]
struct DeviceRecord {
    fingerprint: KeyFingerprint,
    generation: IdentityGeneration,
    status: DeviceStatus,
    grants: BTreeMap<Scope, ScopedGrant>,
}

fn grant_sets(record: &DeviceRecord, now_ms: i64) -> (ScopeSet, ScopeSet) {
    if record.status == DeviceStatus::Revoked {
        return (ScopeSet::empty(), ScopeSet::empty());
    }
    let mut valid = ScopeSet::empty();
    let mut expired = ScopeSet::empty();
    for grant in record.grants.values() {
        if grant.identity_generation != record.generation {
            continue;
        }
        if now_ms < grant.expires_at_ms {
            valid.insert(grant.scope);
        } else {
            expired.insert(grant.scope);
        }
    }
    (valid, expired)
}

/// In-memory model of the durable device trust table.
///
/// The Host persists the same fields in SQLite; this registry owns the
/// authorization decisions so transports cannot re-implement them.
#[derive(Debug, Default)]
pub struct TrustRegistry {
    devices: BTreeMap<DeviceId, DeviceRecord>,
}

impl TrustRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a device at generation 1 with no grants.
    pub fn register(
        &mut self,
        device_id: DeviceId,
        fingerprint: KeyFingerprint,
    ) -> Result<IdentityGeneration, RegistryError> {
        if self.devices.contains_key(&device_id) {
            return Err(RegistryError::AlreadyRegistered);
        }
        self.devices.insert(
            device_id,
            DeviceRecord {
                fingerprint,
                generation: IdentityGeneration::FIRST,
                status: DeviceStatus::Active,
                grants: BTreeMap::new(),
            },
        );
        Ok(IdentityGeneration::FIRST)
    }

    /// Adds or replaces a grant for the device's current generation only.
    pub fn grant(
        &mut self,
        device_id: &DeviceId,
        scope: Scope,
        identity_generation: IdentityGeneration,
        expires_at_ms: i64,
    ) -> Result<(), RegistryError> {
        let record = self
            .devices
            .get_mut(device_id)
            .ok_or(RegistryError::UnknownDevice)?;
        if record.status == DeviceStatus::Revoked {
            return Err(RegistryError::Revoked);
        }
        if identity_generation != record.generation {
            return Err(RegistryError::StaleGrantGeneration);
        }
        record.grants.insert(
            scope,
            ScopedGrant {
                scope,
                identity_generation,
                expires_at_ms,
            },
        );
        Ok(())
    }

    /// Revokes a device and all of its grants. Existing sessions fail on their
    /// next command because authorization re-reads this registry.
    pub fn revoke(&mut self, device_id: &DeviceId) -> Result<(), RegistryError> {
        let record = self
            .devices
            .get_mut(device_id)
            .ok_or(RegistryError::UnknownDevice)?;
        record.status = DeviceStatus::Revoked;
        record.grants.clear();
        Ok(())
    }

    /// Rotates a device key, bumps its generation and clears every grant.
    pub fn rotate_key(
        &mut self,
        device_id: &DeviceId,
        new_fingerprint: KeyFingerprint,
    ) -> Result<IdentityGeneration, RegistryError> {
        let record = self
            .devices
            .get_mut(device_id)
            .ok_or(RegistryError::UnknownDevice)?;
        if record.status == DeviceStatus::Revoked {
            return Err(RegistryError::Revoked);
        }
        if record.fingerprint == new_fingerprint {
            return Err(RegistryError::SameKey);
        }
        let next = record
            .generation
            .next()
            .ok_or(RegistryError::GenerationOverflow)?;
        record.fingerprint = new_fingerprint;
        record.generation = next;
        record.grants.clear();
        Ok(next)
    }

    #[must_use]
    pub fn current_generation(&self, device_id: &DeviceId) -> Option<IdentityGeneration> {
        self.devices.get(device_id).map(|record| record.generation)
    }

    #[must_use]
    pub fn status(&self, device_id: &DeviceId) -> Option<DeviceStatus> {
        self.devices.get(device_id).map(|record| record.status)
    }

    /// Verifies a handshake claim and returns the grants valid right now.
    pub fn authenticate(
        &self,
        claim: &DeviceClaim,
        now_ms: i64,
    ) -> Result<AuthenticatedDevice, Denial> {
        let record = self
            .devices
            .get(&claim.device_id)
            .ok_or(Denial::UnknownDevice)?;
        if record.status == DeviceStatus::Revoked {
            return Err(Denial::DeviceRevoked);
        }
        if claim.generation < record.generation {
            return Err(Denial::StaleIdentity);
        }
        if claim.generation > record.generation {
            return Err(Denial::ForgedIdentity);
        }
        if record.fingerprint != claim.key_fingerprint {
            return Err(Denial::ForgedIdentity);
        }
        let (scopes, expired_scopes) = grant_sets(record, now_ms);
        Ok(AuthenticatedDevice {
            device_id: claim.device_id.clone(),
            generation: claim.generation,
            scopes,
            expired_scopes,
        })
    }

    /// Re-evaluates a live session against the current registry.
    ///
    /// Revocation, rotation and grant expiry all take effect on the next
    /// command; no cached permission snapshot is authoritative. Expired grants
    /// are reported separately so authorization can deny with a precise reason
    /// without ever treating an expired grant as valid.
    pub fn live_grants(
        &self,
        device: &AuthenticatedDevice,
        now_ms: i64,
    ) -> Result<(ScopeSet, ScopeSet), Denial> {
        let record = self
            .devices
            .get(&device.device_id)
            .ok_or(Denial::UnknownDevice)?;
        if record.status == DeviceStatus::Revoked {
            return Err(Denial::DeviceRevoked);
        }
        if record.generation != device.generation {
            return Err(Denial::StaleIdentity);
        }
        Ok(grant_sets(record, now_ms))
    }
}
