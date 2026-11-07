mod support;

use support::{agent_send, envelope, limits, register_device, remote_session, service, NOW};
use ukk_protocol::{
    AdmissionDecision, CommandError, CommandResult, IdempotencyAdmission, IdempotencyError,
    IdempotencyKey, IdempotencyStore, Plane, Principal, QueueError, ReplayQueue,
    RequestFingerprint,
};

fn fingerprint(text: &str) -> RequestFingerprint {
    RequestFingerprint::of(&agent_send(text)).expect("fingerprint")
}

#[test]
fn in_flight_records_never_expire() {
    let limits = limits();
    let retention = limits.idempotency_retention_ms;
    let mut store = IdempotencyStore::new(limits).expect("store");
    let principal = Principal::LocalOwner { uid: 501 };
    let key = IdempotencyKey::new_random();
    let fingerprint = fingerprint("slow command");

    assert_eq!(
        store
            .begin(Plane::LocalIpc, &principal, &key, &fingerprint, NOW)
            .expect("admitted"),
        IdempotencyAdmission::Execute
    );
    assert_eq!(
        store.purge_expired(NOW + retention + 1),
        0,
        "an in-flight record must not be purged"
    );
    assert_eq!(
        store
            .begin(
                Plane::LocalIpc,
                &principal,
                &key,
                &fingerprint,
                NOW + retention + 1
            )
            .expect("still tracked"),
        IdempotencyAdmission::Duplicate { result: None },
        "nothing was executed twice"
    );
    assert_eq!(store.len(), 1);
}

#[test]
fn completed_record_expiry_refuses_retry_instead_of_executing() {
    let limits = limits();
    let retention = limits.idempotency_retention_ms;
    let mut store = IdempotencyStore::new(limits).expect("store");
    let principal = Principal::LocalOwner { uid: 501 };
    let key = IdempotencyKey::new_random();
    let fingerprint = fingerprint("completed command");
    let completed_at = NOW + 10;

    store
        .begin(Plane::LocalIpc, &principal, &key, &fingerprint, NOW)
        .expect("admitted");
    store
        .complete(
            &key,
            CommandResult::Completed {
                detail: Some("ok".to_owned()),
            },
            completed_at,
        )
        .expect("completed");

    assert_eq!(
        store
            .begin(
                Plane::LocalIpc,
                &principal,
                &key,
                &fingerprint,
                completed_at + retention - 1
            )
            .expect("inside retention"),
        IdempotencyAdmission::Duplicate {
            result: Some(CommandResult::Completed {
                detail: Some("ok".to_owned())
            })
        }
    );
    assert_eq!(
        store.purge_expired(completed_at + retention),
        1,
        "retention is measured from completion"
    );
    assert_eq!(
        store.begin(
            Plane::LocalIpc,
            &principal,
            &key,
            &fingerprint,
            completed_at + retention
        ),
        Err(IdempotencyError::Expired),
        "an expired retry must never be treated as a first attempt"
    );
    assert_eq!(
        store.len(),
        1,
        "the tombstone stays so the key cannot be reused for execution"
    );
}

#[test]
fn capacity_refuses_new_keys_and_keeps_tombstones() {
    let mut limits = limits();
    limits.max_idempotency_records = 1;
    let mut store = IdempotencyStore::new(limits).expect("store");
    let principal = Principal::LocalOwner { uid: 501 };
    let first = IdempotencyKey::new_random();
    let second = IdempotencyKey::new_random();
    let first_fingerprint = fingerprint("first");

    store
        .begin(Plane::LocalIpc, &principal, &first, &first_fingerprint, NOW)
        .expect("first admitted");
    assert_eq!(
        store.begin(
            Plane::LocalIpc,
            &principal,
            &second,
            &fingerprint("second"),
            NOW
        ),
        Err(IdempotencyError::Capacity { max: 1 })
    );
    assert_eq!(
        store
            .begin(Plane::LocalIpc, &principal, &first, &first_fingerprint, NOW)
            .expect("retry remains known"),
        IdempotencyAdmission::Duplicate { result: None }
    );

    store
        .complete(&first, CommandResult::Completed { detail: None }, NOW)
        .expect("completed");
    store.purge_expired(NOW + limits.idempotency_retention_ms);
    assert_eq!(
        store.begin(
            Plane::LocalIpc,
            &principal,
            &second,
            &fingerprint("second"),
            NOW + limits.idempotency_retention_ms
        ),
        Err(IdempotencyError::Capacity { max: 1 }),
        "a tombstone still consumes capacity instead of being evicted"
    );
}

#[test]
fn service_refuses_retry_after_retention_instead_of_reexecuting() {
    let mut service = service();
    let fixture = register_device(&mut service, &[ukk_protocol::Scope::AgentSend]);
    let session = remote_session(&service, &fixture);
    let command = envelope(agent_send("slow dispatch"));
    let retention = service.limits.idempotency_retention_ms;

    let AdmissionDecision::Execute { key, .. } = service
        .admit_command(&session, &command, NOW)
        .expect("admitted")
    else {
        panic!("first attempt executes");
    };
    assert_eq!(
        service.admit_command(&session, &command, NOW + retention + 1),
        Ok(AdmissionDecision::Duplicate {
            request_id: command.request_id.clone(),
            result: None
        }),
        "an in-flight duplicate stays in flight forever"
    );

    service
        .complete_command(
            &key,
            CommandResult::Completed {
                detail: Some("finished".to_owned()),
            },
            NOW + 1,
        )
        .expect("completed");
    assert_eq!(
        service.admit_command(&session, &command, NOW + retention + 2),
        Err(CommandError::Idempotency(IdempotencyError::Expired)),
        "retry after retention is refused rather than executed twice"
    );
}

#[test]
fn replay_queue_drops_stale_entries_and_rejects_future_timestamps() {
    let limits = limits();
    let retention = limits.idempotency_retention_ms;
    let mut queue = ReplayQueue::new(limits);
    let queued = envelope(agent_send("queued"));
    queue
        .enqueue(queued.clone(), NOW)
        .expect("fresh command queues");
    assert!(
        queue.drain(10, NOW + retention).is_empty(),
        "entries past the idempotency window are discarded, not replayed"
    );
    assert!(queue.is_empty());

    assert_eq!(
        queue.enqueue(queued.clone(), NOW + retention),
        Err(QueueError::Stale)
    );
    let mut future = envelope(agent_send("future"));
    future.issued_at_ms = NOW + retention * 2;
    assert_eq!(
        queue.enqueue(future, NOW),
        Err(QueueError::Stale),
        "far-future timestamps would survive every retention purge"
    );

    let mut small = limits;
    small.max_queue_depth = 1;
    let mut full = ReplayQueue::new(small);
    full.enqueue(envelope(agent_send("one")), NOW)
        .expect("first fits");
    assert_eq!(
        full.enqueue(envelope(agent_send("two")), NOW),
        Err(QueueError::Full { max: 1 })
    );
}
