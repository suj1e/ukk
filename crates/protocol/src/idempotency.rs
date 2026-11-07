//! Idempotency keys and typed result classification.
//!
//! Acceptance is not completion. A command can be durably accepted, later
//! complete semantically, or be rejected. All three classifications are
//! retained so a reconnect retry receives the original outcome instead of
//! executing the command again.
//!
//! A key is bound to (command fingerprint, transport plane). Reusing a key for
//! a different command or from a different trust domain is a typed conflict,
//! never a cache hit.

use crate::authz::{Denial, Plane, Principal};
use crate::envelope::Command;
use crate::id::{IdempotencyKey, RequestId};
use crate::limits::{LimitError, Limits};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;

/// Maximum human-readable completion detail stored with a result.
pub const MAX_RESULT_DETAIL_BYTES: usize = 512;

/// Semantic classification of a command's outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum CommandResult {
    /// Durably dispatched. The side effect may still be running.
    Accepted { dispatch_id: RequestId },
    /// Semantically complete.
    Completed { detail: Option<String> },
    /// Rejected by authorization or domain validation.
    Rejected { denial: Denial },
}

/// Correlated result delivered to a client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandResultEnvelope {
    pub request_id: RequestId,
    /// True when this is the stored outcome of an earlier attempt.
    pub replayed: bool,
    pub result: CommandResult,
}

/// Stable fingerprint of a command, ignoring correlation and retry metadata.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RequestFingerprint(String);

impl RequestFingerprint {
    pub fn of(command: &Command) -> Result<Self, IdempotencyError> {
        let encoded = serde_json::to_string(command)
            .map_err(|error| IdempotencyError::Encode(error.to_string()))?;
        Ok(Self(encoded))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Outcome of looking up a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdempotencyAdmission {
    /// First use of this key; the caller may execute.
    Execute,
    /// Retry of an earlier attempt. `None` means the first attempt is still
    /// in flight, so the client must not assume it failed.
    Duplicate { result: Option<CommandResult> },
}

/// Typed idempotency failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdempotencyError {
    FingerprintMismatch,
    CrossPlane,
    CrossPrincipal,
    /// The record's retention window ended. A retry must not execute again;
    /// the caller reconciles the outcome out of band.
    Expired,
    Capacity {
        max: usize,
    },
    UnknownKey,
    AlreadyCompleted,
    InvalidResultDetail,
    Limit(LimitError),
    Encode(String),
}

impl fmt::Display for IdempotencyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FingerprintMismatch => {
                formatter.write_str("idempotency key was used for a different command")
            }
            Self::CrossPlane => {
                formatter.write_str("idempotency key was first used on a different plane")
            }
            Self::CrossPrincipal => {
                formatter.write_str("idempotency key belongs to a different authenticated peer")
            }
            Self::Expired => formatter.write_str(
                "idempotency record expired; reconcile the earlier outcome before retrying",
            ),
            Self::Capacity { max } => {
                write!(formatter, "idempotency store is full ({max} records)")
            }
            Self::UnknownKey => formatter.write_str("idempotency key is not in flight"),
            Self::AlreadyCompleted => formatter.write_str("command result is already recorded"),
            Self::InvalidResultDetail => {
                formatter.write_str("command result detail exceeds the size limit")
            }
            Self::Limit(error) => error.fmt(formatter),
            Self::Encode(message) => write!(formatter, "command fingerprint failed: {message}"),
        }
    }
}

impl Error for IdempotencyError {}

impl From<LimitError> for IdempotencyError {
    fn from(error: LimitError) -> Self {
        Self::Limit(error)
    }
}

#[derive(Debug)]
enum RecordState {
    /// Admitted, side effect not yet classified. Never expires: dropping it
    /// would let a retry execute a second time.
    InFlight,
    Completed {
        result: CommandResult,
        completed_at_ms: i64,
    },
    /// Retention ended. The result is dropped but the key remains as a
    /// tombstone so a late retry is refused instead of re-executed.
    Expired,
}

#[derive(Debug)]
struct Record {
    fingerprint: RequestFingerprint,
    plane: Plane,
    principal: Principal,
    state: RecordState,
}

/// Bounded store of in-flight, completed and expired commands.
///
/// When full, it refuses new keys. Evicting a record would allow the same
/// command to execute twice after a reconnect, so capacity exhaustion is a
/// typed error for the caller to handle (reject or shed load), not a silent
/// correctness change. Completed records become tombstones after retention;
/// in-flight records never expire.
#[derive(Debug)]
pub struct IdempotencyStore {
    limits: Limits,
    records: BTreeMap<IdempotencyKey, Record>,
}

impl IdempotencyStore {
    pub fn new(limits: Limits) -> Result<Self, IdempotencyError> {
        limits.validate()?;
        Ok(Self {
            limits,
            records: BTreeMap::new(),
        })
    }

    pub fn begin(
        &mut self,
        plane: Plane,
        principal: &Principal,
        key: &IdempotencyKey,
        fingerprint: &RequestFingerprint,
        now_ms: i64,
    ) -> Result<IdempotencyAdmission, IdempotencyError> {
        self.purge_expired(now_ms);
        if let Some(record) = self.records.get(key) {
            if record.plane != plane {
                return Err(IdempotencyError::CrossPlane);
            }
            if &record.principal != principal {
                return Err(IdempotencyError::CrossPrincipal);
            }
            if &record.fingerprint != fingerprint {
                return Err(IdempotencyError::FingerprintMismatch);
            }
            return match &record.state {
                RecordState::InFlight => Ok(IdempotencyAdmission::Duplicate { result: None }),
                RecordState::Completed { result, .. } => Ok(IdempotencyAdmission::Duplicate {
                    result: Some(result.clone()),
                }),
                RecordState::Expired => Err(IdempotencyError::Expired),
            };
        }
        if self.records.len() >= self.limits.max_idempotency_records {
            return Err(IdempotencyError::Capacity {
                max: self.limits.max_idempotency_records,
            });
        }
        self.records.insert(
            key.clone(),
            Record {
                fingerprint: fingerprint.clone(),
                plane,
                principal: principal.clone(),
                state: RecordState::InFlight,
            },
        );
        Ok(IdempotencyAdmission::Execute)
    }

    pub fn complete(
        &mut self,
        key: &IdempotencyKey,
        result: CommandResult,
        now_ms: i64,
    ) -> Result<(), IdempotencyError> {
        if let CommandResult::Completed {
            detail: Some(detail),
        } = &result
        {
            if detail.len() > MAX_RESULT_DETAIL_BYTES {
                return Err(IdempotencyError::InvalidResultDetail);
            }
        }
        let record = self
            .records
            .get_mut(key)
            .ok_or(IdempotencyError::UnknownKey)?;
        if !matches!(record.state, RecordState::InFlight) {
            return Err(IdempotencyError::AlreadyCompleted);
        }
        record.state = RecordState::Completed {
            result,
            completed_at_ms: now_ms,
        };
        Ok(())
    }

    /// Converts completed records past retention into tombstones and returns
    /// how many were expired.
    ///
    /// In-flight records are never expired, regardless of age. A completed
    /// record's window starts at completion, not first attempt, so a slow
    /// command still has a full retry window after it finishes.
    pub fn purge_expired(&mut self, now_ms: i64) -> usize {
        let retention = self.limits.idempotency_retention_ms;
        let mut expired = 0;
        for record in self.records.values_mut() {
            let should_expire = match &record.state {
                RecordState::Completed {
                    completed_at_ms, ..
                } => now_ms.saturating_sub(*completed_at_ms) >= retention,
                RecordState::InFlight | RecordState::Expired => false,
            };
            if should_expire {
                record.state = RecordState::Expired;
                expired += 1;
            }
        }
        expired
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.records.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
}
