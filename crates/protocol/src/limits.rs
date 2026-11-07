//! Explicit resource bounds for a WAN-facing protocol.
//!
//! Every bound is enforced on the receiving side before work is scheduled.
//! Bounds are policy, not hints: a transport that decodes first and limits
//! later has already spent the budget an attacker is trying to exhaust.
//!
//! Retention interacts with correctness. The idempotency store refuses new
//! entries instead of silently evicting them; eviction would turn a duplicate
//! command into a second execution after reconnect.

use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fmt;

/// Bounded protocol resource policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    /// Maximum encoded size of one envelope or wire message.
    pub max_envelope_bytes: usize,
    /// Maximum encoded size of a command or event payload.
    pub max_payload_bytes: usize,
    /// Maximum encoded size of a snapshot body.
    pub max_snapshot_bytes: usize,
    /// Maximum number of commands waiting for reconnect delivery.
    pub max_queue_depth: usize,
    /// Maximum number of durable events kept for replay.
    pub max_retained_events: usize,
    /// Maximum number of remembered idempotency keys.
    pub max_idempotency_records: usize,
    /// How long an idempotency result stays authoritative.
    pub idempotency_retention_ms: i64,
    /// Maximum number of events returned by one replay request.
    pub max_replay_batch: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_envelope_bytes: 64 * 1024,
            max_payload_bytes: 32 * 1024,
            max_snapshot_bytes: 4 * 1024 * 1024,
            max_queue_depth: 256,
            max_retained_events: 4096,
            max_idempotency_records: 4096,
            idempotency_retention_ms: 10 * 60 * 1000,
            max_replay_batch: 256,
        }
    }
}

/// Typed limit violation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitError {
    EnvelopeTooLarge { actual: usize, max: usize },
    PayloadTooLarge { actual: usize, max: usize },
    SnapshotTooLarge { actual: usize, max: usize },
    QueueFull { max: usize },
    ReplayBatchTooLarge { requested: usize, max: usize },
    IdempotencyCapacity { max: usize },
    InvalidPolicy(&'static str),
}

impl fmt::Display for LimitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EnvelopeTooLarge { actual, max } => {
                write!(formatter, "envelope is {actual} bytes, limit is {max}")
            }
            Self::PayloadTooLarge { actual, max } => {
                write!(formatter, "payload is {actual} bytes, limit is {max}")
            }
            Self::SnapshotTooLarge { actual, max } => {
                write!(formatter, "snapshot is {actual} bytes, limit is {max}")
            }
            Self::QueueFull { max } => write!(formatter, "reconnect queue is full ({max})"),
            Self::ReplayBatchTooLarge { requested, max } => {
                write!(formatter, "replay batch {requested} exceeds limit {max}")
            }
            Self::IdempotencyCapacity { max } => {
                write!(formatter, "idempotency store is full ({max} records)")
            }
            Self::InvalidPolicy(reason) => write!(formatter, "invalid protocol limits: {reason}"),
        }
    }
}

impl Error for LimitError {}

impl Limits {
    /// Rejects policies that would make the protocol unsafe or unenforceable.
    pub fn validate(&self) -> Result<(), LimitError> {
        if self.max_envelope_bytes == 0 {
            return Err(LimitError::InvalidPolicy("max_envelope_bytes is zero"));
        }
        if self.max_payload_bytes == 0 || self.max_payload_bytes > self.max_envelope_bytes {
            return Err(LimitError::InvalidPolicy(
                "max_payload_bytes must be > 0 and <= max_envelope_bytes",
            ));
        }
        if self.max_snapshot_bytes == 0 {
            return Err(LimitError::InvalidPolicy("max_snapshot_bytes is zero"));
        }
        if self.max_queue_depth == 0 {
            return Err(LimitError::InvalidPolicy("max_queue_depth is zero"));
        }
        if self.max_retained_events == 0 {
            return Err(LimitError::InvalidPolicy("max_retained_events is zero"));
        }
        if self.max_idempotency_records == 0 {
            return Err(LimitError::InvalidPolicy("max_idempotency_records is zero"));
        }
        if self.idempotency_retention_ms <= 0 {
            return Err(LimitError::InvalidPolicy(
                "idempotency_retention_ms must be positive",
            ));
        }
        if self.max_replay_batch == 0 || self.max_replay_batch > self.max_retained_events {
            return Err(LimitError::InvalidPolicy(
                "max_replay_batch must be > 0 and <= max_retained_events",
            ));
        }
        Ok(())
    }

    pub fn check_envelope(&self, actual: usize) -> Result<(), LimitError> {
        if actual > self.max_envelope_bytes {
            return Err(LimitError::EnvelopeTooLarge {
                actual,
                max: self.max_envelope_bytes,
            });
        }
        Ok(())
    }

    pub fn check_payload(&self, actual: usize) -> Result<(), LimitError> {
        if actual > self.max_payload_bytes {
            return Err(LimitError::PayloadTooLarge {
                actual,
                max: self.max_payload_bytes,
            });
        }
        Ok(())
    }

    pub fn check_snapshot(&self, actual: usize) -> Result<(), LimitError> {
        if actual > self.max_snapshot_bytes {
            return Err(LimitError::SnapshotTooLarge {
                actual,
                max: self.max_snapshot_bytes,
            });
        }
        Ok(())
    }

    pub fn check_replay_batch(&self, requested: usize) -> Result<(), LimitError> {
        if requested == 0 || requested > self.max_replay_batch {
            return Err(LimitError::ReplayBatchTooLarge {
                requested,
                max: self.max_replay_batch,
            });
        }
        Ok(())
    }
}
