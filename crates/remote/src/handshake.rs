//! Noise-based pairing handshake and short-code confirmation.
//!
//! ## Construction
//!
//! * Pattern: [`NOISE_PATTERN`] (`Noise_IKpsk0_25519_ChaChaPoly_SHA256`), an
//!   established `snow` (Noise Protocol Framework) implementation provides the
//!   DH, AEAD and hashing; no cryptographic primitive is hand-rolled here.
//! * PSK: the 32-byte QR `invite_secret` is installed at PSK slot 0
//!   (`psk0`), so the PSK is mixed into the handshake before the first
//!   ephemeral and a wrong secret fails on the first message.
//! * Host static key: the initiator pins the Host's X25519 public key from the
//!   QR (`remote_public_key`), so a substituted Host key cannot complete the
//!   handshake.
//! * Device static key: the initiator's static key travels encrypted in the
//!   first `IK` message and is recovered by the Host with `get_remote_static`.
//! * Transcript: a canonical prologue binds protocol version, Host id, invite
//!   id, absolute expiry, relay URL and Host public key. Noise hashes the
//!   prologue and every handshake message into its handshake hash.
//! * Confirmation code: six decimal digits derived with SHA-256 over the final
//!   Noise handshake hash plus the one-time challenge. Both ends display the
//!   same code; the local owner compares it before approving scopes.
//!
//! ## Invitation lifecycle
//!
//! `create_invite` stores the server-side secret and expiry. `accept` consumes
//! the invitation before touching the Noise state, so a completed, failed or
//! in-flight invitation can never be replayed. A failed handshake burns the
//! invitation rather than reopening it for another guess.

use crate::error::{PairingError, MAX_PAIRING_FRAME_BYTES, MAX_PAIRING_REQUEST_BYTES};
use crate::invite::{
    HostId, InviteId, InviteSecret, PairingInvite, RelayUrl, INVITE_VERSION, MAX_INVITE_TTL_MS,
};
use crate::keys::{DeviceKeyStore, PublicKey, StaticKeypair};
use crate::trust::{is_pairing_grantable, validate_pairing_scopes, DeviceTrust, PairedDevice};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt;
use ukk_protocol::{DeviceId, IdentityGeneration, Scope, ScopeSet};

/// Noise handshake pattern. X25519 + ChaCha20-Poly1305 + SHA-256 with `psk0`.
pub const NOISE_PATTERN: &str = "Noise_IKpsk0_25519_ChaChaPoly_SHA256";

/// Pairing protocol version carried in request and challenge payloads.
pub const PAIRING_PROTOCOL_VERSION: u8 = INVITE_VERSION;

/// Challenge lifetime after the Host accepts the first handshake message.
pub const CHALLENGE_TTL_MS: i64 = 60 * 1000;

/// Maximum outstanding invitations held by one Host.
pub const MAX_PENDING_INVITES: usize = 32;

/// Maximum encrypted application payload (pre-tag) after the handshake.
pub const MAX_TRANSPORT_PAYLOAD_BYTES: usize = 2048;

const CONFIRMATION_MODULUS: u32 = 1_000_000;
const PROLOGUE_LABEL: &[u8] = b"ukk-pairing-prologue-v1\0";
const CONFIRMATION_LABEL: &[u8] = b"ukk-pairing-confirmation-v1\0";
const AEAD_TAG_BYTES: usize = 16;

fn write_field(output: &mut Vec<u8>, field: &[u8]) {
    output.extend_from_slice(&(field.len() as u32).to_be_bytes());
    output.extend_from_slice(field);
}

/// Canonical non-secret pairing transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingTranscript {
    version: u8,
    relay: RelayUrl,
    host_id: HostId,
    host_public_key: PublicKey,
    invite_id: InviteId,
    expires_at_ms: i64,
}

impl PairingTranscript {
    #[must_use]
    pub fn from_invite(invite: &PairingInvite) -> Self {
        Self {
            version: invite.version(),
            relay: invite.relay().clone(),
            host_id: invite.host_id().clone(),
            host_public_key: invite.host_public_key(),
            invite_id: invite.invite_id().clone(),
            expires_at_ms: invite.expires_at_ms(),
        }
    }

    /// Canonical prologue hashed by Noise. It never contains the invite secret.
    #[must_use]
    pub fn prologue(&self) -> Vec<u8> {
        let mut output = Vec::with_capacity(768);
        output.extend_from_slice(PROLOGUE_LABEL);
        output.push(self.version);
        write_field(&mut output, self.host_id.as_str().as_bytes());
        write_field(&mut output, self.invite_id.as_str().as_bytes());
        output.extend_from_slice(&self.expires_at_ms.to_be_bytes());
        write_field(&mut output, self.relay.as_str().as_bytes());
        write_field(&mut output, self.host_public_key.as_bytes());
        output
    }

    #[must_use]
    pub fn digest(&self) -> [u8; 32] {
        let digest = Sha256::digest(self.prologue());
        digest.into()
    }
}

/// One-time random challenge in the Host response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairingChallenge {
    pub nonce: [u8; 32],
    pub expires_at_ms: i64,
}

impl PairingChallenge {
    fn validate_for(&self, invite_expires_at_ms: i64, now_ms: i64) -> Result<(), PairingError> {
        if self.expires_at_ms <= 0
            || self.expires_at_ms > invite_expires_at_ms
            || now_ms >= self.expires_at_ms
        {
            return Err(PairingError::ChallengeInvalid);
        }
        Ok(())
    }
}

/// Six-digit human confirmation code derived from the final transcript.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ConfirmationCode(u32);

impl ConfirmationCode {
    #[must_use]
    pub fn from_handshake(handshake_hash: &[u8], challenge: &PairingChallenge) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(CONFIRMATION_LABEL);
        hasher.update(handshake_hash);
        hasher.update(challenge.nonce);
        hasher.update(challenge.expires_at_ms.to_be_bytes());
        let digest = hasher.finalize();
        let raw = u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]]);
        Self(raw % CONFIRMATION_MODULUS)
    }

    #[must_use]
    pub fn as_u32(self) -> u32 {
        self.0
    }

    #[must_use]
    pub fn digits(self) -> String {
        format!("{:06}", self.0)
    }
}

impl fmt::Display for ConfirmationCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:06}", self.0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PairingRequest {
    v: u8,
    invite_id: InviteId,
}

impl PairingRequest {
    fn encode(&self) -> Result<Vec<u8>, PairingError> {
        let encoded = serde_json::to_vec(self).map_err(|_| PairingError::InviteMalformed)?;
        if encoded.len() > MAX_PAIRING_REQUEST_BYTES {
            return Err(PairingError::FrameTooLarge {
                max: MAX_PAIRING_REQUEST_BYTES,
            });
        }
        Ok(encoded)
    }

    fn decode(frame: &[u8]) -> Result<Self, PairingError> {
        if frame.len() > MAX_PAIRING_REQUEST_BYTES {
            return Err(PairingError::FrameTooLarge {
                max: MAX_PAIRING_REQUEST_BYTES,
            });
        }
        let request: Self =
            serde_json::from_slice(frame).map_err(|_| PairingError::InviteMalformed)?;
        if request.v != PAIRING_PROTOCOL_VERSION {
            return Err(PairingError::UnsupportedInviteVersion { found: request.v });
        }
        Ok(request)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClientHelloPayload {
    v: u8,
    device_id: DeviceId,
    requested_scopes: ScopeSet,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PairingSuccessPayload {
    v: u8,
    device: PairedDevice,
}

#[derive(Clone, Debug)]
struct ServerInvite {
    transcript: PairingTranscript,
    secret: InviteSecret,
    expires_at_ms: i64,
}

#[derive(Clone, Debug)]
enum InviteState {
    Issued(Box<ServerInvite>),
    InProgress { expires_at_ms: i64 },
    Finished { expires_at_ms: i64 },
}

impl InviteState {
    fn expires_at_ms(&self) -> i64 {
        match self {
            Self::Issued(invite) => invite.expires_at_ms,
            Self::InProgress { expires_at_ms } | Self::Finished { expires_at_ms } => *expires_at_ms,
        }
    }
}

/// Outstanding invitations for one Host.
#[derive(Debug, Default)]
pub struct InviteRegistry {
    invites: BTreeMap<InviteId, InviteState>,
}

impl InviteRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn insert(&mut self, invite: ServerInvite, now_ms: i64) -> Result<(), PairingError> {
        self.purge_expired(now_ms);
        if self.invites.len() >= MAX_PENDING_INVITES {
            return Err(PairingError::TooManyInvites);
        }
        self.invites.insert(
            invite.transcript.invite_id.clone(),
            InviteState::Issued(Box::new(invite)),
        );
        Ok(())
    }

    /// Atomically marks an invite as in progress. A second attempt fails even
    /// if the first handshake later fails.
    fn consume(&mut self, invite_id: &InviteId, now_ms: i64) -> Result<ServerInvite, PairingError> {
        let (consumed, expires_at_ms, expired) = match self.invites.get(invite_id) {
            None => return Err(PairingError::UnknownInvite),
            Some(InviteState::Issued(invite)) => (
                Some((**invite).clone()),
                invite.expires_at_ms,
                now_ms >= invite.expires_at_ms,
            ),
            Some(InviteState::InProgress { .. } | InviteState::Finished { .. }) => {
                return Err(PairingError::InviteAlreadyUsed);
            }
        };
        if expired {
            self.invites
                .insert(invite_id.clone(), InviteState::Finished { expires_at_ms });
            return Err(PairingError::InviteExpired);
        }
        self.invites
            .insert(invite_id.clone(), InviteState::InProgress { expires_at_ms });
        consumed.ok_or(PairingError::UnknownInvite)
    }

    fn purge_expired(&mut self, now_ms: i64) {
        self.invites
            .retain(|_, state| now_ms < state.expires_at_ms());
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.invites.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.invites.is_empty()
    }
}

/// Host-side pairing authority.
pub struct PairingHost {
    host_id: HostId,
    host_key: StaticKeypair,
    invites: InviteRegistry,
    trust: DeviceTrust,
}

impl PairingHost {
    #[must_use]
    pub fn new(host_id: HostId, host_key: StaticKeypair) -> Self {
        Self {
            host_id,
            host_key,
            invites: InviteRegistry::new(),
            trust: DeviceTrust::new(),
        }
    }

    #[must_use]
    pub fn host_id(&self) -> &HostId {
        &self.host_id
    }

    #[must_use]
    pub fn host_public_key(&self) -> PublicKey {
        self.host_key.public_key()
    }

    #[must_use]
    pub fn trust(&self) -> &DeviceTrust {
        &self.trust
    }

    pub fn trust_mut(&mut self) -> &mut DeviceTrust {
        &mut self.trust
    }

    #[must_use]
    pub fn invites(&self) -> &InviteRegistry {
        &self.invites
    }

    /// Creates a single-use invitation. The maximum TTL is
    /// [`MAX_INVITE_TTL_MS`]; the UI default lives in
    /// [`crate::invite::DEFAULT_INVITE_TTL_MS`].
    pub fn create_invite(
        &mut self,
        relay: RelayUrl,
        now_ms: i64,
        ttl_ms: i64,
    ) -> Result<PairingInvite, PairingError> {
        if ttl_ms <= 0 || ttl_ms > MAX_INVITE_TTL_MS {
            return Err(PairingError::InvalidTtl);
        }
        let expires_at_ms = now_ms
            .checked_add(ttl_ms)
            .filter(|expires| *expires > now_ms)
            .ok_or(PairingError::InvalidTtl)?;
        let invite_id = InviteId::new_random();
        let secret = InviteSecret::generate()?;
        let transcript = PairingTranscript {
            version: PAIRING_PROTOCOL_VERSION,
            relay: relay.clone(),
            host_id: self.host_id.clone(),
            host_public_key: self.host_key.public_key(),
            invite_id: invite_id.clone(),
            expires_at_ms,
        };
        let invite = PairingInvite::new(
            relay,
            self.host_id.clone(),
            self.host_key.public_key(),
            invite_id,
            secret.clone(),
            expires_at_ms,
        )?;
        self.invites.insert(
            ServerInvite {
                transcript,
                secret,
                expires_at_ms,
            },
            now_ms,
        )?;
        Ok(invite)
    }

    /// Consumes the invitation and performs the first handshake step.
    pub fn accept(
        &mut self,
        request_frame: &[u8],
        hello_frame: &[u8],
        now_ms: i64,
    ) -> Result<HostPairingAttempt, PairingError> {
        let request = PairingRequest::decode(request_frame)?;
        if hello_frame.len() > MAX_PAIRING_FRAME_BYTES {
            return Err(PairingError::FrameTooLarge {
                max: MAX_PAIRING_FRAME_BYTES,
            });
        }
        let invite = self.invites.consume(&request.invite_id, now_ms)?;
        let transcript = invite.transcript.clone();
        let prologue_digest = transcript.digest();
        let prologue = transcript.prologue();
        let params: snow::params::NoiseParams = NOISE_PATTERN
            .parse()
            .map_err(|_| PairingError::NoiseConfig)?;
        let mut state = snow::Builder::new(params)
            .local_private_key(self.host_key.secret_bytes())
            .map_err(|_| PairingError::NoiseConfig)?
            .prologue(&prologue)
            .map_err(|_| PairingError::NoiseConfig)?
            .psk(0, invite.secret.as_bytes())
            .map_err(|_| PairingError::NoiseConfig)?
            .build_responder()
            .map_err(|_| PairingError::NoiseConfig)?;

        let mut hello_payload = vec![0_u8; MAX_PAIRING_FRAME_BYTES];
        let hello_len = state
            .read_message(hello_frame, &mut hello_payload)
            .map_err(|_| PairingError::HandshakeFailed)?;
        let device_public_bytes = state
            .get_remote_static()
            .ok_or(PairingError::HandshakeFailed)?;
        let device_public_key = PublicKey::from_bytes(
            device_public_bytes
                .try_into()
                .map_err(|_| PairingError::HandshakeFailed)?,
        );
        if device_public_key.is_zero() {
            return Err(PairingError::HandshakeFailed);
        }
        let hello: ClientHelloPayload = serde_json::from_slice(&hello_payload[..hello_len])
            .map_err(|_| PairingError::ClientHelloMalformed)?;
        if hello.v != PAIRING_PROTOCOL_VERSION {
            return Err(PairingError::ClientHelloMalformed);
        }
        for scope in hello.requested_scopes.iter() {
            if !is_pairing_grantable(scope) {
                return Err(PairingError::ScopeNotGrantable);
            }
        }

        let mut nonce = [0_u8; 32];
        getrandom::fill(&mut nonce).map_err(|_| PairingError::RandomUnavailable)?;
        let challenge_expires_at_ms = transcript
            .expires_at_ms
            .min(now_ms.saturating_add(CHALLENGE_TTL_MS));
        if challenge_expires_at_ms <= now_ms {
            return Err(PairingError::InviteExpired);
        }
        let challenge = PairingChallenge {
            nonce,
            expires_at_ms: challenge_expires_at_ms,
        };
        let challenge_payload =
            serde_json::to_vec(&challenge).map_err(|_| PairingError::HandshakeFailed)?;
        if challenge_payload.len() > MAX_TRANSPORT_PAYLOAD_BYTES {
            return Err(PairingError::FrameTooLarge {
                max: MAX_TRANSPORT_PAYLOAD_BYTES,
            });
        }
        let mut response = vec![0_u8; MAX_PAIRING_FRAME_BYTES];
        let response_len = state
            .write_message(&challenge_payload, &mut response)
            .map_err(|_| PairingError::HandshakeFailed)?;
        response.truncate(response_len);

        let handshake_hash: [u8; 32] = state
            .get_handshake_hash()
            .try_into()
            .map_err(|_| PairingError::HandshakeFailed)?;
        let confirmation_code = ConfirmationCode::from_handshake(&handshake_hash, &challenge);
        let transport = state
            .into_transport_mode()
            .map_err(|_| PairingError::HandshakeFailed)?;

        Ok(HostPairingAttempt {
            response,
            challenge,
            device_id: hello.device_id,
            device_public_key,
            requested_scopes: hello.requested_scopes,
            confirmation_code,
            handshake_hash,
            prologue_digest,
            transport: Some(transport),
            host_id: self.host_id.clone(),
            invite_expires_at_ms: transcript.expires_at_ms,
        })
    }

    /// Issues grants after the local owner confirms the code.
    ///
    /// Callers must compare [`HostPairingAttempt::confirmation_code`] with the
    /// code shown by the phone before invoking this method.
    pub fn approve(
        &mut self,
        attempt: HostPairingAttempt,
        scopes: &[Scope],
        grant_ttl_ms: i64,
        now_ms: i64,
    ) -> Result<ApprovedPairing, PairingError> {
        if now_ms >= attempt.invite_expires_at_ms {
            return Err(PairingError::InviteExpired);
        }
        let approved = validate_pairing_scopes(scopes)?;
        for scope in approved.iter() {
            if !attempt.requested_scopes.contains(scope) {
                return Err(PairingError::ScopeEscalation);
            }
        }
        let device = self.trust.approve(
            &self.host_id,
            attempt.device_id.clone(),
            attempt.device_public_key,
            scopes,
            grant_ttl_ms,
            now_ms,
        )?;
        match encrypt_success(attempt.transport, &device) {
            Ok(frame) => Ok(ApprovedPairing { frame, device }),
            Err(error) => {
                // Never leave grants behind if the success frame cannot be
                // produced; the phone would otherwise believe it is unpaired
                // while the Host retained authority.
                let _ = self.trust.revoke(&device.device_id);
                Err(error)
            }
        }
    }
}

fn encrypt_success(
    transport: Option<snow::TransportState>,
    device: &PairedDevice,
) -> Result<Vec<u8>, PairingError> {
    let payload = PairingSuccessPayload {
        v: PAIRING_PROTOCOL_VERSION,
        device: device.clone(),
    };
    let encoded =
        serde_json::to_vec(&payload).map_err(|_| PairingError::PairingSuccessMalformed)?;
    if encoded.len() > MAX_TRANSPORT_PAYLOAD_BYTES {
        return Err(PairingError::FrameTooLarge {
            max: MAX_TRANSPORT_PAYLOAD_BYTES,
        });
    }
    let mut transport = transport.ok_or(PairingError::HandshakeFailed)?;
    let mut frame = vec![0_u8; encoded.len() + AEAD_TAG_BYTES];
    let frame_len = transport
        .write_message(&encoded, &mut frame)
        .map_err(|_| PairingError::HandshakeFailed)?;
    frame.truncate(frame_len);
    Ok(frame)
}

/// Host-side pending pairing after the first handshake message.
pub struct HostPairingAttempt {
    response: Vec<u8>,
    challenge: PairingChallenge,
    device_id: DeviceId,
    device_public_key: PublicKey,
    requested_scopes: ScopeSet,
    confirmation_code: ConfirmationCode,
    handshake_hash: [u8; 32],
    prologue_digest: [u8; 32],
    transport: Option<snow::TransportState>,
    host_id: HostId,
    invite_expires_at_ms: i64,
}

impl HostPairingAttempt {
    #[must_use]
    pub fn response_frame(&self) -> &[u8] {
        &self.response
    }

    #[must_use]
    pub fn challenge(&self) -> &PairingChallenge {
        &self.challenge
    }

    #[must_use]
    pub fn device_id(&self) -> &DeviceId {
        &self.device_id
    }

    #[must_use]
    pub fn device_public_key(&self) -> PublicKey {
        self.device_public_key
    }

    #[must_use]
    pub fn requested_scopes(&self) -> &ScopeSet {
        &self.requested_scopes
    }

    #[must_use]
    pub fn confirmation_code(&self) -> ConfirmationCode {
        self.confirmation_code
    }

    /// Final Noise handshake hash. Includes the prologue and every handshake
    /// message; useful for tests and diagnostics, never a secret.
    #[must_use]
    pub fn handshake_hash(&self) -> &[u8; 32] {
        &self.handshake_hash
    }

    /// SHA-256 of the canonical prologue.
    #[must_use]
    pub fn prologue_digest(&self) -> &[u8; 32] {
        &self.prologue_digest
    }

    #[must_use]
    pub fn host_id(&self) -> &HostId {
        &self.host_id
    }
}

impl fmt::Debug for HostPairingAttempt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HostPairingAttempt")
            .field("host_id", &self.host_id)
            .field("device_id", &self.device_id)
            .field("device_public_key", &self.device_public_key)
            .field("requested_scopes", &self.requested_scopes)
            .field("confirmation_code", &self.confirmation_code)
            .field("invite_expires_at_ms", &self.invite_expires_at_ms)
            .finish_non_exhaustive()
    }
}

/// Encrypted success frame plus the grants issued by the Host.
#[derive(Debug)]
pub struct ApprovedPairing {
    frame: Vec<u8>,
    device: PairedDevice,
}

impl ApprovedPairing {
    /// Encrypted frame to send to the phone after confirmation.
    #[must_use]
    pub fn frame(&self) -> &[u8] {
        &self.frame
    }

    #[must_use]
    pub fn device(&self) -> &PairedDevice {
        &self.device
    }

    #[must_use]
    pub fn into_device(self) -> PairedDevice {
        self.device
    }
}

/// Phone-side pairing client.
pub struct PairingClient {
    invite: PairingInvite,
    store: Box<dyn DeviceKeyStore>,
    device_id: DeviceId,
    device_key: StaticKeypair,
    requested_scopes: ScopeSet,
}

impl PairingClient {
    /// Creates a client for a new device. The device identity and static key
    /// are generated; the key is persisted through `store`.
    pub fn new(
        invite: PairingInvite,
        store: Box<dyn DeviceKeyStore>,
        requested_scopes: &[Scope],
    ) -> Result<Self, PairingError> {
        Self::with_device_id(invite, store, DeviceId::new_random(), requested_scopes)
    }

    /// Uses an existing device id, loading or creating its static key.
    pub fn with_device_id(
        invite: PairingInvite,
        store: Box<dyn DeviceKeyStore>,
        device_id: DeviceId,
        requested_scopes: &[Scope],
    ) -> Result<Self, PairingError> {
        let requested_scopes = validate_pairing_scopes(requested_scopes)?;
        let device_key = match store.load(&device_id)? {
            Some(key) => key,
            None => {
                let key = StaticKeypair::generate()?;
                store.store(&device_id, &key)?;
                key
            }
        };
        Ok(Self {
            invite,
            store,
            device_id,
            device_key,
            requested_scopes,
        })
    }

    #[must_use]
    pub fn device_id(&self) -> &DeviceId {
        &self.device_id
    }

    #[must_use]
    pub fn device_public_key(&self) -> PublicKey {
        self.device_key.public_key()
    }

    #[must_use]
    pub fn requested_scopes(&self) -> &ScopeSet {
        &self.requested_scopes
    }

    #[must_use]
    pub fn invite(&self) -> &PairingInvite {
        &self.invite
    }

    #[must_use]
    pub fn key_store(&self) -> &dyn DeviceKeyStore {
        self.store.as_ref()
    }

    /// Frame the client sends first so the Host can select the invitation.
    pub fn request_frame(&self) -> Result<Vec<u8>, PairingError> {
        PairingRequest {
            v: PAIRING_PROTOCOL_VERSION,
            invite_id: self.invite.invite_id().clone(),
        }
        .encode()
    }

    /// Writes the first Noise message. Fails before any network exchange if the
    /// invitation has already expired locally.
    pub fn start(&self, now_ms: i64) -> Result<(Vec<u8>, ClientHandshake), PairingError> {
        self.invite.validate_at(now_ms)?;
        let transcript = PairingTranscript::from_invite(&self.invite);
        let prologue = transcript.prologue();
        let prologue_digest = transcript.digest();
        let params: snow::params::NoiseParams = NOISE_PATTERN
            .parse()
            .map_err(|_| PairingError::NoiseConfig)?;
        let mut state = snow::Builder::new(params)
            .local_private_key(self.device_key.secret_bytes())
            .map_err(|_| PairingError::NoiseConfig)?
            .remote_public_key(self.invite.host_public_key().as_bytes())
            .map_err(|_| PairingError::NoiseConfig)?
            .prologue(&prologue)
            .map_err(|_| PairingError::NoiseConfig)?
            .psk(0, self.invite.secret().as_bytes())
            .map_err(|_| PairingError::NoiseConfig)?
            .build_initiator()
            .map_err(|_| PairingError::NoiseConfig)?;

        let hello = ClientHelloPayload {
            v: PAIRING_PROTOCOL_VERSION,
            device_id: self.device_id.clone(),
            requested_scopes: self.requested_scopes.clone(),
        };
        let hello_payload =
            serde_json::to_vec(&hello).map_err(|_| PairingError::ClientHelloMalformed)?;
        if hello_payload.len() > MAX_TRANSPORT_PAYLOAD_BYTES {
            return Err(PairingError::FrameTooLarge {
                max: MAX_TRANSPORT_PAYLOAD_BYTES,
            });
        }
        let mut message = vec![0_u8; MAX_PAIRING_FRAME_BYTES];
        let message_len = state
            .write_message(&hello_payload, &mut message)
            .map_err(|_| PairingError::HandshakeFailed)?;
        message.truncate(message_len);
        Ok((
            message,
            ClientHandshake {
                state,
                invite: self.invite.clone(),
                device_id: self.device_id.clone(),
                device_public_key: self.device_key.public_key(),
                requested_scopes: self.requested_scopes.clone(),
                prologue_digest,
            },
        ))
    }
}

impl fmt::Debug for PairingClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PairingClient")
            .field("invite", &self.invite)
            .field("device_id", &self.device_id)
            .field("device_public_key", &self.device_key.public_key())
            .field("requested_scopes", &self.requested_scopes)
            .finish_non_exhaustive()
    }
}

/// Phone-side first handshake message plus pinned Host identity.
pub struct ClientHandshake {
    state: snow::HandshakeState,
    invite: PairingInvite,
    device_id: DeviceId,
    device_public_key: PublicKey,
    requested_scopes: ScopeSet,
    prologue_digest: [u8; 32],
}

impl ClientHandshake {
    /// Consumes the Host response, verifies the pinned Host key, validates the
    /// challenge and derives the same confirmation code.
    pub fn finish(
        mut self,
        response_frame: &[u8],
        now_ms: i64,
    ) -> Result<ClientPairingPending, PairingError> {
        if response_frame.len() > MAX_PAIRING_FRAME_BYTES {
            return Err(PairingError::FrameTooLarge {
                max: MAX_PAIRING_FRAME_BYTES,
            });
        }
        let mut payload = vec![0_u8; MAX_PAIRING_FRAME_BYTES];
        let payload_len = self
            .state
            .read_message(response_frame, &mut payload)
            .map_err(|_| PairingError::HandshakeFailed)?;
        let remote_static = self
            .state
            .get_remote_static()
            .ok_or(PairingError::HandshakeFailed)?;
        if remote_static != self.invite.host_public_key().as_bytes() {
            return Err(PairingError::HostKeyMismatch);
        }
        let challenge: PairingChallenge = serde_json::from_slice(&payload[..payload_len])
            .map_err(|_| PairingError::ChallengeInvalid)?;
        challenge.validate_for(self.invite.expires_at_ms(), now_ms)?;
        let handshake_hash: [u8; 32] = self
            .state
            .get_handshake_hash()
            .try_into()
            .map_err(|_| PairingError::HandshakeFailed)?;
        let confirmation_code = ConfirmationCode::from_handshake(&handshake_hash, &challenge);
        let transport = self
            .state
            .into_transport_mode()
            .map_err(|_| PairingError::HandshakeFailed)?;
        Ok(ClientPairingPending {
            transport: Some(transport),
            invite: self.invite,
            device_id: self.device_id,
            device_public_key: self.device_public_key,
            requested_scopes: self.requested_scopes,
            challenge,
            confirmation_code,
            handshake_hash,
            prologue_digest: self.prologue_digest,
        })
    }
}

impl fmt::Debug for ClientHandshake {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClientHandshake")
            .field("invite", &self.invite)
            .field("device_id", &self.device_id)
            .finish_non_exhaustive()
    }
}

/// Phone-side state after a successful handshake, before grant approval.
pub struct ClientPairingPending {
    transport: Option<snow::TransportState>,
    invite: PairingInvite,
    device_id: DeviceId,
    device_public_key: PublicKey,
    requested_scopes: ScopeSet,
    challenge: PairingChallenge,
    confirmation_code: ConfirmationCode,
    handshake_hash: [u8; 32],
    prologue_digest: [u8; 32],
}

impl ClientPairingPending {
    #[must_use]
    pub fn confirmation_code(&self) -> ConfirmationCode {
        self.confirmation_code
    }

    #[must_use]
    pub fn challenge(&self) -> &PairingChallenge {
        &self.challenge
    }

    #[must_use]
    pub fn handshake_hash(&self) -> &[u8; 32] {
        &self.handshake_hash
    }

    #[must_use]
    pub fn prologue_digest(&self) -> &[u8; 32] {
        &self.prologue_digest
    }

    /// Decrypts and verifies the Host's grant payload. Rejects a payload that
    /// names another device or Host, carries an unrequested scope, grants
    /// `device.manage`, or is already expired.
    pub fn await_approval(
        mut self,
        frame: &[u8],
        now_ms: i64,
    ) -> Result<PairedDevice, PairingError> {
        if frame.len() > MAX_PAIRING_FRAME_BYTES {
            return Err(PairingError::FrameTooLarge {
                max: MAX_PAIRING_FRAME_BYTES,
            });
        }
        let mut payload = vec![0_u8; MAX_PAIRING_FRAME_BYTES];
        let mut transport = self.transport.take().ok_or(PairingError::HandshakeFailed)?;
        let payload_len = transport
            .read_message(frame, &mut payload)
            .map_err(|_| PairingError::HandshakeFailed)?;
        let success: PairingSuccessPayload = serde_json::from_slice(&payload[..payload_len])
            .map_err(|_| PairingError::PairingSuccessMalformed)?;
        if success.v != PAIRING_PROTOCOL_VERSION {
            return Err(PairingError::PairingSuccessMalformed);
        }
        let device = success.device;
        if device.device_id != self.device_id {
            return Err(PairingError::DeviceIdMismatch);
        }
        if device.host_id != *self.invite.host_id() {
            return Err(PairingError::HostMismatch);
        }
        if device.public_key != self.device_public_key {
            return Err(PairingError::KeyFingerprintMismatch);
        }
        if device.fingerprint != self.device_public_key.fingerprint()? {
            return Err(PairingError::KeyFingerprintMismatch);
        }
        if device.generation < IdentityGeneration::FIRST {
            return Err(PairingError::PairingSuccessMalformed);
        }
        for scope in device.scopes.iter() {
            if !is_pairing_grantable(scope) {
                return Err(PairingError::ScopeNotGrantable);
            }
            if !self.requested_scopes.contains(scope) {
                return Err(PairingError::ScopeEscalation);
            }
        }
        if device.is_grant_expired_at(now_ms) {
            return Err(PairingError::GrantExpired);
        }
        Ok(device)
    }
}

impl fmt::Debug for ClientPairingPending {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClientPairingPending")
            .field("invite", &self.invite)
            .field("device_id", &self.device_id)
            .field("requested_scopes", &self.requested_scopes)
            .field("challenge", &self.challenge)
            .field("confirmation_code", &self.confirmation_code)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::invite::DEFAULT_INVITE_TTL_MS;
    use crate::keys::InMemoryDeviceKeyStore;
    use ukk_protocol::Scope;

    const NOW: i64 = 1_000_000;

    fn scope_set(scopes: &[Scope]) -> ScopeSet {
        scopes.iter().copied().collect()
    }

    fn setup(requested: &[Scope]) -> (HostPairingAttempt, ClientPairingPending) {
        let mut host = PairingHost::new(
            HostId::new("host-1").unwrap(),
            StaticKeypair::generate().unwrap(),
        );
        let invite = host
            .create_invite(
                RelayUrl::parse("https://relay.example.invalid").unwrap(),
                NOW,
                DEFAULT_INVITE_TTL_MS,
            )
            .unwrap();
        let client =
            PairingClient::new(invite, Box::new(InMemoryDeviceKeyStore::new()), requested).unwrap();
        let request = client.request_frame().unwrap();
        let (hello, handshake) = client.start(NOW + 10).unwrap();
        let attempt = host.accept(&request, &hello, NOW + 20).unwrap();
        let pending = handshake
            .finish(attempt.response_frame(), NOW + 30)
            .unwrap();
        (attempt, pending)
    }

    fn device(
        pending: &ClientPairingPending,
        host_id: HostId,
        scopes: ScopeSet,
        grant_expires_at_ms: i64,
    ) -> PairedDevice {
        PairedDevice {
            host_id,
            device_id: pending.device_id.clone(),
            public_key: pending.device_public_key,
            fingerprint: pending.device_public_key.fingerprint().unwrap(),
            generation: IdentityGeneration::FIRST,
            scopes,
            grant_expires_at_ms,
            paired_at_ms: NOW + 30,
        }
    }

    fn encrypt(attempt: &mut HostPairingAttempt, device: PairedDevice) -> Vec<u8> {
        let payload = PairingSuccessPayload {
            v: PAIRING_PROTOCOL_VERSION,
            device,
        };
        let encoded = serde_json::to_vec(&payload).unwrap();
        let mut transport = attempt.transport.take().unwrap();
        let mut frame = vec![0_u8; encoded.len() + AEAD_TAG_BYTES];
        let len = transport.write_message(&encoded, &mut frame).unwrap();
        frame.truncate(len);
        frame
    }

    #[test]
    fn client_rejects_success_naming_another_host() {
        let (mut attempt, pending) = setup(&[Scope::SessionObserve]);
        let frame = encrypt(
            &mut attempt,
            device(
                &pending,
                HostId::new("evil-host").unwrap(),
                pending.requested_scopes.clone(),
                NOW + 60_000,
            ),
        );
        assert_eq!(
            pending.await_approval(&frame, NOW + 40),
            Err(PairingError::HostMismatch)
        );
    }

    #[test]
    fn client_rejects_unrequested_or_forbidden_scopes() {
        let (mut attempt, pending) = setup(&[Scope::SessionObserve]);
        let frame = encrypt(
            &mut attempt,
            device(
                &pending,
                pending.invite.host_id().clone(),
                scope_set(&[Scope::SessionObserve, Scope::AgentSend]),
                NOW + 60_000,
            ),
        );
        assert_eq!(
            pending.await_approval(&frame, NOW + 40),
            Err(PairingError::ScopeEscalation)
        );

        let (mut attempt, pending) = setup(&[Scope::SessionObserve]);
        let frame = encrypt(
            &mut attempt,
            device(
                &pending,
                pending.invite.host_id().clone(),
                scope_set(&[Scope::DeviceManage]),
                NOW + 60_000,
            ),
        );
        assert_eq!(
            pending.await_approval(&frame, NOW + 40),
            Err(PairingError::ScopeNotGrantable)
        );
    }

    #[test]
    fn client_rejects_expired_success_grant() {
        let (mut attempt, pending) = setup(&[Scope::SessionObserve]);
        let frame = encrypt(
            &mut attempt,
            device(
                &pending,
                pending.invite.host_id().clone(),
                pending.requested_scopes.clone(),
                NOW + 40,
            ),
        );
        assert_eq!(
            pending.await_approval(&frame, NOW + 40),
            Err(PairingError::GrantExpired)
        );
    }
}
