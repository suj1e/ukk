//! Durable S1 device trust and scoped grants.
//!
//! A device is stored as `(device_id, public-key fingerprint, generation,
//! status)` plus grants bound to that generation and an absolute expiry. Key
//! rotation and revocation are compare-and-swap operations on the generation,
//! so a stale writer cannot rotate a new key or revoke a device it did not
//! observe. The live authorization decision stays in
//! `ukk_protocol::TrustRegistry`; [`Store::load_trust_registry`] only hydrates
//! that registry from these rows.
//!
//! No private key, pairing secret or session key is written here. The
//! fingerprint is public identity metadata.

use super::*;
use rusqlite::{params, OptionalExtension};
use std::collections::BTreeMap;
use ukk_protocol::{
    DeviceId, DeviceStatus, IdentityGeneration, KeyFingerprint, RegistryError, RestoredDevice,
    Scope, ScopedGrant, TrustRegistry,
};

fn generation_to_i64(generation: IdentityGeneration) -> Result<i64> {
    i64::try_from(generation.raw())
        .map_err(|_| StoreError::Invalid("identity generation exceeds storage range".into()))
}

fn generation_from_i64(raw: i64) -> Result<IdentityGeneration> {
    let raw = u64::try_from(raw).map_err(|_| {
        StoreError::Invalid("identity generation is outside the stored range".into())
    })?;
    if raw == 0 {
        return Err(StoreError::Invalid(
            "identity generation must be at least 1".into(),
        ));
    }
    Ok(IdentityGeneration::from_raw(raw))
}

impl Store {
    /// Registers a device at generation 1 with no grants. Re-registering the
    /// same id is refused; pairing must rotate or revoke explicitly.
    pub fn register_device(
        &mut self,
        device_id: &DeviceId,
        fingerprint: &KeyFingerprint,
        now_ms: i64,
    ) -> Result<IdentityGeneration> {
        let changed = self.conn.execute(
            "INSERT INTO devices(device_id,key_fingerprint,generation,revoked,created_ms,updated_ms)
             VALUES(?1,?2,1,0,?3,?3)
             ON CONFLICT(device_id) DO NOTHING",
            params![device_id.as_str(), fingerprint.as_str(), now_ms],
        )?;
        if changed == 0 {
            return Err(StoreError::Trust(RegistryError::AlreadyRegistered));
        }
        Ok(IdentityGeneration::FIRST)
    }

    /// Adds or replaces a grant for the device's current generation. A grant
    /// for a superseded generation is refused rather than stored.
    pub fn grant_device_scope(
        &mut self,
        device_id: &DeviceId,
        scope: Scope,
        identity_generation: IdentityGeneration,
        expires_at_ms: i64,
        now_ms: i64,
    ) -> Result<()> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let row: Option<(i64, i64)> = tx
            .query_row(
                "SELECT generation,revoked FROM devices WHERE device_id=?1",
                params![device_id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((generation, revoked)) = row else {
            return Err(StoreError::Trust(RegistryError::UnknownDevice));
        };
        if revoked != 0 {
            return Err(StoreError::Trust(RegistryError::Revoked));
        }
        if generation_from_i64(generation)? != identity_generation {
            return Err(StoreError::Trust(RegistryError::StaleGrantGeneration));
        }
        tx.execute(
            "INSERT INTO device_grants(device_id,scope,identity_generation,expires_at_ms)
             VALUES(?1,?2,?3,?4)
             ON CONFLICT(device_id,scope) DO UPDATE SET
                identity_generation=excluded.identity_generation,
                expires_at_ms=excluded.expires_at_ms",
            params![
                device_id.as_str(),
                scope.as_str(),
                generation_to_i64(identity_generation)?,
                expires_at_ms
            ],
        )?;
        tx.execute(
            "UPDATE devices SET updated_ms=?2 WHERE device_id=?1",
            params![device_id.as_str(), now_ms],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Revokes a device and clears every grant. `expected_generation` is the
    /// compare-and-swap guard: a stale caller cannot revoke a device after its
    /// key rotated. Revoking an already-revoked device at the same generation
    /// is idempotent, matching `TrustRegistry::revoke`.
    pub fn revoke_device(
        &mut self,
        device_id: &DeviceId,
        expected_generation: IdentityGeneration,
        now_ms: i64,
    ) -> Result<()> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let row: Option<i64> = tx
            .query_row(
                "SELECT generation FROM devices WHERE device_id=?1",
                params![device_id.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        let Some(generation) = row else {
            return Err(StoreError::Trust(RegistryError::UnknownDevice));
        };
        if generation_from_i64(generation)? != expected_generation {
            return Err(StoreError::CasConflict);
        }
        tx.execute(
            "UPDATE devices SET revoked=1, updated_ms=?2 WHERE device_id=?1",
            params![device_id.as_str(), now_ms],
        )?;
        tx.execute(
            "DELETE FROM device_grants WHERE device_id=?1",
            params![device_id.as_str()],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Rotates the public-key fingerprint, bumps the generation and clears
    /// every grant atomically. A stale generation is a CAS conflict; rotating
    /// to the registered fingerprint is refused.
    pub fn rotate_device_key(
        &mut self,
        device_id: &DeviceId,
        expected_generation: IdentityGeneration,
        new_fingerprint: &KeyFingerprint,
        now_ms: i64,
    ) -> Result<IdentityGeneration> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let row: Option<(String, i64, i64)> = tx
            .query_row(
                "SELECT key_fingerprint,generation,revoked FROM devices WHERE device_id=?1",
                params![device_id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let Some((fingerprint, generation, revoked)) = row else {
            return Err(StoreError::Trust(RegistryError::UnknownDevice));
        };
        if revoked != 0 {
            return Err(StoreError::Trust(RegistryError::Revoked));
        }
        if generation_from_i64(generation)? != expected_generation {
            return Err(StoreError::CasConflict);
        }
        if fingerprint == new_fingerprint.as_str() {
            return Err(StoreError::Trust(RegistryError::SameKey));
        }
        let next = expected_generation
            .raw()
            .checked_add(1)
            .ok_or(StoreError::Trust(RegistryError::GenerationOverflow))?;
        let next = IdentityGeneration::from_raw(next);
        tx.execute(
            "UPDATE devices SET key_fingerprint=?2,generation=?3,updated_ms=?4 WHERE device_id=?1",
            params![
                device_id.as_str(),
                new_fingerprint.as_str(),
                generation_to_i64(next)?,
                now_ms
            ],
        )?;
        tx.execute(
            "DELETE FROM device_grants WHERE device_id=?1",
            params![device_id.as_str()],
        )?;
        tx.commit()?;
        Ok(next)
    }

    /// Returns the current generation, or `None` when the device is unknown.
    pub fn device_generation(&self, device_id: &DeviceId) -> Result<Option<IdentityGeneration>> {
        let row: Option<i64> = self
            .conn
            .query_row(
                "SELECT generation FROM devices WHERE device_id=?1",
                params![device_id.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        row.map(generation_from_i64).transpose()
    }

    /// Loads the persisted grants for one device, including expired grants so
    /// authorization can distinguish `grant_expired` from `missing_scope`.
    pub fn device_grants(&self, device_id: &DeviceId) -> Result<Vec<ScopedGrant>> {
        let mut stmt = self.conn.prepare(
            "SELECT scope,identity_generation,expires_at_ms
             FROM device_grants WHERE device_id=?1 ORDER BY scope",
        )?;
        let rows = stmt.query_map(params![device_id.as_str()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?;
        let mut grants = Vec::new();
        for row in rows {
            let (scope, generation, expires_at_ms) = row?;
            let scope = Scope::from_wire(&scope).ok_or_else(|| {
                StoreError::Invalid(format!("stored device scope is unknown: {scope}"))
            })?;
            grants.push(ScopedGrant {
                scope,
                identity_generation: generation_from_i64(generation)?,
                expires_at_ms,
            });
        }
        Ok(grants)
    }

    /// Hydrates the protocol trust registry from durable rows.
    ///
    /// This is the only place the store reconstructs authorization state.
    /// Every device and grant is validated; a grant bound to a stale
    /// generation is a storage error, not a silently widened capability.
    pub fn load_trust_registry(&self) -> Result<TrustRegistry> {
        let tx = self.conn.unchecked_transaction()?;
        let mut grants_by_device: BTreeMap<String, Vec<ScopedGrant>> = BTreeMap::new();
        {
            let mut stmt = tx.prepare(
                "SELECT device_id,scope,identity_generation,expires_at_ms FROM device_grants",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })?;
            for row in rows {
                let (device_id, scope, generation, expires_at_ms) = row?;
                let scope = Scope::from_wire(&scope).ok_or_else(|| {
                    StoreError::Invalid(format!("stored device scope is unknown: {scope}"))
                })?;
                grants_by_device
                    .entry(device_id)
                    .or_default()
                    .push(ScopedGrant {
                        scope,
                        identity_generation: generation_from_i64(generation)?,
                        expires_at_ms,
                    });
            }
        }
        let devices = {
            let mut stmt = tx.prepare(
                "SELECT device_id,key_fingerprint,generation,revoked
                 FROM devices ORDER BY device_id",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })?;
            let mut devices = Vec::new();
            for row in rows {
                let (device_id, fingerprint, generation, revoked) = row?;
                let device_id = DeviceId::new(device_id).map_err(|error| {
                    StoreError::Invalid(format!("stored device id is invalid: {error}"))
                })?;
                let key_fingerprint = KeyFingerprint::new(fingerprint).map_err(|error| {
                    StoreError::Invalid(format!("stored device fingerprint is invalid: {error}"))
                })?;
                let generation = generation_from_i64(generation)?;
                let status = if revoked != 0 {
                    DeviceStatus::Revoked
                } else {
                    DeviceStatus::Active
                };
                let grants = grants_by_device
                    .remove(device_id.as_str())
                    .unwrap_or_default();
                devices.push(RestoredDevice {
                    device_id,
                    key_fingerprint,
                    generation,
                    status,
                    grants,
                });
            }
            devices
        };
        tx.commit()?;
        TrustRegistry::restore(devices).map_err(StoreError::Trust)
    }
}
