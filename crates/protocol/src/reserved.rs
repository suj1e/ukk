//! Ephemeral runtime fields that must never cross a durable protocol boundary.
//!
//! `hc_seq` is a herdr-core stream position that resets when the core restarts.
//! `host_epoch` identifies one Host process lifetime. Both are useful for local
//! in-memory continuity checks and both are invalid as cross-device durable
//! cursors: after a restart a client would either replay stale history or skip
//! events. Durable positions are [`crate::cursor::DurableCursor`] values issued
//! by the event log only.
//!
//! Durable wire messages reject these names at the top level instead of
//! silently ignoring them, so a client cannot believe its cursor was accepted.

use serde_json::{Map, Value};

/// Field names reserved for local, non-durable runtime state.
pub const RESERVED_EPHEMERAL_FIELDS: &[&str] = &["hc_seq", "host_epoch"];

pub(crate) fn scan_reserved(object: &Map<String, Value>) -> Option<&'static str> {
    RESERVED_EPHEMERAL_FIELDS
        .iter()
        .copied()
        .find(|field| object.contains_key(*field))
}

pub(crate) fn scan_value(value: &Value) -> Option<&'static str> {
    value.as_object().and_then(scan_reserved)
}
