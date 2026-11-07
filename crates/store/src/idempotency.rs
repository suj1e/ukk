//! Durable S1 idempotency records.
//!
//! A key is bound to `(command fingerprint, plane, authenticated principal)`.
//! `begin` is one `IMMEDIATE` transaction: concurrent retries either insert the
//! in-flight record or observe it, never both execute. In-flight records never
//! expire; completed records become tombstones after the retention window so a
//! late retry is refused instead of executing again. Tombstones still count
//! toward capacity, and capacity refusal is explicit rather than an eviction.
//!
//! Result classification, fingerprinting and error types come from
//! `ukk-protocol`; this module only persists them.

use super::*;
use rusqlite::{params, OptionalExtension};
use ukk_protocol::idempotency::MAX_RESULT_DETAIL_BYTES;
use ukk_protocol::{
    CommandResult, IdempotencyAdmission, IdempotencyError, IdempotencyKey, Plane, Principal,
    RequestFingerprint,
};

const IN_FLIGHT: &str = "in_flight";
const COMPLETED: &str = "completed";
const EXPIRED: &str = "expired";

fn plane_key(plane: Plane) -> &'static str {
    match plane {
        Plane::LocalIpc => "local_ipc",
        Plane::Remote => "remote",
    }
}

fn principal_key(principal: &Principal) -> (&'static str, String) {
    match principal {
        Principal::LocalOwner { uid } => ("local_owner", uid.to_string()),
        Principal::Device(device_id) => ("device", device_id.as_str().to_owned()),
    }
}

/// Converts completed records past retention into tombstones. In-flight
/// records are deliberately untouched regardless of age.
fn purge_expired_tx(
    tx: &rusqlite::Transaction<'_>,
    now_ms: i64,
    retention_ms: i64,
) -> Result<usize> {
    let cutoff = now_ms.saturating_sub(retention_ms);
    Ok(tx.execute(
        "UPDATE idempotency_records
         SET state=?1, result_json=NULL, expired_at_ms=?2
         WHERE state=?3 AND completed_at_ms IS NOT NULL AND completed_at_ms<=?4",
        params![EXPIRED, now_ms, COMPLETED, cutoff],
    )?)
}

impl Store {
    /// Atomically admits a command key.
    ///
    /// Returns [`IdempotencyAdmission::Execute`] exactly once for a new key.
    /// An in-flight retry returns `Duplicate { result: None }`; a completed
    /// retry returns the stored classification; an expired tombstone is a
    /// typed `Expired` refusal. The key's plane, principal and fingerprint are
    /// checked before any result is revealed.
    pub fn begin_idempotent_command(
        &mut self,
        plane: Plane,
        principal: &Principal,
        key: &IdempotencyKey,
        fingerprint: &RequestFingerprint,
        now_ms: i64,
    ) -> Result<IdempotencyAdmission> {
        if fingerprint.as_str().len() > self.limits.max_payload_bytes {
            return Err(StoreError::Idempotency(IdempotencyError::Encode(
                "command fingerprint exceeds the payload limit".into(),
            )));
        }
        let limits = self.limits;
        with_immediate(&mut self.conn, |tx| {
            purge_expired_tx(tx, now_ms, limits.idempotency_retention_ms)?;
            let existing: Option<(String, String, String, String, String, Option<String>)> = tx
                .query_row(
                    "SELECT fingerprint,plane,principal_kind,principal_id,state,result_json
                     FROM idempotency_records WHERE key=?1",
                    params![key.as_str()],
                    |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                            row.get(5)?,
                        ))
                    },
                )
                .optional()?;
            if let Some((stored_fingerprint, stored_plane, stored_kind, stored_id, state, result)) =
                existing
            {
                let (incoming_plane, incoming_kind, incoming_id) = {
                    let (kind, id) = principal_key(principal);
                    (plane_key(plane), kind, id)
                };
                if stored_plane != incoming_plane {
                    return Err(StoreError::Idempotency(IdempotencyError::CrossPlane));
                }
                if stored_kind != incoming_kind || stored_id != incoming_id {
                    return Err(StoreError::Idempotency(IdempotencyError::CrossPrincipal));
                }
                if stored_fingerprint != fingerprint.as_str() {
                    return Err(StoreError::Idempotency(
                        IdempotencyError::FingerprintMismatch,
                    ));
                }
                return match state.as_str() {
                    IN_FLIGHT => Ok(IdempotencyAdmission::Duplicate { result: None }),
                    COMPLETED => {
                        let encoded = result.ok_or_else(|| {
                            StoreError::Invalid(
                                "completed idempotency record has no stored result".into(),
                            )
                        })?;
                        let result: CommandResult = serde_json::from_str(&encoded)?;
                        Ok(IdempotencyAdmission::Duplicate {
                            result: Some(result),
                        })
                    }
                    EXPIRED => Err(StoreError::Idempotency(IdempotencyError::Expired)),
                    other => Err(StoreError::Invalid(format!(
                        "unknown idempotency state: {other}"
                    ))),
                };
            }
            let count: i64 =
                tx.query_row("SELECT COUNT(*) FROM idempotency_records", [], |row| {
                    row.get(0)
                })?;
            let max = i64::try_from(limits.max_idempotency_records).unwrap_or(i64::MAX);
            if count >= max {
                return Err(StoreError::Idempotency(IdempotencyError::Capacity {
                    max: limits.max_idempotency_records,
                }));
            }
            let (kind, id) = principal_key(principal);
            tx.execute(
                "INSERT INTO idempotency_records(
                    key,fingerprint,plane,principal_kind,principal_id,state,
                    result_json,created_ms,completed_at_ms,expired_at_ms
                 ) VALUES(?1,?2,?3,?4,?5,?6,NULL,?7,NULL,NULL)",
                params![
                    key.as_str(),
                    fingerprint.as_str(),
                    plane_key(plane),
                    kind,
                    id,
                    IN_FLIGHT,
                    now_ms
                ],
            )?;
            Ok(IdempotencyAdmission::Execute)
        })
    }

    /// Records the final classification for an in-flight command. A second
    /// completion, an unknown key or an oversized detail is refused.
    pub fn finish_idempotent_command(
        &mut self,
        key: &IdempotencyKey,
        result: CommandResult,
        now_ms: i64,
    ) -> Result<()> {
        if let CommandResult::Completed {
            detail: Some(detail),
        } = &result
        {
            if detail.len() > MAX_RESULT_DETAIL_BYTES {
                return Err(StoreError::Idempotency(
                    IdempotencyError::InvalidResultDetail,
                ));
            }
        }
        let encoded = serde_json::to_string(&result)?;
        if encoded.len() > self.limits.max_payload_bytes {
            return Err(StoreError::Idempotency(IdempotencyError::Encode(
                "command result exceeds the payload limit".into(),
            )));
        }
        with_immediate(&mut self.conn, |tx| {
            let state: Option<String> = tx
                .query_row(
                    "SELECT state FROM idempotency_records WHERE key=?1",
                    params![key.as_str()],
                    |row| row.get(0),
                )
                .optional()?;
            match state.as_deref() {
                None => {
                    return Err(StoreError::Idempotency(IdempotencyError::UnknownKey));
                }
                Some(IN_FLIGHT) => {}
                Some(_) => {
                    return Err(StoreError::Idempotency(IdempotencyError::AlreadyCompleted));
                }
            }
            tx.execute(
                "UPDATE idempotency_records
                 SET state=?2,result_json=?3,completed_at_ms=?4
                 WHERE key=?1 AND state=?5",
                params![key.as_str(), COMPLETED, encoded, now_ms, IN_FLIGHT],
            )?;
            Ok(())
        })
    }

    /// Converts completed records past retention into tombstones and returns
    /// how many transitioned. In-flight records never expire.
    pub fn purge_expired_idempotency(&mut self, now_ms: i64) -> Result<usize> {
        let retention = self.limits.idempotency_retention_ms;
        with_immediate(&mut self.conn, |tx| purge_expired_tx(tx, now_ms, retention))
    }

    /// Number of stored keys, including in-flight records and tombstones.
    pub fn idempotency_record_count(&self) -> Result<usize> {
        let count: i64 =
            self.conn
                .query_row("SELECT COUNT(*) FROM idempotency_records", [], |row| {
                    row.get(0)
                })?;
        usize::try_from(count)
            .map_err(|_| StoreError::Invalid("idempotency record count is invalid".into()))
    }
}
