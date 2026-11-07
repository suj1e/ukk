//! Durable S1 persistence coverage: reopen, cursor/retention, trust CAS and
//! idempotency atomicity.
use super::*;
use rusqlite::params;
use std::{
    env, fs,
    path::PathBuf,
    sync::{Arc, Barrier},
    thread,
};
use ukk_domain::new_id;
use ukk_protocol::idempotency::MAX_RESULT_DETAIL_BYTES;
use ukk_protocol::{
    Command, CommandResult, Denial, DeviceClaim, DeviceId, DeviceRevoke, DomainId, DurableCursor,
    Event, EventId, IdempotencyAdmission, IdempotencyError, IdempotencyKey, IdentityGeneration,
    KeyFingerprint, Limits, Plane, Principal, RequestFingerprint, Resume, Scope, SessionCreate,
    SessionCreated, StreamId, SyncError,
};

fn temp_db(label: &str) -> (PathBuf, PathBuf) {
    let dir = env::temp_dir().join(format!("ukk-s1-{label}-{}", new_id()));
    fs::create_dir_all(&dir).unwrap();
    (dir.clone(), dir.join("workspace.db"))
}

fn workspace_stream() -> StreamId {
    StreamId::new("workspace").unwrap()
}

fn session_created(title: &str) -> Event {
    Event::SessionCreated(SessionCreated {
        space_id: DomainId::new("space-1").unwrap(),
        session_id: DomainId::new("session-1").unwrap(),
        title: title.to_owned(),
        revision: 1,
    })
}

fn sample_command() -> Command {
    Command::SessionCreate(SessionCreate {
        space_id: DomainId::new("space-1").unwrap(),
        title: "demo".into(),
        expected_revision: None,
    })
}

fn sample_fingerprint() -> RequestFingerprint {
    RequestFingerprint::of(&sample_command()).unwrap()
}

#[test]
fn event_log_persists_cursors_and_payload_across_reopen() {
    let (dir, path) = temp_db("events-reopen");
    let stream = workspace_stream();
    let first_cursor;
    {
        let mut store = Store::open(&path).unwrap();
        let first = store
            .append_event(
                &stream,
                EventId::new("event-1").unwrap(),
                session_created("one"),
                1_000,
            )
            .unwrap();
        first_cursor = first.cursor;
        assert_eq!(first_cursor, DurableCursor::from_raw(1));
        store
            .append_event(
                &stream,
                EventId::new("event-2").unwrap(),
                session_created("two"),
                1_001,
            )
            .unwrap();
        store
            .append_event(
                &stream,
                EventId::new("event-3").unwrap(),
                session_created("three"),
                1_002,
            )
            .unwrap();
        let bounds = store.event_stream_bounds(&stream).unwrap().unwrap();
        assert_eq!(bounds.floor, DurableCursor::ZERO);
        assert_eq!(bounds.head, DurableCursor::from_raw(3));
        let snapshot = store
            .snapshot_at_head(&stream, 9, serde_json::json!({"sessions": []}))
            .unwrap();
        assert_eq!(snapshot.at_cursor, DurableCursor::from_raw(3));
        assert_eq!(snapshot.revision, 9);
    }
    {
        let store = Store::open(&path).unwrap();
        let bounds = store.event_stream_bounds(&stream).unwrap().unwrap();
        assert_eq!(bounds.floor, DurableCursor::ZERO);
        assert_eq!(bounds.head, DurableCursor::from_raw(3));
        let resume = store.read_events(&stream, DurableCursor::ZERO, 10).unwrap();
        let Resume::Events {
            events,
            head,
            has_more,
        } = resume
        else {
            panic!("expected retained events");
        };
        assert_eq!(head, DurableCursor::from_raw(3));
        assert!(!has_more);
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].cursor, first_cursor);
        assert_eq!(events[0].event, session_created("one"));
        assert_eq!(events[2].cursor, DurableCursor::from_raw(3));
        let resume = store
            .read_events(&stream, DurableCursor::from_raw(1), 10)
            .unwrap();
        let Resume::Events { events, .. } = resume else {
            panic!("expected retained events");
        };
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].cursor, DurableCursor::from_raw(2));
        let snapshot = store
            .snapshot_at_head(&stream, 10, serde_json::json!({}))
            .unwrap();
        assert_eq!(snapshot.at_cursor, DurableCursor::from_raw(3));
    }
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn event_log_refuses_cursor_regression_and_generation_mismatch() {
    let mut store = Store::open_memory().unwrap();
    let stream = workspace_stream();
    store
        .append_event(
            &stream,
            EventId::new("event-1").unwrap(),
            session_created("one"),
            1_000,
        )
        .unwrap();
    store
        .append_event(
            &stream,
            EventId::new("event-2").unwrap(),
            session_created("two"),
            1_001,
        )
        .unwrap();
    let bounds = store
        .prune_events_through(&stream, DurableCursor::from_raw(1))
        .unwrap();
    assert_eq!(bounds.floor, DurableCursor::from_raw(1));
    assert_eq!(bounds.head, DurableCursor::from_raw(2));
    assert!(matches!(
        store.prune_events_through(&stream, DurableCursor::ZERO),
        Err(StoreError::Protocol(SyncError::InvalidDurableLog(_)))
    ));
    let resume = store.read_events(&stream, DurableCursor::ZERO, 10).unwrap();
    let Resume::SnapshotRequired { at_cursor, bounds } = resume else {
        panic!("expected a snapshot boundary");
    };
    assert_eq!(at_cursor, DurableCursor::from_raw(2));
    assert_eq!(bounds.floor, DurableCursor::from_raw(1));
    assert!(matches!(
        store.read_events(&stream, DurableCursor::from_raw(3), 10),
        Err(StoreError::Protocol(SyncError::CursorBeyondHead { .. }))
    ));
    store
        .conn
        .execute(
            "UPDATE event_streams SET generation=2 WHERE stream_id=?1",
            params![stream.as_str()],
        )
        .unwrap();
    assert!(matches!(
        store.append_event(
            &stream,
            EventId::new("event-3").unwrap(),
            session_created("three"),
            1_002
        ),
        Err(StoreError::Protocol(SyncError::InvalidDurableLog(_)))
    ));
    assert!(matches!(
        store.read_events(&stream, DurableCursor::ZERO, 10),
        Err(StoreError::Protocol(SyncError::InvalidDurableLog(_)))
    ));
}

#[test]
fn event_log_rejects_duplicate_event_id_without_moving_head() {
    let mut store = Store::open_memory().unwrap();
    let stream = workspace_stream();
    let event_id = EventId::new("event-1").unwrap();
    store
        .append_event(&stream, event_id.clone(), session_created("one"), 1_000)
        .unwrap();
    assert!(matches!(
        store.append_event(&stream, event_id, session_created("different"), 1_001),
        Err(StoreError::DuplicateEvent(_))
    ));
    let bounds = store.event_stream_bounds(&stream).unwrap().unwrap();
    assert_eq!(bounds.head, DurableCursor::from_raw(1));
    let resume = store.read_events(&stream, DurableCursor::ZERO, 10).unwrap();
    let Resume::Events { events, .. } = resume else {
        panic!("expected retained events");
    };
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event, session_created("one"));
    store
        .append_event(
            &stream,
            EventId::new("event-2").unwrap(),
            session_created("two"),
            1_002,
        )
        .unwrap();
    assert_eq!(
        store.event_stream_bounds(&stream).unwrap().unwrap().head,
        DurableCursor::from_raw(2)
    );
}

#[test]
fn event_log_retention_floor_and_snapshot_boundary_survive_reopen() {
    let (dir, path) = temp_db("events-retention");
    let stream = workspace_stream();
    let limits = Limits {
        max_retained_events: 2,
        max_replay_batch: 2,
        ..Limits::default()
    };
    {
        let mut store = Store::open_with_limits(&path, limits).unwrap();
        for i in 1_i64..=3 {
            store
                .append_event(
                    &stream,
                    EventId::new(format!("event-{i}")).unwrap(),
                    session_created(&format!("event {i}")),
                    1_000 + i,
                )
                .unwrap();
        }
        let bounds = store.event_stream_bounds(&stream).unwrap().unwrap();
        assert_eq!(bounds.floor, DurableCursor::from_raw(1));
        assert_eq!(bounds.head, DurableCursor::from_raw(3));
        let Resume::SnapshotRequired { at_cursor, .. } =
            store.read_events(&stream, DurableCursor::ZERO, 2).unwrap()
        else {
            panic!("expected a snapshot boundary");
        };
        assert_eq!(at_cursor, DurableCursor::from_raw(3));
        let Resume::Events {
            events, has_more, ..
        } = store
            .read_events(&stream, DurableCursor::from_raw(1), 2)
            .unwrap()
        else {
            panic!("expected retained events");
        };
        assert_eq!(
            events
                .iter()
                .map(|event| event.cursor.get())
                .collect::<Vec<_>>(),
            vec![2, 3]
        );
        assert!(!has_more);
        let snapshot = store
            .snapshot_at_head(&stream, 3, serde_json::json!({"events": 3}))
            .unwrap();
        assert_eq!(snapshot.at_cursor, DurableCursor::from_raw(3));
        store
            .prune_events_through(&stream, DurableCursor::from_raw(2))
            .unwrap();
        assert!(matches!(
            store
                .read_events(&stream, DurableCursor::from_raw(1), 2)
                .unwrap(),
            Resume::SnapshotRequired { at_cursor, .. }
                if at_cursor == DurableCursor::from_raw(3)
        ));
        let Resume::Events { events, .. } = store
            .read_events(&stream, DurableCursor::from_raw(2), 2)
            .unwrap()
        else {
            panic!("expected retained events");
        };
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].cursor, DurableCursor::from_raw(3));
    }
    {
        let store = Store::open_with_limits(&path, limits).unwrap();
        let bounds = store.event_stream_bounds(&stream).unwrap().unwrap();
        assert_eq!(bounds.floor, DurableCursor::from_raw(2));
        assert_eq!(bounds.head, DurableCursor::from_raw(3));
        let Resume::Events { events, .. } = store
            .read_events(&stream, DurableCursor::from_raw(2), 2)
            .unwrap()
        else {
            panic!("expected retained events");
        };
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event, session_created("event 3"));
    }
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn device_trust_grants_survive_reopen_and_include_expiry() {
    let (dir, path) = temp_db("trust-reopen");
    let device = DeviceId::new("device-1").unwrap();
    let fingerprint = KeyFingerprint::new("sha256:device-one").unwrap();
    {
        let mut store = Store::open(&path).unwrap();
        let generation = store.register_device(&device, &fingerprint, 1_000).unwrap();
        assert_eq!(generation, IdentityGeneration::FIRST);
        store
            .grant_device_scope(&device, Scope::SessionObserve, generation, 10_000, 1_000)
            .unwrap();
        store
            .grant_device_scope(&device, Scope::AgentSend, generation, 10_000, 1_000)
            .unwrap();
        let trust = store.load_trust_registry().unwrap();
        let claim = DeviceClaim {
            device_id: device.clone(),
            key_fingerprint: fingerprint.clone(),
            generation,
        };
        let authenticated = trust.authenticate(&claim, 2_000).unwrap();
        let (valid, expired) = trust.live_grants(&authenticated, 2_000).unwrap();
        assert!(valid.contains(Scope::SessionObserve));
        assert!(valid.contains(Scope::AgentSend));
        assert!(expired.is_empty());
    }
    {
        let mut store = Store::open(&path).unwrap();
        store
            .grant_device_scope(
                &device,
                Scope::ScreenView,
                IdentityGeneration::FIRST,
                1_500,
                1_000,
            )
            .unwrap();
        let trust = store.load_trust_registry().unwrap();
        let claim = DeviceClaim {
            device_id: device.clone(),
            key_fingerprint: fingerprint.clone(),
            generation: IdentityGeneration::FIRST,
        };
        let authenticated = trust.authenticate(&claim, 2_000).unwrap();
        let (valid, expired) = trust.live_grants(&authenticated, 2_000).unwrap();
        assert!(valid.contains(Scope::SessionObserve));
        assert!(valid.contains(Scope::AgentSend));
        assert!(!valid.contains(Scope::ScreenView));
        assert!(expired.contains(Scope::ScreenView));
    }
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn device_rotation_and_revocation_invalidate_old_identity_with_cas() {
    let mut store = Store::open_memory().unwrap();
    let device = DeviceId::new("device-1").unwrap();
    let first = KeyFingerprint::new("sha256:first").unwrap();
    let second = KeyFingerprint::new("sha256:second").unwrap();
    let generation = store.register_device(&device, &first, 1_000).unwrap();
    store
        .grant_device_scope(&device, Scope::SessionObserve, generation, 10_000, 1_000)
        .unwrap();
    let old_claim = DeviceClaim {
        device_id: device.clone(),
        key_fingerprint: first,
        generation,
    };
    let old_trust = store.load_trust_registry().unwrap();
    let old_auth = old_trust.authenticate(&old_claim, 2_000).unwrap();
    assert!(old_trust
        .live_grants(&old_auth, 2_000)
        .unwrap()
        .0
        .contains(Scope::SessionObserve));

    assert!(matches!(
        store.rotate_device_key(&device, IdentityGeneration::from_raw(9), &second, 2_000),
        Err(StoreError::CasConflict)
    ));
    let rotated = store
        .rotate_device_key(&device, generation, &second, 2_000)
        .unwrap();
    assert_eq!(rotated, IdentityGeneration::from_raw(2));
    assert!(store.device_grants(&device).unwrap().is_empty());

    let rotated_trust = store.load_trust_registry().unwrap();
    assert!(matches!(
        rotated_trust.authenticate(&old_claim, 2_000),
        Err(Denial::StaleIdentity)
    ));
    assert!(matches!(
        rotated_trust.live_grants(&old_auth, 2_000),
        Err(Denial::StaleIdentity)
    ));
    let new_claim = DeviceClaim {
        device_id: device.clone(),
        key_fingerprint: second,
        generation: rotated,
    };
    let new_auth = rotated_trust.authenticate(&new_claim, 2_000).unwrap();
    let (valid, expired) = rotated_trust.live_grants(&new_auth, 2_000).unwrap();
    assert!(valid.is_empty());
    assert!(expired.is_empty());

    assert!(matches!(
        store.revoke_device(&device, generation, 3_000),
        Err(StoreError::CasConflict)
    ));
    store.revoke_device(&device, rotated, 3_000).unwrap();
    let revoked_trust = store.load_trust_registry().unwrap();
    assert!(matches!(
        revoked_trust.authenticate(&new_claim, 4_000),
        Err(Denial::DeviceRevoked)
    ));
    assert!(matches!(
        revoked_trust.live_grants(&new_auth, 4_000),
        Err(Denial::DeviceRevoked)
    ));
    assert!(store.device_grants(&device).unwrap().is_empty());
    // Revocation at the same generation is idempotent.
    store.revoke_device(&device, rotated, 3_100).unwrap();
}

#[test]
fn idempotency_concurrent_begin_admits_exactly_one_execution() {
    let (dir, path) = temp_db("idempotency-concurrent");
    let key = IdempotencyKey::new("key-1").unwrap();
    let fingerprint = sample_fingerprint();
    let principal = Principal::Device(DeviceId::new("device-1").unwrap());
    let barrier = Arc::new(Barrier::new(2));
    let mut handles = Vec::new();
    for _ in 0..2 {
        let path = path.clone();
        let key = key.clone();
        let fingerprint = fingerprint.clone();
        let principal = principal.clone();
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            let mut store = Store::open(&path).unwrap();
            barrier.wait();
            store.begin_idempotent_command(Plane::Remote, &principal, &key, &fingerprint, 1_000)
        }));
    }
    let results: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect();
    let mut executes = 0;
    let mut duplicates = 0;
    for result in results {
        match result.unwrap() {
            IdempotencyAdmission::Execute => executes += 1,
            IdempotencyAdmission::Duplicate { result: None } => duplicates += 1,
            other => panic!("unexpected admission: {other:?}"),
        }
    }
    assert_eq!(executes, 1);
    assert_eq!(duplicates, 1);
    let mut store = Store::open(&path).unwrap();
    assert_eq!(store.idempotency_record_count().unwrap(), 1);
    store
        .finish_idempotent_command(
            &key,
            CommandResult::Completed {
                detail: Some("done".into()),
            },
            1_001,
        )
        .unwrap();
    let admission = store
        .begin_idempotent_command(Plane::Remote, &principal, &key, &fingerprint, 1_002)
        .unwrap();
    assert!(matches!(
        admission,
        IdempotencyAdmission::Duplicate {
            result: Some(CommandResult::Completed { .. })
        }
    ));
    drop(store);
    let mut reopened = Store::open(&path).unwrap();
    let admission = reopened
        .begin_idempotent_command(Plane::Remote, &principal, &key, &fingerprint, 1_003)
        .unwrap();
    assert!(matches!(
        admission,
        IdempotencyAdmission::Duplicate {
            result: Some(CommandResult::Completed { .. })
        }
    ));
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn idempotency_tombstones_expire_but_in_flight_and_capacity_do_not() {
    let limits = Limits {
        max_idempotency_records: 2,
        idempotency_retention_ms: 1_000,
        ..Limits::default()
    };
    let mut store = Store::open_memory_with_limits(limits).unwrap();
    let fingerprint = sample_fingerprint();
    let principal = Principal::LocalOwner { uid: 501 };
    let key1 = IdempotencyKey::new("key-1").unwrap();
    let key2 = IdempotencyKey::new("key-2").unwrap();
    let key3 = IdempotencyKey::new("key-3").unwrap();
    assert!(matches!(
        store
            .begin_idempotent_command(Plane::LocalIpc, &principal, &key1, &fingerprint, 0)
            .unwrap(),
        IdempotencyAdmission::Execute
    ));
    store
        .finish_idempotent_command(&key1, CommandResult::Completed { detail: None }, 0)
        .unwrap();
    let admission = store
        .begin_idempotent_command(Plane::LocalIpc, &principal, &key1, &fingerprint, 999)
        .unwrap();
    assert!(matches!(
        admission,
        IdempotencyAdmission::Duplicate {
            result: Some(CommandResult::Completed { .. })
        }
    ));
    assert_eq!(store.purge_expired_idempotency(1_000).unwrap(), 1);
    assert!(matches!(
        store.begin_idempotent_command(Plane::LocalIpc, &principal, &key1, &fingerprint, 1_000),
        Err(StoreError::Idempotency(IdempotencyError::Expired))
    ));
    assert!(matches!(
        store
            .begin_idempotent_command(Plane::LocalIpc, &principal, &key2, &fingerprint, 1_000)
            .unwrap(),
        IdempotencyAdmission::Execute
    ));
    assert!(matches!(
        store.begin_idempotent_command(Plane::LocalIpc, &principal, &key3, &fingerprint, 1_000),
        Err(StoreError::Idempotency(IdempotencyError::Capacity {
            max: 2
        }))
    ));
    assert_eq!(store.purge_expired_idempotency(1_000_000).unwrap(), 0);
    let admission = store
        .begin_idempotent_command(Plane::LocalIpc, &principal, &key2, &fingerprint, 1_000_000)
        .unwrap();
    assert!(matches!(
        admission,
        IdempotencyAdmission::Duplicate { result: None }
    ));
    store
        .finish_idempotent_command(&key2, CommandResult::Completed { detail: None }, 1_000_000)
        .unwrap();
    assert_eq!(store.purge_expired_idempotency(1_001_000).unwrap(), 1);
    assert!(matches!(
        store.begin_idempotent_command(Plane::LocalIpc, &principal, &key3, &fingerprint, 1_001_000),
        Err(StoreError::Idempotency(IdempotencyError::Capacity {
            max: 2
        }))
    ));
    assert_eq!(store.idempotency_record_count().unwrap(), 2);
    assert!(matches!(
        store.finish_idempotent_command(
            &key1,
            CommandResult::Completed { detail: None },
            1_001_000
        ),
        Err(StoreError::Idempotency(IdempotencyError::AlreadyCompleted))
    ));
}

#[test]
fn idempotency_keys_are_bound_to_plane_principal_and_fingerprint() {
    let mut store = Store::open_memory().unwrap();
    let key = IdempotencyKey::new("key-1").unwrap();
    let fingerprint = sample_fingerprint();
    let device = DeviceId::new("device-1").unwrap();
    let other = DeviceId::new("device-2").unwrap();
    assert!(matches!(
        store
            .begin_idempotent_command(
                Plane::Remote,
                &Principal::Device(device.clone()),
                &key,
                &fingerprint,
                1_000
            )
            .unwrap(),
        IdempotencyAdmission::Execute
    ));
    assert!(matches!(
        store.begin_idempotent_command(
            Plane::LocalIpc,
            &Principal::LocalOwner { uid: 501 },
            &key,
            &fingerprint,
            1_001
        ),
        Err(StoreError::Idempotency(IdempotencyError::CrossPlane))
    ));
    assert!(matches!(
        store.begin_idempotent_command(
            Plane::Remote,
            &Principal::Device(other),
            &key,
            &fingerprint,
            1_002
        ),
        Err(StoreError::Idempotency(IdempotencyError::CrossPrincipal))
    ));
    let other_fingerprint = RequestFingerprint::of(&Command::DeviceRevoke(DeviceRevoke {
        device_id: device.clone(),
    }))
    .unwrap();
    assert!(matches!(
        store.begin_idempotent_command(
            Plane::Remote,
            &Principal::Device(device),
            &key,
            &other_fingerprint,
            1_003
        ),
        Err(StoreError::Idempotency(
            IdempotencyError::FingerprintMismatch
        ))
    ));
}

#[test]
fn idempotency_finish_requires_in_flight_and_bounded_detail() {
    let mut store = Store::open_memory().unwrap();
    let key = IdempotencyKey::new("key-1").unwrap();
    let fingerprint = sample_fingerprint();
    let principal = Principal::LocalOwner { uid: 501 };
    assert!(matches!(
        store.finish_idempotent_command(&key, CommandResult::Completed { detail: None }, 0),
        Err(StoreError::Idempotency(IdempotencyError::UnknownKey))
    ));
    store
        .begin_idempotent_command(Plane::LocalIpc, &principal, &key, &fingerprint, 0)
        .unwrap();
    let too_long = "x".repeat(MAX_RESULT_DETAIL_BYTES + 1);
    assert!(matches!(
        store.finish_idempotent_command(
            &key,
            CommandResult::Completed {
                detail: Some(too_long)
            },
            1
        ),
        Err(StoreError::Idempotency(
            IdempotencyError::InvalidResultDetail
        ))
    ));
    store
        .finish_idempotent_command(
            &key,
            CommandResult::Completed {
                detail: Some("ok".into()),
            },
            2,
        )
        .unwrap();
    assert!(matches!(
        store.finish_idempotent_command(&key, CommandResult::Completed { detail: None }, 3),
        Err(StoreError::Idempotency(IdempotencyError::AlreadyCompleted))
    ));
}

#[test]
fn schema_v2_upgrades_to_v3_without_losing_workspace_state() {
    let (dir, path) = temp_db("migrate-v2");
    let session_id;
    let revision;
    {
        let mut store = Store::open(&path).unwrap();
        let space = store.create_space("legacy", "/legacy").unwrap();
        let session = store.create_session(&space.id, "Session").unwrap();
        session_id = session.id;
        revision = store.workspace_revision().unwrap();
        store
            .conn
            .execute_batch(
                "DROP TABLE idempotency_records;
                 DROP TABLE device_grants;
                 DROP TABLE devices;
                 DROP TABLE event_log;
                 DROP TABLE event_streams;
                 PRAGMA user_version=2;",
            )
            .unwrap();
    }
    {
        let mut store = Store::open(&path).unwrap();
        let version: i64 = store
            .conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, 3);
        assert_eq!(store.workspace_revision().unwrap(), revision);
        assert_eq!(store.snapshot().unwrap().sessions[0].id, session_id);
        let stream = workspace_stream();
        store
            .append_event(
                &stream,
                EventId::new("event-1").unwrap(),
                session_created("one"),
                1_000,
            )
            .unwrap();
        let device = DeviceId::new("device-1").unwrap();
        let fingerprint = KeyFingerprint::new("sha256:device-one").unwrap();
        store.register_device(&device, &fingerprint, 1_000).unwrap();
        let key = IdempotencyKey::new("key-1").unwrap();
        let request = sample_fingerprint();
        assert!(matches!(
            store
                .begin_idempotent_command(
                    Plane::Remote,
                    &Principal::Device(device),
                    &key,
                    &request,
                    1_000
                )
                .unwrap(),
            IdempotencyAdmission::Execute
        ));
    }
    fs::remove_dir_all(dir).unwrap();
}
