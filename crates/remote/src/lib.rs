//! Pairing, device trust and the relay boundary for ukk remote clients.
//!
//! This crate is the S2 core, not a networking or UI implementation. It owns:
//!
//! * [`invite`] — the versioned QR payload, strict validation and secret
//!   redaction/zeroization.
//! * [`handshake`] — a Noise `IKpsk0` pairing handshake with the QR secret as
//!   PSK, the Host static key pinned from the QR, transcript binding, a
//!   one-time challenge, a constant-time typed confirmation gate
//!   (`ConfirmedAttempt`) and a pending-until-ack completion step.
//! * [`trust`] — scoped device grants layered on `ukk-protocol`'s live trust
//!   registry, including rotation, revocation and grant expiry.
//! * [`keys`] — static X25519 keys and the [`keys::DeviceKeyStore`] trait with
//!   an in-memory test implementation. OS Keychain/Keyring is out of scope.
//! * [`relay`] — a versioned ukk-internal signalling envelope and the
//!   [`relay::RelayDriver`] state machine for outbound register/connect/route.
//!   It is explicitly not the alma-relay wire protocol.
//! * [`transport`] — the outbound [`transport::RelayConnection`] seam and the
//!   documented outbound WSS assumptions. This crate performs no network I/O.
//!
//! ## What is proven by tests
//!
//! Invite parsing and redaction/expiry/replay, wrong PSK, Host key
//! substitution, transcript/Host mismatch, low-order key rejection,
//! confirmation-code agreement and wrong-code rejection, pending-until-ack
//! commit, scope-escalation denial, device revocation/key rotation and bounded
//! malformed input all have contract tests. A deterministic Noise vector is
//! included. The relay boundary has an in-memory mock-relay contract test for
//! outbound registration, routing bounded pairing frames, duplicate
//! registration, unknown routes, oversized frames, reconnect and revocation.
//!
//! ## What is NOT VERIFIED
//!
//! **MAJOR GATE:** pairing authenticates the device only during the Noise
//! exchange. The S1 WAN `DeviceClaim` is still client-supplied with no
//! proof-of-possession; do not expose a relay listener until host/relay wiring
//! binds a fresh session to the paired device static key. No real relay was
//! contacted, no OS key store was used, no physical iPhone ran this code and
//! there has been no external cryptographic or security review. The
//! ukk-internal signalling envelope is **not** alma-relay-compatible; see
//! [docs/PAIRING.md](../../../docs/PAIRING.md) for the gap record. Do not
//! describe this as production-ready cryptography.

pub mod error;
pub mod handshake;
pub mod invite;
pub mod keys;
pub mod relay;
pub mod transport;
pub mod trust;

mod encoding;

pub use error::{PairingError, MAX_PAIRING_FRAME_BYTES, MAX_PAIRING_REQUEST_BYTES, MAX_QR_BYTES};
pub use handshake::{
    ClientHandshake, ClientPairingAccepted, ClientPairingPending, ConfirmationCode,
    ConfirmedAttempt, HostPairingAttempt, InviteRegistry, PairingChallenge, PairingClient,
    PairingHost, PairingTranscript, PendingPairing, ACK_TTL_MS, CHALLENGE_TTL_MS,
    MAX_PENDING_INVITES, MAX_TRANSPORT_PAYLOAD_BYTES, NOISE_PATTERN, PAIRING_PROTOCOL_VERSION,
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
pub use relay::{
    CloseReason, OpaqueFrame, RelayDriver, RelayError, RelayErrorCode, RelayEvent, RelayMessage,
    RelayRole, RelayState, SessionId, MAX_SESSION_ID_BYTES, MAX_SIGNALLING_FRAME_BYTES,
    MAX_SIGNALLING_PAYLOAD_BYTES, MAX_SIGNALLING_PAYLOAD_ENCODED_BYTES, MIN_SESSION_ID_BYTES,
    SIGNALLING_VERSION,
};
pub use transport::RelayConnection;
pub use trust::{
    is_pairing_grantable, validate_grant_ttl, validate_pairing_scopes, DeviceTrust, PairedDevice,
    DEFAULT_GRANT_TTL_MS, MAX_GRANT_TTL_MS, PAIRING_GRANTABLE_SCOPES,
};
