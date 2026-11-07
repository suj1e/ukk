mod support;

use serde_json::json;
use support::{event_with_cursor, limits, log, log_with, status_event, stream, NOW};
use ukk_protocol::{
    ApplyOutcome, CursorCheckpoint, CursorError, DurableCursor, EventEnvelope, EventId, EventInbox,
    EventLog, InboxError, Resume, StreamId, SyncError,
};

fn append_n(log: &mut EventLog, count: u64) -> Vec<EventEnvelope> {
    (1..=count)
        .map(|revision| {
            log.append(status_event(revision), NOW)
                .expect("event appends")
        })
        .collect()
}

#[test]
fn two_clients_reconnect_independently() {
    let mut log = log();
    let events = append_n(&mut log, 3);
    let mut client_a = EventInbox::new(stream(), 16).expect("inbox");
    let mut client_b = EventInbox::new(stream(), 16).expect("inbox");

    for event in &events[..2] {
        assert_eq!(
            client_a.apply(event).expect("applies"),
            ApplyOutcome::Applied {
                cursor: event.cursor
            }
        );
    }
    assert_eq!(
        client_b.apply(&events[0]).expect("applies"),
        ApplyOutcome::Applied {
            cursor: events[0].cursor
        }
    );

    let checkpoint_a = CursorCheckpoint::new(stream(), client_a.applied());
    let checkpoint_b = CursorCheckpoint::new(stream(), client_b.applied());
    assert_eq!(checkpoint_a.cursor, DurableCursor::from_raw(2));
    assert_eq!(checkpoint_b.cursor, DurableCursor::from_raw(1));

    let replay_a = log.resume(checkpoint_a.cursor, 16).expect("resume");
    let replay_b = log.resume(checkpoint_b.cursor, 16).expect("resume");
    match replay_a {
        Resume::Events {
            events,
            head,
            has_more,
        } => {
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].cursor, DurableCursor::from_raw(3));
            assert_eq!(head, DurableCursor::from_raw(3));
            assert!(!has_more);
        }
        Resume::SnapshotRequired { .. } => panic!("history is retained"),
    }
    match replay_b {
        Resume::Events {
            events,
            head,
            has_more,
        } => {
            assert_eq!(
                events
                    .iter()
                    .map(|event| event.cursor.get())
                    .collect::<Vec<_>>(),
                vec![2, 3]
            );
            assert_eq!(head, DurableCursor::from_raw(3));
            assert!(!has_more);
        }
        Resume::SnapshotRequired { .. } => panic!("history is retained"),
    }
}

#[test]
fn dropped_ack_redelivers_and_client_deduplicates() {
    let mut log = log();
    let events = append_n(&mut log, 2);
    let mut inbox = EventInbox::new(stream(), 16).expect("inbox");
    for event in &events {
        inbox.apply(event).expect("applies");
    }
    let checkpoint = CursorCheckpoint::new(stream(), inbox.applied());
    assert_eq!(checkpoint.cursor, DurableCursor::from_raw(2));

    // The ACK never reached the Host, so replay starts from zero.
    let replay = log.resume(DurableCursor::ZERO, 16).expect("resume");
    let Resume::Events { events: replay, .. } = replay else {
        panic!("history is retained");
    };
    assert_eq!(replay.len(), 2);
    for event in &replay {
        assert_eq!(
            inbox.apply(event).expect("dedupes"),
            ApplyOutcome::Duplicate {
                cursor: event.cursor
            }
        );
    }
    assert_eq!(inbox.applied(), DurableCursor::from_raw(2));

    // A stale checkpoint must never move backwards.
    let mut persisted = checkpoint;
    assert_eq!(
        persisted.advance(DurableCursor::from_raw(2)),
        Err(CursorError::Regression {
            current: 2,
            next: 2
        })
    );
    assert_eq!(persisted.cursor, DurableCursor::from_raw(2));
}

#[test]
fn out_of_order_delivery_surfaces_a_gap() {
    let mut log = log();
    let events = append_n(&mut log, 2);
    let mut inbox = EventInbox::new(stream(), 16).expect("inbox");

    assert_eq!(
        inbox.apply(&events[1]).expect_err("gap"),
        InboxError::OutOfOrder {
            expected: DurableCursor::from_raw(1),
            received: DurableCursor::from_raw(2),
        }
    );
    assert_eq!(
        inbox.apply(&events[0]).expect("first applies"),
        ApplyOutcome::Applied {
            cursor: DurableCursor::from_raw(1)
        }
    );
    assert_eq!(
        inbox.apply(&events[1]).expect("second applies"),
        ApplyOutcome::Applied {
            cursor: DurableCursor::from_raw(2)
        }
    );
}

#[test]
fn event_id_reuse_at_a_different_cursor_is_detected() {
    let mut inbox = EventInbox::new(stream(), 16).expect("inbox");
    let id = EventId::new("event-fixed").expect("valid");
    let first = event_with_cursor(id.clone(), 1);
    inbox.apply(&first).expect("applies");

    let same = event_with_cursor(id.clone(), 1);
    assert_eq!(
        inbox.apply(&same).expect("duplicate"),
        ApplyOutcome::Duplicate {
            cursor: DurableCursor::from_raw(1)
        }
    );

    let forged = event_with_cursor(id, 2);
    assert_eq!(
        inbox.apply(&forged).expect_err("reused id"),
        InboxError::InconsistentReplay {
            cursor: DurableCursor::from_raw(2)
        }
    );
    assert_eq!(inbox.applied(), DurableCursor::from_raw(1));
}

#[test]
fn retention_gap_requires_snapshot_then_resumes() {
    let mut limits = limits();
    limits.max_retained_events = 2;
    limits.max_replay_batch = 2;
    let mut log = log_with(limits);
    append_n(&mut log, 4);
    assert_eq!(log.head(), DurableCursor::from_raw(4));
    assert_eq!(log.floor(), DurableCursor::from_raw(2));
    assert_eq!(log.len(), 2);

    assert_eq!(
        log.resume(DurableCursor::ZERO, 2).expect("resume"),
        Resume::SnapshotRequired {
            at_cursor: DurableCursor::from_raw(4),
            bounds: log.bounds(),
        }
    );
    assert!(matches!(
        log.resume(DurableCursor::from_raw(1), 2).expect("resume"),
        Resume::SnapshotRequired { .. }
    ));
    match log.resume(DurableCursor::from_raw(2), 2).expect("resume") {
        Resume::Events {
            events,
            head,
            has_more,
        } => {
            assert_eq!(
                events
                    .iter()
                    .map(|event| event.cursor.get())
                    .collect::<Vec<_>>(),
                vec![3, 4]
            );
            assert_eq!(head, DurableCursor::from_raw(4));
            assert!(!has_more);
        }
        Resume::SnapshotRequired { .. } => panic!("cursor at floor is resumable"),
    }

    let snapshot = log.snapshot(9, json!({"sessions": []}));
    assert_eq!(snapshot.at_cursor, DurableCursor::from_raw(4));
    let encoded = snapshot.encode(&limits).expect("snapshot encodes");
    let decoded = ukk_protocol::Snapshot::decode(&encoded, &limits).expect("snapshot decodes");
    assert_eq!(decoded, snapshot);
    assert_eq!(
        log.resume(decoded.at_cursor, 2).expect("resume"),
        Resume::Events {
            events: Vec::new(),
            head: DurableCursor::from_raw(4),
            has_more: false,
        }
    );

    let next = log.append(status_event(5), NOW).expect("append");
    assert_eq!(next.cursor, DurableCursor::from_raw(5));
    match log.resume(DurableCursor::from_raw(4), 2).expect("resume") {
        Resume::Events { events, .. } => {
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].cursor, next.cursor);
        }
        Resume::SnapshotRequired { .. } => panic!("new event is retained"),
    }
    assert_eq!(log.len(), 2, "the pruned tail stays pruned");
}

#[test]
fn cursor_is_monotonic_across_host_restart() {
    let limits = limits();
    let mut original = log_with(limits);
    let first_three = append_n(&mut original, 3);
    let persisted: Vec<String> = first_three
        .iter()
        .map(|event| event.encode(&limits).expect("event encodes"))
        .collect();
    let durable_events: Vec<EventEnvelope> = persisted
        .iter()
        .map(|raw| EventEnvelope::decode(raw, &limits).expect("event decodes"))
        .collect();
    let head = original.head();
    let floor = original.floor();

    let checkpoint = CursorCheckpoint::new(stream(), DurableCursor::from_raw(2));
    let persisted_checkpoint = checkpoint.encode().expect("checkpoint encodes");
    let mut restored_checkpoint =
        CursorCheckpoint::decode(&persisted_checkpoint).expect("checkpoint decodes");
    assert_eq!(restored_checkpoint.cursor, DurableCursor::from_raw(2));

    // Restart: the durable store provides the head, not an ephemeral counter.
    let mut restored =
        EventLog::restore(stream(), durable_events.clone(), head, floor, limits).expect("restore");
    assert_eq!(restored.head(), DurableCursor::from_raw(3));

    let after_restart = restored.append(status_event(4), NOW).expect("append");
    assert_eq!(after_restart.cursor, DurableCursor::from_raw(4));
    assert_eq!(restored.head(), DurableCursor::from_raw(4));

    match restored
        .resume(restored_checkpoint.cursor, 16)
        .expect("resume")
    {
        Resume::Events { events, .. } => {
            assert_eq!(
                events
                    .iter()
                    .map(|event| event.cursor.get())
                    .collect::<Vec<_>>(),
                vec![3, 4]
            );
        }
        Resume::SnapshotRequired { .. } => panic!("history is retained"),
    }

    restored_checkpoint
        .advance(DurableCursor::from_raw(3))
        .expect("forward");
    assert_eq!(
        restored_checkpoint.advance(DurableCursor::from_raw(2)),
        Err(CursorError::Regression {
            current: 3,
            next: 2
        })
    );

    // A second restart keeps counting from the durable head.
    let all = [first_three.clone(), vec![after_restart]].concat();
    let mut again =
        EventLog::restore(stream(), all, restored.head(), floor, limits).expect("second restore");
    let next = again.append(status_event(5), NOW).expect("append");
    assert_eq!(next.cursor, DurableCursor::from_raw(5));
}

#[test]
fn restore_rejects_non_contiguous_or_inconsistent_durable_state() {
    let limits = limits();
    let mut original = log_with(limits);
    let events = append_n(&mut original, 3);
    let head = original.head();

    let broken = vec![events[0].clone(), events[2].clone()];
    assert!(matches!(
        EventLog::restore(stream(), broken, head, DurableCursor::ZERO, limits),
        Err(SyncError::InvalidDurableLog(_))
    ));

    let empty = EventLog::restore(
        stream(),
        Vec::new(),
        DurableCursor::from_raw(3),
        DurableCursor::ZERO,
        limits,
    );
    assert!(matches!(empty, Err(SyncError::InvalidDurableLog(_))));

    assert!(matches!(
        EventLog::restore(
            stream(),
            events,
            DurableCursor::from_raw(2),
            DurableCursor::ZERO,
            limits
        ),
        Err(SyncError::InvalidDurableLog(_))
    ));
}

#[test]
fn cursor_beyond_head_is_a_typed_error_not_a_reset() {
    let mut log = log();
    append_n(&mut log, 2);
    assert_eq!(
        log.resume(DurableCursor::from_raw(5), 16),
        Err(SyncError::CursorBeyondHead { cursor: 5, head: 2 })
    );
}

#[test]
fn restore_preserves_pruned_floor() {
    let mut limits = limits();
    limits.max_retained_events = 2;
    limits.max_replay_batch = 2;
    let mut original = log_with(limits);
    append_n(&mut original, 4);
    let retained: Vec<EventEnvelope> = original
        .resume(DurableCursor::from_raw(2), 2)
        .expect("resume")
        .into_events();
    let restored = EventLog::restore(
        stream(),
        retained,
        DurableCursor::from_raw(4),
        DurableCursor::from_raw(2),
        limits,
    )
    .expect("restore");
    assert_eq!(
        restored.resume(DurableCursor::ZERO, 2).expect("resume"),
        Resume::SnapshotRequired {
            at_cursor: DurableCursor::from_raw(4),
            bounds: ukk_protocol::StreamBounds {
                stream: StreamId::new("workspace").expect("valid"),
                floor: DurableCursor::from_raw(2),
                head: DurableCursor::from_raw(4),
            },
        }
    );
}

trait IntoEvents {
    fn into_events(self) -> Vec<EventEnvelope>;
}

impl IntoEvents for Resume {
    fn into_events(self) -> Vec<EventEnvelope> {
        match self {
            Resume::Events { events, .. } => events,
            Resume::SnapshotRequired { .. } => panic!("history is retained"),
        }
    }
}
