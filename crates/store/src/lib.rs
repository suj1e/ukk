//! ukk SQLite repository API. Runtime processes remain owned by herdr-core.
//! Re-export public storage API while keeping schema, records and layout SQL
//! separate from each other and the GPUI frontend.
//!
//! Schema v3 adds the durable S1 protocol state: per-stream event logs, device
//! trust/grants and idempotency records. Protocol types and decisions stay in
//! `ukk-protocol`; this crate only persists and reloads them.

use rusqlite::Connection;
use std::{error::Error, fmt, path::PathBuf};
use ukk_domain::LayoutError;
use ukk_protocol::{EnvelopeError, IdempotencyError, LimitError, RegistryError, SyncError};

pub mod backup;
mod events;
mod idempotency;
mod layouts;
mod records;
#[cfg(test)]
mod s1_tests;
mod schema;
mod snapshot;
#[cfg(test)]
mod tests;
mod trust;

pub use schema::default_database_path;

#[derive(Debug)]
pub enum StoreError {
    Sql(rusqlite::Error),
    Json(serde_json::Error),
    Io(std::io::Error),
    Layout(LayoutError),
    Limit(LimitError),
    Envelope(EnvelopeError),
    Protocol(SyncError),
    Trust(RegistryError),
    Idempotency(IdempotencyError),
    Invalid(String),
    NotFound(String),
    Conflict,
    AlreadyBound(String),
    CasConflict,
    DuplicateEvent(String),
    UnsupportedVersion(i64),
    DestinationExists(PathBuf),
    UnsafePath(String),
    NotADatabase(String),
    IntegrityCheckFailed(String),
    Busy(String),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sql(e) => write!(f, "SQLite: {e}"),
            Self::Json(e) => write!(f, "layout JSON: {e}"),
            Self::Io(e) => write!(f, "filesystem: {e}"),
            Self::Layout(e) => write!(f, "invalid pane layout: {e}"),
            Self::Limit(e) => write!(f, "protocol limit: {e}"),
            Self::Envelope(e) => write!(f, "protocol envelope: {e}"),
            Self::Protocol(e) => write!(f, "durable protocol state: {e}"),
            Self::Trust(e) => write!(f, "device trust: {e}"),
            Self::Idempotency(e) => write!(f, "idempotency: {e}"),
            Self::Invalid(s) => write!(f, "invalid input: {s}"),
            Self::NotFound(s) => write!(f, "not found: {s}"),
            Self::Conflict => write!(f, "layout revision conflict; reload before retrying"),
            Self::AlreadyBound(detail) => write!(f, "already bound: {detail}"),
            Self::CasConflict => {
                write!(f, "generation conflict; reload before retrying")
            }
            Self::DuplicateEvent(id) => write!(f, "durable event already exists: {id}"),
            Self::UnsupportedVersion(v) => {
                write!(f, "database schema version {v} is newer than supported")
            }
            Self::DestinationExists(path) => {
                write!(
                    f,
                    "destination already exists: {} (overwrite was not requested)",
                    path.display()
                )
            }
            Self::UnsafePath(detail) => write!(f, "unsafe path: {detail}"),
            Self::NotADatabase(detail) => write!(f, "not a ukk workspace database: {detail}"),
            Self::IntegrityCheckFailed(detail) => {
                write!(f, "SQLite integrity check failed: {detail}")
            }
            Self::Busy(detail) => write!(f, "database is busy: {detail}"),
        }
    }
}

impl Error for StoreError {}

impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Sql(e)
    }
}
impl From<serde_json::Error> for StoreError {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e)
    }
}
impl From<std::io::Error> for StoreError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}
impl From<LayoutError> for StoreError {
    fn from(e: LayoutError) -> Self {
        Self::Layout(e)
    }
}
impl From<LimitError> for StoreError {
    fn from(e: LimitError) -> Self {
        Self::Limit(e)
    }
}
impl From<EnvelopeError> for StoreError {
    fn from(e: EnvelopeError) -> Self {
        Self::Envelope(e)
    }
}
impl From<SyncError> for StoreError {
    fn from(e: SyncError) -> Self {
        Self::Protocol(e)
    }
}
impl From<RegistryError> for StoreError {
    fn from(e: RegistryError) -> Self {
        Self::Trust(e)
    }
}
impl From<IdempotencyError> for StoreError {
    fn from(e: IdempotencyError) -> Self {
        Self::Idempotency(e)
    }
}

pub type Result<T, E = StoreError> = std::result::Result<T, E>;

/// Number of attempts for transient SQLite open/lock contention. The normal
/// path succeeds on the first attempt; retries only cover Windows sharing
/// violations and `SQLITE_BUSY`/`SQLITE_LOCKED` beyond the busy timeout.
pub(crate) const SQLITE_RETRY_ATTEMPTS: usize = 5;

pub(crate) fn is_retryable_sqlite(error: &rusqlite::Error) -> bool {
    match error {
        rusqlite::Error::SqliteFailure(code, message) => {
            matches!(
                code.code,
                rusqlite::ErrorCode::CannotOpen
                    | rusqlite::ErrorCode::DatabaseBusy
                    | rusqlite::ErrorCode::DatabaseLocked
            ) || message.as_deref().is_some_and(|message| {
                message.contains("used by another process")
                    || message.contains("cannot access the file")
            })
        }
        _ => false,
    }
}

pub(crate) fn retry_delay(attempt: usize) {
    std::thread::sleep(std::time::Duration::from_millis(10 * attempt as u64));
}

/// Runs one `IMMEDIATE` transaction, retrying only transient lock contention.
/// The operation commits on `Ok` and rolls back on `Err`; conflict semantics
/// are unchanged.
pub(crate) fn with_immediate<T>(
    conn: &mut Connection,
    operation: impl FnOnce(&rusqlite::Transaction<'_>) -> Result<T>,
) -> Result<T> {
    let mut attempt = 0;
    loop {
        match conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate) {
            Ok(tx) => {
                let value = operation(&tx)?;
                tx.commit()?;
                return Ok(value);
            }
            Err(error) if attempt + 1 < SQLITE_RETRY_ATTEMPTS && is_retryable_sqlite(&error) => {
                attempt += 1;
                retry_delay(attempt);
            }
            Err(error) => return Err(error.into()),
        }
    }
}

pub struct Store {
    conn: Connection,
    limits: ukk_protocol::Limits,
}

/// Wall-clock milliseconds for persisted timestamps and grant/idempotency
/// expiry. This is metadata time, not a durable cursor.
pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}
