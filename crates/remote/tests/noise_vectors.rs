//! Deterministic Noise vector for the exact pattern, PSK placement and
//! confirmation-code derivation. Fixed static keys, fixed ephemerals and a
//! fixed challenge make the transcript reproducible; this is a regression
//! vector, not a general KAT for the underlying primitives.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ukk_protocol::Scope;
use ukk_remote::{
    ConfirmationCode, PairingChallenge, PairingInvite, PairingTranscript, PublicKey, StaticKeypair,
    NOISE_PATTERN,
};

const NOW: i64 = 1_000_000;

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        write!(&mut out, "{byte:02x}").unwrap();
    }
    out
}

fn fixed_invite() -> PairingInvite {
    let host = StaticKeypair::from_secret([1_u8; 32]);
    let encoded = serde_json::json!({
        "v": 1,
        "relay": "https://relay.example.invalid",
        "host_id": "host-vector",
        "host_public_key": host.public_key().to_base64(),
        "invite_id": "invite-vector",
        "invite_secret": URL_SAFE_NO_PAD.encode([5_u8; 32]),
        "expires_at_ms": NOW + 120_000,
    })
    .to_string();
    PairingInvite::decode(&encoded).unwrap()
}

#[test]
fn deterministic_ikpsk0_transcript_and_confirmation_code() {
    assert!(NOISE_PATTERN.contains("IKpsk0"));
    assert!(NOISE_PATTERN.contains("25519"));
    assert!(NOISE_PATTERN.contains("ChaChaPoly"));
    assert!(NOISE_PATTERN.contains("SHA256"));

    let invite = fixed_invite();
    let host = StaticKeypair::from_secret([1_u8; 32]);
    let device = StaticKeypair::from_secret([2_u8; 32]);
    let transcript = PairingTranscript::from_invite(&invite);
    let prologue = transcript.prologue();
    let params = NOISE_PATTERN.parse::<snow::params::NoiseParams>().unwrap();
    let params2 = NOISE_PATTERN.parse::<snow::params::NoiseParams>().unwrap();

    let mut initiator = snow::Builder::new(params)
        .local_private_key(device.secret_bytes())
        .unwrap()
        .remote_public_key(host.public_key().as_bytes())
        .unwrap()
        .prologue(&prologue)
        .unwrap()
        .psk(0, &[5_u8; 32])
        .unwrap()
        .fixed_ephemeral_key_for_testing_only(&[3_u8; 32])
        .build_initiator()
        .unwrap();
    let mut responder = snow::Builder::new(params2)
        .local_private_key(host.secret_bytes())
        .unwrap()
        .prologue(&prologue)
        .unwrap()
        .psk(0, &[5_u8; 32])
        .unwrap()
        .fixed_ephemeral_key_for_testing_only(&[4_u8; 32])
        .build_responder()
        .unwrap();

    let challenge = PairingChallenge {
        nonce: [0x11_u8; 32],
        expires_at_ms: NOW + 60_000,
    };
    let challenge_payload = serde_json::to_vec(&challenge).unwrap();

    let mut first = [0_u8; 1024];
    let first_len = initiator
        .write_message(b"client-hello", &mut first)
        .unwrap();
    let mut payload = [0_u8; 1024];
    responder
        .read_message(&first[..first_len], &mut payload)
        .unwrap();

    let mut second = [0_u8; 1024];
    let second_len = responder
        .write_message(&challenge_payload, &mut second)
        .unwrap();
    initiator
        .read_message(&second[..second_len], &mut payload)
        .unwrap();

    let initiator_hash = initiator.get_handshake_hash().to_vec();
    let responder_hash = responder.get_handshake_hash().to_vec();
    assert_eq!(initiator_hash, responder_hash);
    assert_eq!(initiator_hash.len(), 32);
    let code = ConfirmationCode::from_handshake(&initiator_hash, &challenge);

    assert_eq!(
        hex(&initiator_hash),
        "c07270fe1f99162bf0f248c09f653b3deed7e1a2fd052dd3f49dce5e1edc67a3"
    );
    assert_eq!(code.digits(), "555244");
}

#[test]
fn confirmation_code_binds_challenge_and_prologue() {
    let invite = fixed_invite();
    let transcript = PairingTranscript::from_invite(&invite);
    let digest = transcript.digest();
    let challenge = PairingChallenge {
        nonce: [0x11_u8; 32],
        expires_at_ms: NOW + 60_000,
    };
    let code = ConfirmationCode::from_handshake(&digest, &challenge);

    let other_challenge = PairingChallenge {
        nonce: [0x12_u8; 32],
        expires_at_ms: NOW + 60_000,
    };
    assert_ne!(
        code,
        ConfirmationCode::from_handshake(&digest, &other_challenge)
    );

    let mut value: serde_json::Value = serde_json::from_str(&invite.encode().unwrap()).unwrap();
    value["host_id"] = serde_json::json!("host-other");
    let other_host = PairingInvite::decode(&value.to_string()).unwrap();
    let other_digest = PairingTranscript::from_invite(&other_host).digest();
    assert_ne!(digest, other_digest);
    assert_ne!(
        code,
        ConfirmationCode::from_handshake(&other_digest, &challenge)
    );

    assert_eq!(digest.len(), 32);
    assert_eq!(PublicKey::from_bytes([0_u8; 32]).to_base64().len(), 43);
    assert_eq!(Scope::SessionObserve.as_str(), "session.observe");
}
