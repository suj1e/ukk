//! Opaque user-facing product identifiers; never reuse herdr-core PTY IDs.
use uuid::Uuid;

pub fn new_id() -> String {
    Uuid::new_v4().to_string()
}
