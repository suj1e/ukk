//! QR payload contract: strict parsing, redaction, expiry and size bounds.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ukk_protocol::Scope;
use ukk_remote::{
    HostId, InviteId, PairingError, PairingInvite, PublicKey, RelayUrl, DEFAULT_INVITE_TTL_MS,
    MAX_QR_BYTES,
};

const NOW: i64 = 1_000_000;

fn relay() -> RelayUrl {
    RelayUrl::parse("https://relay.example.invalid").unwrap()
}

fn invite(host_public_key: PublicKey, secret: [u8; 32]) -> PairingInvite {
    // Build through the public decoder so this test exercises the same strict
    // validation a scanned QR goes through.
    let encoded = serde_json::json!({
        "v": 1,
        "relay": "https://relay.example.invalid",
        "host_id": "host-1",
        "host_public_key": host_public_key.to_base64(),
        "invite_id": "invite-1",
        "invite_secret": URL_SAFE_NO_PAD.encode(secret),
        "expires_at_ms": NOW + DEFAULT_INVITE_TTL_MS,
    })
    .to_string();
    PairingInvite::decode(&encoded).unwrap()
}

fn sample_invite() -> PairingInvite {
    invite(PublicKey::from_bytes([1_u8; 32]), [2_u8; 32])
}

#[test]
fn round_trips_exact_fields_and_hides_the_secret() {
    let invite = sample_invite();
    let encoded = invite.encode().unwrap();
    let decoded = PairingInvite::decode(&encoded).unwrap();
    assert_eq!(decoded.version(), 1);
    assert_eq!(decoded.relay(), &relay());
    assert_eq!(decoded.host_id().as_str(), "host-1");
    assert_eq!(decoded.invite_id().as_str(), "invite-1");
    assert_eq!(decoded.expires_at_ms(), NOW + DEFAULT_INVITE_TTL_MS);
    assert_eq!(decoded.host_public_key(), PublicKey::from_bytes([1_u8; 32]));

    let secret_b64 = URL_SAFE_NO_PAD.encode([2_u8; 32]);
    let debug = format!("{invite:?}");
    let display = format!("{invite}");
    assert!(debug.contains("redacted"));
    assert!(!debug.contains(&secret_b64));
    assert!(!display.contains(&secret_b64));
    assert!(!format!("{decoded:?}").contains(&secret_b64));
}

#[test]
fn rejects_unknown_missing_and_wrongly_typed_fields() {
    let encoded = sample_invite().encode().unwrap();
    let mut value: serde_json::Value = serde_json::from_str(&encoded).unwrap();
    value["extra"] = serde_json::json!("no");
    assert!(matches!(
        PairingInvite::decode(&value.to_string()),
        Err(PairingError::InviteMalformed)
    ));

    let mut value: serde_json::Value = serde_json::from_str(&encoded).unwrap();
    value.as_object_mut().unwrap().remove("host_public_key");
    assert!(matches!(
        PairingInvite::decode(&value.to_string()),
        Err(PairingError::InviteMalformed)
    ));

    let mut value: serde_json::Value = serde_json::from_str(&encoded).unwrap();
    value["relay"] = serde_json::json!("http://relay.example.invalid");
    assert!(matches!(
        PairingInvite::decode(&value.to_string()),
        Err(PairingError::InviteMalformed)
    ));
}

#[test]
fn rejects_versions_expiry_and_secret_encodings() {
    let encoded = sample_invite().encode().unwrap();
    let mut value: serde_json::Value = serde_json::from_str(&encoded).unwrap();
    value["v"] = serde_json::json!(2);
    assert!(matches!(
        PairingInvite::decode(&value.to_string()),
        Err(PairingError::UnsupportedInviteVersion { found: 2 })
    ));

    let mut value: serde_json::Value = serde_json::from_str(&encoded).unwrap();
    value["expires_at_ms"] = serde_json::json!(0);
    assert!(matches!(
        PairingInvite::decode(&value.to_string()),
        Err(PairingError::InvalidTtl)
    ));

    for secret in [
        "",
        "AAAA",
        &format!("{}==", URL_SAFE_NO_PAD.encode([1_u8; 32])),
    ] {
        let mut value: serde_json::Value = serde_json::from_str(&encoded).unwrap();
        value["invite_secret"] = serde_json::json!(secret);
        assert!(matches!(
            PairingInvite::decode(&value.to_string()),
            Err(PairingError::InviteMalformed)
        ));
    }

    let mut value: serde_json::Value = serde_json::from_str(&encoded).unwrap();
    value["host_public_key"] = serde_json::json!(PublicKey::from_bytes([0_u8; 32]).to_base64());
    assert!(matches!(
        PairingInvite::decode(&value.to_string()),
        Err(PairingError::InvalidPublicKey)
    ));
}

#[test]
fn rejects_oversized_and_trailing_input() {
    let encoded = sample_invite().encode().unwrap();
    let oversized = format!("{encoded}{}", "z".repeat(MAX_QR_BYTES));
    assert!(matches!(
        PairingInvite::decode(&oversized),
        Err(PairingError::InviteOversized { max: MAX_QR_BYTES })
    ));
    let trailing = format!("{encoded}{{}}");
    assert!(matches!(
        PairingInvite::decode(&trailing),
        Err(PairingError::InviteMalformed)
    ));
}

#[test]
fn rejects_unsafe_relay_urls() {
    for raw in [
        "http://relay.example.invalid",
        "ws://relay.example.invalid",
        "https://user:pw@relay.example.invalid",
        "wss://relay.example.invalid/#fragment",
        "https://relay.example.invalid:0",
        "file:///tmp/relay",
        "relay.example.invalid",
    ] {
        assert!(
            matches!(
                RelayUrl::parse(raw),
                Err(PairingError::InvalidRelayUrl)
                    | Err(PairingError::RelaySchemeNotAllowed)
                    | Err(PairingError::RelayUserInfoForbidden)
                    | Err(PairingError::RelayFragmentForbidden)
                    | Err(PairingError::RelayQueryForbidden)
            ),
            "{raw}"
        );
    }
    assert!(RelayUrl::parse("https://relay.example.invalid").is_ok());
    assert!(RelayUrl::parse("wss://relay.example.invalid/control").is_ok());
}

#[test]
fn expiry_is_checked_against_the_local_clock() {
    let invite = sample_invite();
    assert!(invite.validate_at(NOW + 1).is_ok());
    assert!(matches!(
        invite.validate_at(invite.expires_at_ms()),
        Err(PairingError::InviteExpired)
    ));
    assert!(invite.is_expired_at(invite.expires_at_ms()));
}

#[test]
fn identifiers_are_validated_and_never_empty() {
    assert!(HostId::new("").is_err());
    assert!(HostId::new("host with space").is_err());
    assert!(InviteId::new("a\nb").is_err());
    assert!(HostId::new_random().as_str().len() > 10);
    assert!(InviteId::new_random().as_str().len() > 10);
}

#[test]
fn scope_constants_are_distinct_wire_strings() {
    // Pairing reuses the S1 scope contract rather than defining a second one.
    assert_ne!(Scope::SessionObserve.as_str(), Scope::AgentSend.as_str());
    assert_ne!(
        Scope::ApprovalRespond.as_str(),
        Scope::ScreenControl.as_str()
    );
    assert_ne!(Scope::ScreenView.as_str(), Scope::ScreenControl.as_str());
}
