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
