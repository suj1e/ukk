//! Noise pairing handshake: success, replay, wrong PSK, key substitution,
//! transcript mismatch, malformed input and scope escalation.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ukk_protocol::Scope;
use ukk_remote::{
    HostId, InMemoryDeviceKeyStore, PairingClient, PairingError, PairingHost, PairingInvite,
    PublicKey, RelayUrl, StaticKeypair, DEFAULT_GRANT_TTL_MS, DEFAULT_INVITE_TTL_MS,
    MAX_PAIRING_FRAME_BYTES, MAX_PAIRING_REQUEST_BYTES, MAX_PENDING_INVITES,
};

const NOW: i64 = 1_000_000;

fn relay() -> RelayUrl {
    RelayUrl::parse("https://relay.example.invalid").unwrap()
}

fn test_host() -> PairingHost {
    PairingHost::new(
        HostId::new("host-1").unwrap(),
        StaticKeypair::generate().unwrap(),
    )
}

fn new_invite(host: &mut PairingHost) -> PairingInvite {
    host.create_invite(relay(), NOW, DEFAULT_INVITE_TTL_MS)
        .unwrap()
}

fn mutate(invite: &PairingInvite, field: &str, value: serde_json::Value) -> PairingInvite {
    let encoded = invite.encode().unwrap();
    let mut value_object: serde_json::Value = serde_json::from_str(&encoded).unwrap();
    value_object[field] = value;
    PairingInvite::decode(&value_object.to_string()).unwrap()
}

fn secret_b64(secret: [u8; 32]) -> String {
    URL_SAFE_NO_PAD.encode(secret)
}

#[test]
fn completes_pairing_with_agreeing_code_and_scoped_grant() {
    let mut host = test_host();
    let invite = new_invite(&mut host);
    let store = Box::new(InMemoryDeviceKeyStore::new());
    let client =
        PairingClient::new(invite, store, &[Scope::SessionObserve, Scope::AgentSend]).unwrap();
    let request = client.request_frame().unwrap();
    let (hello, handshake) = client.start(NOW + 10).unwrap();

    let attempt = host.accept(&request, &hello, NOW + 20).unwrap();
    assert_eq!(attempt.device_id(), client.device_id());
    assert_eq!(attempt.requested_scopes().len(), 2);
    assert_eq!(attempt.host_id().as_str(), "host-1");
    let response = attempt.response_frame().to_vec();
    let pending = handshake.finish(&response, NOW + 30).unwrap();

    let code = attempt.confirmation_code();
    assert_eq!(code.digits().len(), 6);
    assert_eq!(pending.confirmation_code(), code);
    assert_eq!(pending.handshake_hash(), attempt.handshake_hash());
    assert_eq!(pending.prologue_digest(), attempt.prologue_digest());

    let approved = host
        .approve(
            attempt,
            &[Scope::SessionObserve],
            DEFAULT_GRANT_TTL_MS,
            NOW + 40,
        )
        .unwrap();
    let device = pending.await_approval(approved.frame(), NOW + 50).unwrap();
    assert_eq!(&device.device_id, client.device_id());
    assert_eq!(device.host_id.as_str(), "host-1");
    assert_eq!(device.scopes.len(), 1);
    assert!(device.scopes.contains(Scope::SessionObserve));
    assert!(!device.scopes.contains(Scope::AgentSend));
    assert_eq!(device.public_key, client.device_public_key());
    assert!(device.grant_expires_at_ms > NOW + 50);
}

#[test]
fn rejects_a_different_invite_secret() {
    let mut host = test_host();
    let invite = new_invite(&mut host);
    let tampered = mutate(
        &invite,
        "invite_secret",
        serde_json::json!(secret_b64([9_u8; 32])),
    );
    let client = PairingClient::new(
        tampered,
        Box::new(InMemoryDeviceKeyStore::new()),
        &[Scope::SessionObserve],
    )
    .unwrap();
    let request = client.request_frame().unwrap();
    let (hello, _handshake) = client.start(NOW + 10).unwrap();
    assert!(matches!(
        host.accept(&request, &hello, NOW + 20),
        Err(PairingError::HandshakeFailed)
    ));
}

#[test]
fn rejects_host_key_substitution() {
    let mut host = test_host();
    let invite = new_invite(&mut host);
    let attacker_key = StaticKeypair::generate().unwrap().public_key();
    let tampered = mutate(
        &invite,
        "host_public_key",
        serde_json::json!(attacker_key.to_base64()),
    );
    let client = PairingClient::new(
        tampered,
        Box::new(InMemoryDeviceKeyStore::new()),
        &[Scope::SessionObserve],
    )
    .unwrap();
    let request = client.request_frame().unwrap();
    let (hello, _handshake) = client.start(NOW + 10).unwrap();
    assert!(matches!(
        host.accept(&request, &hello, NOW + 20),
        Err(PairingError::HandshakeFailed)
    ));
}

#[test]
fn rejects_transcript_field_changes() {
    for (field, value) in [
        ("host_id", serde_json::json!("host-2")),
        ("invite_id", serde_json::json!("invite-2")),
        (
            "expires_at_ms",
            serde_json::json!(NOW + DEFAULT_INVITE_TTL_MS + 1),
        ),
        (
            "relay",
            serde_json::json!("https://other-relay.example.invalid"),
        ),
    ] {
        let mut host = test_host();
        let invite = new_invite(&mut host);
        let tampered = mutate(&invite, field, value);
        let client = PairingClient::new(
            tampered,
            Box::new(InMemoryDeviceKeyStore::new()),
            &[Scope::SessionObserve],
        )
        .unwrap();
        let request = client.request_frame().unwrap();
        let (hello, _handshake) = client.start(NOW + 10).unwrap();
        assert!(
            host.accept(&request, &hello, NOW + 20).is_err(),
            "field {field} must be transcript-bound"
        );
    }
}

#[test]
fn rejects_unknown_invite_and_replay() {
    let mut host = test_host();
    let invite = new_invite(&mut host);
    let unknown = mutate(&invite, "invite_id", serde_json::json!("unknown-invite"));
    let client = PairingClient::new(
        unknown,
        Box::new(InMemoryDeviceKeyStore::new()),
        &[Scope::SessionObserve],
    )
    .unwrap();
    let request = client.request_frame().unwrap();
    let (hello, _handshake) = client.start(NOW + 10).unwrap();
    assert!(matches!(
        host.accept(&request, &hello, NOW + 20),
        Err(PairingError::UnknownInvite)
    ));

    let mut host = test_host();
    let invite = new_invite(&mut host);
    let first = PairingClient::new(
        invite.clone(),
        Box::new(InMemoryDeviceKeyStore::new()),
        &[Scope::SessionObserve],
    )
    .unwrap();
    let request = first.request_frame().unwrap();
    let (hello, _handshake) = first.start(NOW + 10).unwrap();
    host.accept(&request, &hello, NOW + 20).unwrap();

    let replay = PairingClient::new(
        invite,
        Box::new(InMemoryDeviceKeyStore::new()),
        &[Scope::SessionObserve],
    )
    .unwrap();
    let request = replay.request_frame().unwrap();
    let (hello, _handshake) = replay.start(NOW + 10).unwrap();
    assert!(matches!(
        host.accept(&request, &hello, NOW + 20),
        Err(PairingError::InviteAlreadyUsed)
    ));
}

#[test]
fn rejects_expired_invites_on_both_sides() {
    let mut host = test_host();
    let invite = host.create_invite(relay(), NOW, 1_000).unwrap();
    let client = PairingClient::new(
        invite.clone(),
        Box::new(InMemoryDeviceKeyStore::new()),
        &[Scope::SessionObserve],
    )
    .unwrap();
    let request = client.request_frame().unwrap();
    assert!(matches!(
        client.start(NOW + 1_000),
        Err(PairingError::InviteExpired)
    ));
    let (hello, _handshake) = client.start(NOW + 999).unwrap();
    assert!(matches!(
        host.accept(&request, &hello, NOW + 1_000),
        Err(PairingError::InviteExpired)
    ));
}

#[test]
fn rejects_tampered_or_oversized_frames() {
    let mut host = test_host();
    let invite = new_invite(&mut host);
    let client = PairingClient::new(
        invite,
        Box::new(InMemoryDeviceKeyStore::new()),
        &[Scope::SessionObserve],
    )
    .unwrap();
    let request = client.request_frame().unwrap();
    let (mut hello, _handshake) = client.start(NOW + 10).unwrap();
    hello[0] ^= 0x01;
    assert!(matches!(
        host.accept(&request, &hello, NOW + 20),
        Err(PairingError::HandshakeFailed)
    ));

    let mut host = test_host();
    let invite = new_invite(&mut host);
    let client = PairingClient::new(
        invite,
        Box::new(InMemoryDeviceKeyStore::new()),
        &[Scope::SessionObserve],
    )
    .unwrap();
    let request = client.request_frame().unwrap();
    let (hello, _handshake) = client.start(NOW + 10).unwrap();
    let oversized_request = vec![0_u8; MAX_PAIRING_REQUEST_BYTES + 1];
    assert!(matches!(
        host.accept(&oversized_request, &hello, NOW + 20),
        Err(PairingError::FrameTooLarge { .. })
    ));
    let oversized_hello = vec![0_u8; MAX_PAIRING_FRAME_BYTES + 1];
    assert!(matches!(
        host.accept(&request, &oversized_hello, NOW + 20),
        Err(PairingError::FrameTooLarge { .. })
    ));
    assert!(matches!(
        host.accept(b"", b"", NOW + 20),
        Err(PairingError::InviteMalformed)
    ));
}

#[test]
fn client_rejects_a_tampered_host_response() {
    let mut host = test_host();
    let invite = new_invite(&mut host);
    let client = PairingClient::new(
        invite,
        Box::new(InMemoryDeviceKeyStore::new()),
        &[Scope::SessionObserve],
    )
    .unwrap();
    let request = client.request_frame().unwrap();
    let (hello, handshake) = client.start(NOW + 10).unwrap();
    let attempt = host.accept(&request, &hello, NOW + 20).unwrap();
    let mut response = attempt.response_frame().to_vec();
    response[0] ^= 0x01;
    assert!(matches!(
        handshake.finish(&response, NOW + 30),
        Err(PairingError::HandshakeFailed)
    ));
}

#[test]
fn approval_cannot_escalate_requested_scopes() {
    let mut host = test_host();
    let invite = new_invite(&mut host);
    let client = PairingClient::new(
        invite,
        Box::new(InMemoryDeviceKeyStore::new()),
        &[Scope::SessionObserve],
    )
    .unwrap();
    let request = client.request_frame().unwrap();
    let (hello, handshake) = client.start(NOW + 10).unwrap();
    let attempt = host.accept(&request, &hello, NOW + 20).unwrap();
    let _pending = handshake
        .finish(attempt.response_frame(), NOW + 30)
        .unwrap();
    assert!(matches!(
        host.approve(
            attempt,
            &[Scope::SessionObserve, Scope::AgentSend],
            DEFAULT_GRANT_TTL_MS,
            NOW + 40
        ),
        Err(PairingError::ScopeEscalation)
    ));
}

#[test]
fn device_manage_cannot_be_requested_or_granted_by_pairing() {
    let mut host = test_host();
    let invite = new_invite(&mut host);
    assert!(matches!(
        PairingClient::new(
            invite,
            Box::new(InMemoryDeviceKeyStore::new()),
            &[Scope::DeviceManage]
        ),
        Err(PairingError::ScopeNotGrantable)
    ));

    let invite = new_invite(&mut host);
    let client = PairingClient::new(
        invite,
        Box::new(InMemoryDeviceKeyStore::new()),
        &[Scope::SessionObserve],
    )
    .unwrap();
    let request = client.request_frame().unwrap();
    let (hello, handshake) = client.start(NOW + 10).unwrap();
    let attempt = host.accept(&request, &hello, NOW + 20).unwrap();
    let _pending = handshake
        .finish(attempt.response_frame(), NOW + 30)
        .unwrap();
    assert!(matches!(
        host.approve(
            attempt,
            &[Scope::DeviceManage],
            DEFAULT_GRANT_TTL_MS,
            NOW + 40
        ),
        Err(PairingError::ScopeNotGrantable)
    ));
}

#[test]
fn client_rejects_an_already_expired_grant() {
    let mut host = test_host();
    let invite = new_invite(&mut host);
    let client = PairingClient::new(
        invite,
        Box::new(InMemoryDeviceKeyStore::new()),
        &[Scope::SessionObserve],
    )
    .unwrap();
    let request = client.request_frame().unwrap();
    let (hello, handshake) = client.start(NOW + 10).unwrap();
    let attempt = host.accept(&request, &hello, NOW + 20).unwrap();
    let pending = handshake
        .finish(attempt.response_frame(), NOW + 30)
        .unwrap();
    let approved = host
        .approve(attempt, &[Scope::SessionObserve], 1, NOW + 40)
        .unwrap();
    assert!(matches!(
        pending.await_approval(approved.frame(), NOW + 41),
        Err(PairingError::GrantExpired)
    ));
}

#[test]
fn host_rejects_invalid_invite_ttl() {
    let mut host = test_host();
    assert!(matches!(
        host.create_invite(relay(), NOW, 0),
        Err(PairingError::InvalidTtl)
    ));
    assert!(matches!(
        host.create_invite(relay(), NOW, 365 * 24 * 60 * 60 * 1000),
        Err(PairingError::InvalidTtl)
    ));
}

#[test]
fn pairing_is_bound_to_the_device_public_key_in_the_key_store() {
    let mut host = test_host();
    let invite = new_invite(&mut host);
    let store = Box::new(InMemoryDeviceKeyStore::new());
    let client = PairingClient::new(invite, store, &[Scope::SessionObserve]).unwrap();
    let device_id = client.device_id().clone();
    let public_key = client.device_public_key();
    let request = client.request_frame().unwrap();
    let (hello, handshake) = client.start(NOW + 10).unwrap();
    let attempt = host.accept(&request, &hello, NOW + 20).unwrap();
    let pending = handshake
        .finish(attempt.response_frame(), NOW + 30)
        .unwrap();
    let approved = host
        .approve(
            attempt,
            &[Scope::SessionObserve],
            DEFAULT_GRANT_TTL_MS,
            NOW + 40,
        )
        .unwrap();
    let device = pending.await_approval(approved.frame(), NOW + 50).unwrap();
    assert_eq!(device.device_id, device_id);
    assert_eq!(device.public_key, public_key);
    assert_eq!(device.fingerprint, public_key.fingerprint().unwrap());
    let stored = client.key_store().load(&device_id).unwrap().unwrap();
    assert_eq!(stored.public_key(), public_key);
}

#[test]
fn outstanding_invites_are_bounded() {
    let mut host = test_host();
    for _ in 0..MAX_PENDING_INVITES {
        host.create_invite(relay(), NOW, DEFAULT_INVITE_TTL_MS)
            .unwrap();
    }
    assert!(matches!(
        host.create_invite(relay(), NOW, DEFAULT_INVITE_TTL_MS),
        Err(PairingError::TooManyInvites)
    ));
}

#[test]
fn public_key_is_not_a_placeholder() {
    let key = PublicKey::from_bytes([4_u8; 32]);
    assert_eq!(key.to_base64().len(), 43);
    assert_eq!(PublicKey::from_base64(&key.to_base64()), Some(key));
}
