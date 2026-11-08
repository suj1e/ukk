//! SQLite persistence and optimistic concurrency regression coverage.
use super::*;
use std::{env, fs};
use ukk_domain::{new_id, PaneContent, PaneNode, SplitAxis};

#[test]
fn session_and_layout_persist_across_reopen() {
    let dir = env::temp_dir().join(format!("ukk-store-{}", new_id()));
    let path = dir.join("workspace.db");
    let space_id;
    let session_id;
    let leaf_id;
    {
        let mut store = Store::open(&path).unwrap();
        let space = store.create_space("ukk", "/work/ukk").unwrap();
        space_id = space.id;
        let session = store.create_session(&space_id, "Implement sync").unwrap();
        session_id = session.id;
        let layout = store.get_layout(&session_id).unwrap();
        leaf_id = layout.root.id().to_owned();
        let split = layout
            .root
            .split_leaf(&leaf_id, SplitAxis::Horizontal, 0.55, PaneContent::Diff)
            .unwrap();
        store.set_layout(&session_id, 0, split).unwrap();
    }
    {
        let store = Store::open(&path).unwrap();
        let snap = store.snapshot().unwrap();
        assert_eq!(snap.spaces[0].id, space_id);
        assert_eq!(snap.sessions[0].id, session_id);
        assert_eq!(snap.layouts[0].revision, 1);
        assert_eq!(snap.layouts[0].root.leaves().len(), 2);
        assert_eq!(snap.layouts[0].root.leaves()[0].0, leaf_id);
    }
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn closing_pane_or_archiving_session_does_not_remove_agent() {
    let mut store = Store::open_memory().unwrap();
    let space = store.create_space("project", "/work").unwrap();
    let session = store.create_session(&space.id, "parallel agents").unwrap();
    let agent = store
        .bind_agent(&session.id, "hc-core-1", "t9", "codex", None)
        .unwrap();
    let layout = store.get_layout(&session.id).unwrap();
    let two = layout
        .root
        .split_leaf(
            layout.root.id(),
            SplitAxis::Vertical,
            0.5,
            PaneContent::Agent {
                agent_run_id: agent.id.clone(),
            },
        )
        .unwrap();
    let saved = store.set_layout(&session.id, 0, two).unwrap();
    let pane_to_close = saved.root.leaves()[1].0.to_owned();
    let after = saved.root.close_leaf(&pane_to_close).unwrap();
    store.set_layout(&session.id, 1, after).unwrap();
    store.archive_session(&session.id).unwrap();
    let snapshot = store.snapshot().unwrap();
    assert!(snapshot.sessions[0].archived);
    assert_eq!(snapshot.agent_runs[0].id, agent.id);
    assert_eq!(snapshot.agent_runs[0].terminal_address, "t9");
}

#[test]
fn idempotent_session_creation_preserves_layout_on_retry() {
    let mut store = Store::open_memory().unwrap();
    let space = store.create_space("ukk", "/work").unwrap();
    let request_id = new_id();
    let first = store
        .create_session_with_id(&space.id, "Draft", &request_id)
        .unwrap();
    let initial = store.get_layout(&first.id).unwrap();
    let split = initial
        .root
        .split_leaf(
            initial.root.id(),
            SplitAxis::Horizontal,
            0.5,
            PaneContent::Diff,
        )
        .unwrap();
    store.set_layout(&first.id, 0, split).unwrap();
    let repeat = store
        .create_session_with_id(&space.id, "Draft", &request_id)
        .unwrap();
    assert_eq!(first, repeat);
    assert_eq!(store.snapshot().unwrap().sessions.len(), 1);
    assert_eq!(store.get_layout(&first.id).unwrap().revision, 1);
    assert!(matches!(
        store.create_session_with_id(&space.id, "Different", &request_id),
        Err(StoreError::Conflict)
    ));
}

#[test]
fn optimistic_layout_revision_prevents_stale_writer() {
    let mut store = Store::open_memory().unwrap();
    let space = store.create_space("x", "/x").unwrap();
    let session = store.create_session(&space.id, "session").unwrap();
    let layout = store.get_layout(&session.id).unwrap();
    store
        .set_layout(&session.id, 0, layout.root.clone())
        .unwrap();
    assert!(matches!(
        store.set_layout(&session.id, 0, layout.root),
        Err(StoreError::Conflict)
    ));
}

#[test]
fn forbids_link_to_agent_in_different_session() {
    let mut store = Store::open_memory().unwrap();
    let space = store.create_space("x", "/x").unwrap();
    let one = store.create_session(&space.id, "one").unwrap();
    let two = store.create_session(&space.id, "two").unwrap();
    let agent = store
        .bind_agent(&one.id, "hc-core-a", "t1", "codex", None)
        .unwrap();
    let root = PaneNode::leaf(PaneContent::Agent {
        agent_run_id: agent.id,
    });
    assert!(matches!(
        store.set_layout(&two.id, 0, root),
        Err(StoreError::Invalid(_))
    ));
}

#[test]
fn binds_parent_only_within_same_session() {
    let mut store = Store::open_memory().unwrap();
    let space = store.create_space("x", "/x").unwrap();
    let one = store.create_session(&space.id, "one").unwrap();
    let two = store.create_session(&space.id, "two").unwrap();
    let parent = store
        .bind_agent(&one.id, "hc-core-a", "t1", "codex", None)
        .unwrap();
    assert!(matches!(
        store.bind_agent(&two.id, "hc-core-a", "t2", "claude", Some(&parent.id)),
        Err(StoreError::Invalid(_))
    ));
}

#[test]
fn workspace_revision_tracks_all_metadata_writes_without_duplicate_retries() {
    let mut store = Store::open_memory().unwrap();
    assert_eq!(store.workspace_revision().unwrap(), 0);
    let space = store.create_space("ukk", "/repo").unwrap();
    assert_eq!(store.workspace_revision().unwrap(), 1);
    let id = new_id();
    let session = store
        .create_session_with_id(&space.id, "Demo", &id)
        .unwrap();
    // One INSERT into sessions and another into session_layouts.
    assert_eq!(store.workspace_revision().unwrap(), 3);
    store
        .create_session_with_id(&space.id, "Demo", &id)
        .unwrap();
    assert_eq!(store.workspace_revision().unwrap(), 3);
    store
        .bind_agent(&session.id, "core-1", "t9", "codex", None)
        .unwrap();
    assert_eq!(store.workspace_revision().unwrap(), 4);
    let layout = store.get_layout(&session.id).unwrap();
    store
        .set_layout(&session.id, layout.revision, layout.root)
        .unwrap();
    assert_eq!(store.workspace_revision().unwrap(), 5);
    store.archive_session(&session.id).unwrap();
    assert_eq!(store.workspace_revision().unwrap(), 6);
}

#[test]
fn workspace_revision_observes_writes_from_another_connection() {
    let dir = env::temp_dir().join(format!("ukk-revision-{}", new_id()));
    let path = dir.join("workspace.db");
    let writer = Store::open(&path).unwrap();
    let mut reader = Store::open(&path).unwrap();
    assert_eq!(reader.workspace_revision().unwrap(), 0);
    drop(writer);
    let mut second = Store::open(&path).unwrap();
    second.create_space("shared", "/shared").unwrap();
    assert_eq!(reader.workspace_revision().unwrap(), 1);
    reader.create_space("readwrite", "/two").unwrap();
    assert_eq!(second.workspace_revision().unwrap(), 2);
    drop(reader);
    drop(second);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn schema_v1_upgrades_without_losing_sessions() {
    let dir = env::temp_dir().join(format!("ukk-migrate-{}", new_id()));
    let path = dir.join("workspace.db");
    let mut original = Store::open(&path).unwrap();
    let space = original.create_space("legacy", "/legacy").unwrap();
    let session = original.create_session(&space.id, "Session").unwrap();
    // Recreate the shape of a deployed v1 DB for the migration test.
    for table in ["spaces", "sessions", "agent_runs", "session_layouts"] {
        for suffix in ["insert", "update", "delete"] {
            original
                .conn
                .execute_batch(&format!("DROP TRIGGER ukk_rev_{table}_{suffix};"))
                .unwrap();
        }
    }
    original
        .conn
        .execute_batch(
            "DROP TABLE IF EXISTS idempotency_records;
             DROP TABLE IF EXISTS device_grants;
             DROP TABLE IF EXISTS devices;
             DROP TABLE IF EXISTS event_log;
             DROP TABLE IF EXISTS event_streams;
             DROP TABLE workspace_revision;
             PRAGMA user_version=1;",
        )
        .unwrap();
    drop(original);

    // Two readers opening a legacy DB simultaneously must share one
    // successful v2 migration rather than racing CREATE TABLE/TRIGGER.
    let parallel_path = path.clone();
    let second = std::thread::spawn(move || {
        let store = Store::open(&parallel_path).unwrap();
        store.workspace_cursor().unwrap()
    });
    let upgraded = Store::open(&path).unwrap();
    let cursor = upgraded.workspace_cursor().unwrap();
    assert_eq!(cursor.1, 0);
    assert_eq!(upgraded.snapshot().unwrap().sessions[0].id, session.id);
    assert_eq!(second.join().unwrap(), cursor);
    drop(upgraded);
    // Reopening should retain the same database incarnation.
    let reopened = Store::open(&path).unwrap();
    assert_eq!(reopened.workspace_cursor().unwrap(), cursor);
    drop(reopened);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn tab_order_survives_database_reopen_without_shared_active_tab() {
    let dir = env::temp_dir().join(format!("ukk-tabs-{}", new_id()));
    let path = dir.join("workspace.db");
    let session_id;
    let expected_order;
    {
        let mut store = Store::open(&path).unwrap();
        let space = store.create_space("tabs", "/repo").unwrap();
        let session = store.create_session(&space.id, "Session tabs").unwrap();
        session_id = session.id;
        let original = store.get_layout(&session_id).unwrap();
        let tabbed = original
            .root
            .add_tab(original.root.id(), "Files".into(), PaneContent::Files)
            .unwrap();
        let group_id = tabbed.id().to_owned();
        let file_tab_id = tabbed.leaves()[1].0.to_owned();
        let reordered = tabbed.reorder_tab(&group_id, &file_tab_id, -1).unwrap();
        expected_order = reordered
            .leaves()
            .iter()
            .map(|(id, _)| (*id).to_owned())
            .collect::<Vec<_>>();
        store.set_layout(&session_id, 0, reordered).unwrap();
    }
    {
        let store = Store::open(&path).unwrap();
        let saved = store.get_layout(&session_id).unwrap();
        assert_eq!(saved.revision, 1);
        let actual = saved
            .root
            .leaves()
            .iter()
            .map(|(id, _)| (*id).to_owned())
            .collect::<Vec<_>>();
        assert_eq!(actual, expected_order);
        let encoded = serde_json::to_value(saved.root).unwrap();
        assert!(
            encoded["active_tab"].is_null(),
            "active focus is client-local"
        );
    }
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn tab_agent_binding_checks_every_hidden_tab() {
    let mut store = Store::open_memory().unwrap();
    let space = store.create_space("ukk", "/repo").unwrap();
    let one = store.create_session(&space.id, "one").unwrap();
    let two = store.create_session(&space.id, "two").unwrap();
    let agent = store
        .bind_agent(&one.id, "core", "t1", "codex", None)
        .unwrap();
    let initial = store.get_layout(&two.id).unwrap();
    let illegal = initial
        .root
        .add_tab(
            initial.root.id(),
            "Other session agent".into(),
            PaneContent::Agent {
                agent_run_id: agent.id,
            },
        )
        .unwrap();
    assert!(matches!(
        store.set_layout(&two.id, initial.revision, illegal),
        Err(StoreError::Invalid(_))
    ));
}

#[test]
fn explicit_bind_is_idempotent_and_rejects_input_drift() {
    let mut store = Store::open_memory().unwrap();
    let space = store.create_space("ukk", "/work").unwrap();
    let session = store.create_session(&space.id, "bound").unwrap();
    let other = store.create_session(&space.id, "other").unwrap();
    let request_id = new_id();
    let first = store
        .bind_agent_with_id(&request_id, &session.id, "core-1", "t7", "codex", None)
        .unwrap();
    assert_eq!(first.id, request_id);
    let repeat = store
        .bind_agent_with_id(&request_id, &session.id, "core-1", "t7", "codex", None)
        .unwrap();
    assert_eq!(first, repeat);
    assert_eq!(store.snapshot().unwrap().agent_runs.len(), 1);

    for (session_id, core, terminal, adapter, parent) in [
        (other.id.as_str(), "core-1", "t7", "codex", None),
        (session.id.as_str(), "core-2", "t7", "codex", None),
        (session.id.as_str(), "core-1", "t8", "codex", None),
        (session.id.as_str(), "core-1", "t7", "claude", None),
    ] {
        assert!(matches!(
            store.bind_agent_with_id(&request_id, session_id, core, terminal, adapter, parent),
            Err(StoreError::Conflict)
        ));
    }
    assert_eq!(store.snapshot().unwrap().agent_runs.len(), 1);
    assert!(matches!(
        store.bind_agent_with_id("not-a-uuid", &session.id, "core-1", "t8", "codex", None),
        Err(StoreError::Invalid(_))
    ));
}

#[test]
fn explicit_bind_requires_known_session_parent_and_unbound_terminal() {
    let mut store = Store::open_memory().unwrap();
    let space = store.create_space("ukk", "/work").unwrap();
    let one = store.create_session(&space.id, "one").unwrap();
    let two = store.create_session(&space.id, "two").unwrap();
    assert!(matches!(
        store.bind_agent_with_id(&new_id(), &new_id(), "core", "t1", "codex", None),
        Err(StoreError::NotFound(_))
    ));
    assert!(matches!(
        store.bind_agent_with_id(&new_id(), &one.id, "core", "t1", "codex", Some("missing")),
        Err(StoreError::NotFound(_))
    ));
    let parent = store
        .bind_agent_with_id(&new_id(), &one.id, "core", "t1", "codex", None)
        .unwrap();
    assert!(matches!(
        store.bind_agent_with_id(&new_id(), &two.id, "core", "t2", "claude", Some(&parent.id)),
        Err(StoreError::Invalid(_))
    ));
    // The same terminal on the same core cannot silently gain a second run.
    assert!(matches!(
        store.bind_agent_with_id(&new_id(), &two.id, "core", "t1", "claude", None),
        Err(StoreError::AlreadyBound(_))
    ));
    // A different core with the same terminal address remains a distinct
    // runtime identity and is allowed.
    store
        .bind_agent_with_id(&new_id(), &two.id, "core-b", "t1", "claude", None)
        .unwrap();
}

#[test]
fn explicit_bind_persists_across_reopen() {
    let dir = env::temp_dir().join(format!("ukk-store-bind-{}", new_id()));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("workspace.db");
    let request_id = new_id();
    let (session_id, run_id) = {
        let mut store = Store::open(&path).unwrap();
        let space = store.create_space("ukk", "/work").unwrap();
        let session = store.create_session(&space.id, "persisted").unwrap();
        let run = store
            .bind_agent_with_id(&request_id, &session.id, "core-1", "t7", "codex", None)
            .unwrap();
        (session.id, run.id)
    };
    {
        let mut store = Store::open(&path).unwrap();
        let runs = store.snapshot().unwrap().agent_runs;
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].id, run_id);
        assert_eq!(runs[0].session_id, session_id);
        let repeat = store
            .bind_agent_with_id(&request_id, &session_id, "core-1", "t7", "codex", None)
            .unwrap();
        assert_eq!(repeat.id, run_id);
        assert_eq!(store.agent_run_by_id(&run_id).unwrap().unwrap().id, run_id);
    }
    fs::remove_dir_all(dir).unwrap();
}
