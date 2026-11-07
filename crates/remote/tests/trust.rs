//! Device grant issuance, revocation, rotation and scope separation.

use ukk_protocol::{Denial, DeviceClaim, DeviceId, DeviceStatus, IdentityGeneration, Scope};
use ukk_remote::{DeviceTrust, HostId, PublicKey, DEFAULT_GRANT_TTL_MS, MAX_GRANT_TTL_MS};

fn host_id() -> HostId {
    HostId::new("host-1").unwrap()
}

fn device_id() -> DeviceId {
    DeviceId::new("device-1").unwrap()
}

#[test]
fn approved_scopes_are_independent_and_expire_together() {
    let mut trust = DeviceTrust::new();
    let key = PublicKey::from_bytes([11_u8; 32]);
    let paired = trust
        .approve(
            &host_id(),
            device_id(),
            key,
            &[
                Scope::SessionObserve,
                Scope::AgentSend,
                Scope::ApprovalRespond,
                Scope::ScreenView,
            ],
            DEFAULT_GRANT_TTL_MS,
            1_000,
        )
        .unwrap();
    assert_eq!(paired.generation, IdentityGeneration::FIRST);
    assert_eq!(paired.scopes.len(), 4);
    assert!(paired.scopes.contains(Scope::SessionObserve));
    assert!(paired.scopes.contains(Scope::AgentSend));
    assert!(paired.scopes.contains(Scope::ApprovalRespond));
    assert!(paired.scopes.contains(Scope::ScreenView));
    assert!(!paired.scopes.contains(Scope::ScreenControl));
    assert!(!paired.scopes.contains(Scope::DeviceManage));

    let claim = DeviceClaim {
        device_id: paired.device_id.clone(),
        key_fingerprint: paired.fingerprint.clone(),
        generation: paired.generation,
    };
    let authenticated = trust.authenticate(&claim, 2_000).unwrap();
    let (valid, expired) = trust.live_grants(&authenticated, 2_000).unwrap();
    assert_eq!(valid.len(), 4);
    assert!(expired.is_empty());

    let (valid, expired) = trust
        .live_grants(&authenticated, paired.grant_expires_at_ms)
        .unwrap();
    assert!(valid.is_empty());
    assert_eq!(expired.len(), 4);
}

#[test]
fn empty_scope_sets_grant_nothing() {
    let mut trust = DeviceTrust::new();
    let paired = trust
        .approve(
            &host_id(),
            device_id(),
            PublicKey::from_bytes([12_u8; 32]),
            &[],
            DEFAULT_GRANT_TTL_MS,
            1_000,
        )
        .unwrap();
    assert!(paired.scopes.is_empty());
    let claim = DeviceClaim {
        device_id: paired.device_id.clone(),
        key_fingerprint: paired.fingerprint.clone(),
        generation: paired.generation,
    };
    let authenticated = trust.authenticate(&claim, 2_000).unwrap();
    assert!(trust
        .live_grants(&authenticated, 2_000)
        .unwrap()
        .0
        .is_empty());
}

#[test]
fn duplicate_registration_and_same_key_rotation_are_rejected() {
    let mut trust = DeviceTrust::new();
    let key = PublicKey::from_bytes([13_u8; 32]);
    trust
        .approve(
            &host_id(),
            device_id(),
            key,
            &[Scope::SessionObserve],
            DEFAULT_GRANT_TTL_MS,
            1_000,
        )
        .unwrap();
    assert!(matches!(
        trust.approve(
            &host_id(),
            device_id(),
            PublicKey::from_bytes([14_u8; 32]),
            &[Scope::SessionObserve],
            DEFAULT_GRANT_TTL_MS,
            1_000,
        ),
        Err(ukk_remote::PairingError::DeviceAlreadyRegistered)
    ));
    assert!(matches!(
        trust.rotate_key(&device_id(), key),
        Err(ukk_remote::PairingError::SameKey)
    ));
}

#[test]
fn revocation_clears_grants_and_invalidates_live_sessions() {
    let mut trust = DeviceTrust::new();
    let key = PublicKey::from_bytes([15_u8; 32]);
    let paired = trust
        .approve(
            &host_id(),
            device_id(),
            key,
            &[Scope::SessionObserve, Scope::AgentSend],
            DEFAULT_GRANT_TTL_MS,
            1_000,
        )
        .unwrap();
    let claim = DeviceClaim {
        device_id: paired.device_id.clone(),
        key_fingerprint: paired.fingerprint.clone(),
        generation: paired.generation,
    };
    let authenticated = trust.authenticate(&claim, 2_000).unwrap();
    trust.revoke(&paired.device_id).unwrap();
    assert_eq!(trust.status(&paired.device_id), Some(DeviceStatus::Revoked));
    assert_eq!(
        trust.authenticate(&claim, 2_000),
        Err(Denial::DeviceRevoked)
    );
    assert_eq!(
        trust.live_grants(&authenticated, 2_000),
        Err(Denial::DeviceRevoked)
    );
    assert_eq!(
        trust.revoke(&DeviceId::new("missing").unwrap()),
        Err(ukk_remote::PairingError::UnknownDevice)
    );
}

#[test]
fn key_rotation_invalidates_old_generation_and_clears_grants() {
    let mut trust = DeviceTrust::new();
    let old_key = PublicKey::from_bytes([16_u8; 32]);
    let paired = trust
        .approve(
            &host_id(),
            device_id(),
            old_key,
            &[Scope::AgentSend],
            DEFAULT_GRANT_TTL_MS,
            1_000,
        )
        .unwrap();
    let old_claim = DeviceClaim {
        device_id: paired.device_id.clone(),
        key_fingerprint: paired.fingerprint.clone(),
        generation: paired.generation,
    };
    let old_session = trust.authenticate(&old_claim, 2_000).unwrap();

    let new_key = PublicKey::from_bytes([17_u8; 32]);
    let new_generation = trust.rotate_key(&paired.device_id, new_key).unwrap();
    assert_eq!(new_generation.raw(), 2);
    assert_eq!(
        trust.current_generation(&paired.device_id),
        Some(new_generation)
    );
    assert_eq!(
        trust.authenticate(&old_claim, 2_000),
        Err(Denial::StaleIdentity)
    );
    assert_eq!(
        trust.live_grants(&old_session, 2_000),
        Err(Denial::StaleIdentity)
    );

    let new_session = trust
        .authenticate(
            &DeviceClaim {
                device_id: paired.device_id.clone(),
                key_fingerprint: new_key.fingerprint().unwrap(),
                generation: new_generation,
            },
            2_000,
        )
        .unwrap();
    assert!(trust.live_grants(&new_session, 2_000).unwrap().0.is_empty());
}

#[test]
fn grant_ttl_bounds_are_enforced() {
    let mut trust = DeviceTrust::new();
    for ttl in [0, -1, MAX_GRANT_TTL_MS + 1] {
        assert!(matches!(
            trust.approve(
                &host_id(),
                DeviceId::new(format!("device-{ttl}")).unwrap(),
                PublicKey::from_bytes([18_u8; 32]),
                &[Scope::SessionObserve],
                ttl,
                1_000,
            ),
            Err(ukk_remote::PairingError::InvalidTtl)
        ));
    }
    assert!(trust
        .approve(
            &host_id(),
            DeviceId::new("device-max").unwrap(),
            PublicKey::from_bytes([19_u8; 32]),
            &[Scope::SessionObserve],
            MAX_GRANT_TTL_MS,
            1_000,
        )
        .is_ok());
}

#[test]
fn device_manage_is_not_grantable_through_pairing() {
    let mut trust = DeviceTrust::new();
    assert!(matches!(
        trust.approve(
            &host_id(),
            device_id(),
            PublicKey::from_bytes([20_u8; 32]),
            &[Scope::DeviceManage],
            DEFAULT_GRANT_TTL_MS,
            1_000,
        ),
        Err(ukk_remote::PairingError::ScopeNotGrantable)
    ));
    assert!(trust.status(&device_id()).is_none());
}
