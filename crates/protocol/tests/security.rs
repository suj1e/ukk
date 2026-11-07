mod support;

use support::{
    agent_send, approval, envelope, limits, local_session, register_device,
    register_device_with_ttl, remote_session, service, session_id, NOW,
};
use ukk_protocol::{
    AdmissionDecision, ApprovalDecision, ClientHello, CommandError, CommandResult, ControllerLease,
    Denial, DeviceClaim, DeviceId, DomainId, IdempotencyError, IdentityGeneration, KeyFingerprint,
    LeaseError, Plane, ReplayPolicy, ReplayQueue, RequestId, Scope, SessionError, TransportBinding,
    CURRENT_GENERATION,
};

#[test]
fn revoked_device_loses_access_mid_session() {
    let mut service = service();
    let fixture = register_device(
        &mut service,
        &[
            Scope::AgentSend,
            Scope::SessionObserve,
            Scope::ScreenControl,
        ],
    );
    let session = remote_session(&service, &fixture);
    let lease = service
        .leases
        .issue(fixture.device_id.clone(), session_id(), NOW, 60_000)
        .expect("lease");

    let first = envelope(agent_send("first"));
    let AdmissionDecision::Execute { key, .. } = service
        .admit_command(&session, &first, NOW)
        .expect("admitted")
    else {
        panic!("first attempt executes");
    };
    service
        .complete_command(
            &key,
            CommandResult::Completed {
                detail: Some("dispatched".to_owned()),
            },
        )
        .expect("completed");

    let pointer = envelope(support::pointer(Some(lease.id.clone())));
    assert!(matches!(
        service.admit_command(&session, &pointer, NOW),
        Ok(AdmissionDecision::Execute { .. })
    ));

    service
        .revoke_device(&fixture.device_id)
        .expect("device known");

    assert_eq!(
        service.admit_command(&session, &envelope(agent_send("second")), NOW),
        Err(CommandError::Denial(Denial::DeviceRevoked))
    );
    assert_eq!(
        service.admit_command(&session, &first, NOW),
        Err(CommandError::Denial(Denial::DeviceRevoked)),
        "authorization runs before idempotency and reveals nothing"
    );
    assert_eq!(
        service.authorize_subscribe(&session, NOW),
        Err(Denial::DeviceRevoked)
    );
    assert_eq!(
        service.admit_command(&session, &pointer, NOW),
        Err(CommandError::Denial(Denial::DeviceRevoked))
    );
}

#[test]
fn stale_forged_and_unknown_identities_are_denied() {
    let mut service = service();
    let fixture = register_device(&mut service, &[Scope::AgentSend]);

    let unknown = DeviceClaim {
        device_id: DeviceId::new_random(),
        key_fingerprint: KeyFingerprint::new("fingerprint-unknown").expect("valid"),
        generation: IdentityGeneration::FIRST,
    };
    let hello = ClientHello::remote(RequestId::new_random(), vec![CURRENT_GENERATION], unknown);
    assert_eq!(
        service.accept_hello(&TransportBinding::remote(), &hello, NOW),
        Err(SessionError::Denial(Denial::UnknownDevice))
    );

    let mut forged = fixture.claim.clone();
    forged.key_fingerprint = KeyFingerprint::new("fingerprint-other").expect("valid");
    let hello = ClientHello::remote(RequestId::new_random(), vec![CURRENT_GENERATION], forged);
    assert_eq!(
        service.accept_hello(&TransportBinding::remote(), &hello, NOW),
        Err(SessionError::Denial(Denial::ForgedIdentity))
    );

    let mut future = fixture.claim.clone();
    future.generation = IdentityGeneration::from_raw(2);
    let hello = ClientHello::remote(RequestId::new_random(), vec![CURRENT_GENERATION], future);
    assert_eq!(
        service.accept_hello(&TransportBinding::remote(), &hello, NOW),
        Err(SessionError::Denial(Denial::ForgedIdentity))
    );

    let session = remote_session(&service, &fixture);
    let rotated = service
        .trust
        .rotate_key(
            &fixture.device_id,
            KeyFingerprint::new("fingerprint-rotated").expect("valid"),
        )
        .expect("rotation");
    assert_eq!(rotated, IdentityGeneration::from_raw(2));

    let hello = ClientHello::remote(
        RequestId::new_random(),
        vec![CURRENT_GENERATION],
        fixture.claim.clone(),
    );
    assert_eq!(
        service.accept_hello(&TransportBinding::remote(), &hello, NOW),
        Err(SessionError::Denial(Denial::StaleIdentity))
    );
    assert_eq!(
        service.admit_command(&session, &envelope(agent_send("after rotation")), NOW),
        Err(CommandError::Denial(Denial::StaleIdentity)),
        "rotation invalidates sessions minted for the old key generation"
    );
}

#[test]
fn duplicate_commands_are_idempotent_and_conflicts_are_typed() {
    let mut service = service();
    let fixture = register_device(&mut service, &[Scope::AgentSend]);
    let session = remote_session(&service, &fixture);
    let first = envelope(agent_send("run the tests"));

    let AdmissionDecision::Execute { key, .. } = service
        .admit_command(&session, &first, NOW)
        .expect("admitted")
    else {
        panic!("first attempt executes");
    };
    assert_eq!(
        service.admit_command(&session, &first, NOW),
        Ok(AdmissionDecision::Duplicate {
            request_id: first.request_id.clone(),
            result: None,
        }),
        "an in-flight duplicate must not be reported as failed"
    );

    service
        .complete_command(
            &key,
            CommandResult::Completed {
                detail: Some("done".to_owned()),
            },
        )
        .expect("completed");
    assert_eq!(
        service.admit_command(&session, &first, NOW),
        Ok(AdmissionDecision::Duplicate {
            request_id: first.request_id.clone(),
            result: Some(CommandResult::Completed {
                detail: Some("done".to_owned())
            }),
        })
    );

    let mut conflicting = envelope(agent_send("a different instruction"));
    conflicting.idempotency_key = key;
    assert_eq!(
        service.admit_command(&session, &conflicting, NOW),
        Err(CommandError::Idempotency(
            IdempotencyError::FingerprintMismatch
        ))
    );
}

#[test]
fn idempotency_keys_are_scoped_to_principal() {
    let mut service = service();
    let device_a = register_device(&mut service, &[Scope::AgentSend]);
    let device_b = register_device(&mut service, &[Scope::AgentSend]);
    let session_a = remote_session(&service, &device_a);
    let session_b = remote_session(&service, &device_b);

    let first = envelope(agent_send("same instruction"));
    let AdmissionDecision::Execute { key, .. } = service
        .admit_command(&session_a, &first, NOW)
        .expect("admitted")
    else {
        panic!("first device executes");
    };
    service
        .complete_command(
            &key,
            CommandResult::Accepted {
                dispatch_id: RequestId::new_random(),
            },
        )
        .expect("completed");

    assert_eq!(
        service.admit_command(&session_b, &first, NOW),
        Err(CommandError::Idempotency(IdempotencyError::CrossPrincipal)),
        "another device must not learn or reuse this retry record"
    );
}

#[test]
fn approval_and_pointer_are_never_replay_queued() {
    let mut queue = ReplayQueue::new(limits());
    let approval_envelope = envelope(approval(ApprovalDecision::Approve));
    assert_eq!(approval_envelope.replay_policy(), ReplayPolicy::NeverReplay);
    assert_eq!(
        queue.enqueue(approval_envelope.clone()),
        Err(ukk_protocol::QueueError::ReplayForbidden)
    );
    assert_eq!(
        queue.enqueue(envelope(support::pointer(None))),
        Err(ukk_protocol::QueueError::ReplayForbidden)
    );
    assert!(queue.is_empty());

    let replayable = envelope(agent_send("safe retry"));
    queue.enqueue(replayable.clone()).expect("idempotent");
    assert_eq!(queue.drain(10), vec![replayable]);
    assert!(queue.is_empty());
}

#[test]
fn admitted_approval_and_pointer_are_marked_in_session_only() {
    let mut service = service();
    let fixture = register_device(
        &mut service,
        &[Scope::ApprovalRespond, Scope::ScreenControl],
    );
    let session = remote_session(&service, &fixture);
    let lease = service
        .leases
        .issue(fixture.device_id.clone(), session_id(), NOW, 60_000)
        .expect("lease");

    for command in [
        approval(ApprovalDecision::Approve),
        support::pointer(Some(lease.id)),
    ] {
        match service
            .admit_command(&session, &envelope(command), NOW)
            .expect("admitted")
        {
            AdmissionDecision::Execute { replay_policy, .. } => {
                assert_eq!(replay_policy, ReplayPolicy::NeverReplay);
            }
            AdmissionDecision::Duplicate { .. } => panic!("first use is not a duplicate"),
        }
    }
}

#[test]
fn pointer_input_requires_a_scope_and_a_live_matching_lease() {
    let mut service = service();
    let fixture = register_device(&mut service, &[Scope::ScreenControl]);
    let session = remote_session(&service, &fixture);

    assert_eq!(
        service.admit_command(&session, &envelope(support::pointer(None)), NOW),
        Err(CommandError::Denial(Denial::ControllerLeaseMissing))
    );

    let other_device = DeviceId::new_random();
    let foreign_lease = service
        .leases
        .issue(other_device, session_id(), NOW, 60_000)
        .expect("lease");
    assert_eq!(
        service.admit_command(
            &session,
            &envelope(support::pointer(Some(foreign_lease.id.clone()))),
            NOW
        ),
        Err(CommandError::Denial(Denial::ControllerLeaseMismatch))
    );

    let other_session = DomainId::new("session-2").expect("valid");
    let wrong_session = service
        .leases
        .issue(fixture.device_id.clone(), other_session, NOW, 60_000)
        .expect("lease");
    assert_eq!(
        service.admit_command(
            &session,
            &envelope(support::pointer(Some(wrong_session.id.clone()))),
            NOW
        ),
        Err(CommandError::Denial(Denial::ControllerLeaseMismatch))
    );

    let expired = service
        .leases
        .issue(fixture.device_id.clone(), session_id(), NOW, 1_000)
        .expect("lease");
    assert_eq!(
        service.admit_command(
            &session,
            &envelope(support::pointer(Some(expired.id.clone()))),
            NOW + 1_000
        ),
        Err(CommandError::Denial(Denial::ControllerLeaseExpired))
    );

    let live = service
        .leases
        .issue(fixture.device_id.clone(), session_id(), NOW, 60_000)
        .expect("lease");
    assert!(matches!(
        service.admit_command(
            &session,
            &envelope(support::pointer(Some(live.id))),
            NOW + 1
        ),
        Ok(AdmissionDecision::Execute { .. })
    ));

    assert_eq!(
        service.leases.issue(
            fixture.device_id.clone(),
            session_id(),
            NOW,
            ukk_protocol::MAX_CONTROLLER_LEASE_MS + 1
        ),
        Err(LeaseError::InvalidTtl)
    );

    let read_only = register_device(&mut service, &[Scope::AgentSend]);
    let read_only_session = remote_session(&service, &read_only);
    let read_only_lease = service
        .leases
        .issue(read_only.device_id.clone(), session_id(), NOW, 60_000)
        .expect("lease");
    assert_eq!(
        service.admit_command(
            &read_only_session,
            &envelope(support::pointer(Some(read_only_lease.id))),
            NOW
        ),
        Err(CommandError::Denial(Denial::MissingScope {
            scope: Scope::ScreenControl
        }))
    );
}

#[test]
fn local_ipc_and_wan_trust_domains_are_distinct() {
    let mut service = service();
    let fixture = register_device(&mut service, &[Scope::AgentSend]);

    let device_hello = ClientHello::remote(
        RequestId::new_random(),
        vec![CURRENT_GENERATION],
        fixture.claim.clone(),
    );
    assert_eq!(
        service.accept_hello(&TransportBinding::local_ipc(501), &device_hello, NOW),
        Err(SessionError::Denial(Denial::PlaneMismatch)),
        "a remote device claim cannot be presented on the local socket"
    );

    let local_hello = ClientHello::local(RequestId::new_random(), vec![CURRENT_GENERATION]);
    assert_eq!(
        service.accept_hello(&TransportBinding::remote(), &local_hello, NOW),
        Err(SessionError::Denial(Denial::PlaneMismatch))
    );

    let unverified_local = TransportBinding {
        plane: Plane::LocalIpc,
        verified_local_uid: None,
    };
    assert_eq!(
        service.accept_hello(&unverified_local, &local_hello, NOW),
        Err(SessionError::Binding(
            ukk_protocol::BindingError::LocalPeerMissing
        ))
    );

    let remote_with_uid = TransportBinding {
        plane: Plane::Remote,
        verified_local_uid: Some(501),
    };
    assert_eq!(
        service.accept_hello(&remote_with_uid, &device_hello, NOW),
        Err(SessionError::Binding(
            ukk_protocol::BindingError::UnexpectedLocalPeer
        ))
    );

    let local = local_session(&service);
    let remote = remote_session(&service, &fixture);
    let shared = envelope(agent_send("same command"));
    assert!(matches!(
        service.admit_command(&remote, &shared, NOW),
        Ok(AdmissionDecision::Execute { .. })
    ));
    assert_eq!(
        service.admit_command(&local, &shared, NOW),
        Err(CommandError::Idempotency(IdempotencyError::CrossPlane)),
        "a key minted in the WAN domain cannot be replayed inside local IPC"
    );
    assert!(
        matches!(
            service.admit_command(&local, &envelope(agent_send("local only")), NOW),
            Ok(AdmissionDecision::Execute { .. })
        ),
        "the local OS owner does not need device grants"
    );
}

#[test]
fn expired_grants_are_denied_with_a_typed_reason() {
    let mut service = service();
    let fixture = register_device_with_ttl(
        &mut service,
        &[Scope::AgentSend, Scope::SessionObserve],
        NOW + 100,
    );
    let session = remote_session(&service, &fixture);

    assert_eq!(
        service.admit_command(&session, &envelope(agent_send("too late")), NOW + 100),
        Err(CommandError::Denial(Denial::GrantExpired {
            scope: Scope::AgentSend
        }))
    );
    assert_eq!(
        service.authorize_subscribe(&session, NOW + 100),
        Err(Denial::GrantExpired {
            scope: Scope::SessionObserve
        })
    );
}

#[test]
fn revoked_device_leases_are_withdrawn() {
    let mut service = service();
    let fixture = register_device(&mut service, &[Scope::ScreenControl]);
    let lease: ControllerLease = service
        .leases
        .issue(fixture.device_id.clone(), session_id(), NOW, 60_000)
        .expect("lease");
    service.revoke_device(&fixture.device_id).expect("revoked");
    assert_eq!(
        service
            .leases
            .verify(&lease.id, &fixture.device_id, &session_id(), NOW),
        Err(Denial::ControllerLeaseMismatch),
        "revocation removes the lease rather than leaving a dormant capability"
    );
}
