//! Pairing, device trust and the relay boundary for ukk remote clients.
//!
//! This crate is the S2 core, not a networking or UI implementation. It owns:
//!
//! * [`invite`] — the versioned QR payload, strict validation and secret
//!   redaction/zeroization.
//! * [`handshake`] — a Noise `IKpsk0` pairing handshake with the QR secret as
//!   PSK, the Host static key pinned from the QR, transcript binding and a
//!   one-time challenge for a host-side confirmation code.
//! * [`trust`] — scoped device grants layered on `ukk-protocol`'s live trust
//!   registry, including rotation, revocation and grant expiry.
//! * [`keys`] — static X25519 keys and the [`keys::DeviceKeyStore`] trait with
//!   an in-memory test implementation. OS Keychain/Keyring is out of scope.
//! * [`transport`] — the opaque relay frame trait and documented outbound WSS
//!   assumptions. No relay wire protocol is implemented.
//!
//! ## What is proven by tests
//!
//! Invite parsing and redaction/expiry/replay, wrong PSK, Host key
//! substitution, transcript/Host mismatch, confirmation-code agreement,
//! scope-escalation denial, device revocation/key rotation and bounded
//! malformed input all have contract tests. A deterministic Noise vector is
//! included.
//!
//! ## What is NOT VERIFIED
//!
//! No real relay was contacted, no OS key store was used, no physical iPhone
//! ran this code and there has been no external cryptographic or security
//! review. Do not describe this as production-ready cryptography.

pub mod error;
pub mod handshake;
pub mod invite;
pub mod keys;
pub mod transport;
pub mod trust;

mod encoding;

pub use error::{PairingError, MAX_PAIRING_FRAME_BYTES, MAX_PAIRING_REQUEST_BYTES, MAX_QR_BYTES};
pub use handshake::{
    ApprovedPairing, ClientHandshake, ClientPairingPending, ConfirmationCode, HostPairingAttempt,
    InviteRegistry, PairingChallenge, PairingClient, PairingHost, PairingTranscript,
    CHALLENGE_TTL_MS, MAX_PENDING_INVITES, MAX_TRANSPORT_PAYLOAD_BYTES, NOISE_PATTERN,
    PAIRING_PROTOCOL_VERSION,
};
pub use invite::{
    validate_relay_url, HostId, InviteId, InviteSecret, PairingInvite, RelayUrl,
    DEFAULT_INVITE_TTL_MS, INVITE_SECRET_BYTES, INVITE_VERSION, MAX_INVITE_TTL_MS,
    MAX_RELAY_URL_BYTES,
};
pub use keys::{
    DeviceKeyStore, InMemoryDeviceKeyStore, KeyStoreError, PublicKey, StaticKeypair,
    STATIC_KEY_BYTES,
};
pub use transport::RelayTransport;
pub use trust::{
    is_pairing_grantable, validate_grant_ttl, validate_pairing_scopes, DeviceTrust, PairedDevice,
    DEFAULT_GRANT_TTL_MS, MAX_GRANT_TTL_MS, PAIRING_GRANTABLE_SCOPES,
};
