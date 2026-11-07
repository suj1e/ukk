//! Cross-process Workspace change observation backed by SQLite revision
//! triggers. Each Host long-poll request has a bounded wait; no Agent process
//! state, terminal output or user secrets flow through these methods.
use serde_json::{json, Value};
use std::thread;
use std::time::{Duration, Instant};
use ukk_store::{Result as StoreResult, Store};

fn versioned(store: &Store) -> StoreResult<Value> {
    let (db_epoch, revision, snapshot) = store.snapshot_versioned()?;
    Ok(json!({
        "db_epoch": db_epoch,
        "revision": revision,
        "snapshot": snapshot,
    }))
}

pub(crate) fn snapshot() -> StoreResult<Value> {
    versioned(&Store::open_default()?)
}

/// Bounded (10 seconds max) long-poll that observes writes committed by
/// *any* SQLite connection. A database replacement changes db_epoch, so a
/// stale desktop always receives an authoritative new snapshot.
pub(crate) fn wait(
    after_epoch: &str,
    after_revision: i64,
    timeout_ms: u64,
) -> StoreResult<Value> {
    let store = Store::open_default()?;
    let deadline = Instant::now() + Duration::from_millis(timeout_ms.min(10_000));
    loop {
        let (db_epoch, revision) = store.workspace_cursor()?;
        if after_epoch != db_epoch || after_revision != revision {
            return versioned(&store);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(json!({
                "db_epoch": db_epoch,
                "revision": revision,
                "snapshot": null,
            }));
        }
        thread::sleep(remaining.min(Duration::from_millis(250)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mismatch_returns_snapshot_and_matching_cursor() {
        let mut store = Store::open_memory().unwrap();
        store.create_space("x", "/x").unwrap();
        let value = versioned(&store).unwrap();
        assert_eq!(value["revision"], 1);
        assert!(value["snapshot"]["spaces"].as_array().unwrap().len() == 1);
        assert_eq!(
            value["db_epoch"],
            store.workspace_cursor().unwrap().0
        );
    }
}
