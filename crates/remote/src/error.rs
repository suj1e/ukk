//! Typed pairing errors.
//!
//! No variant carries an invite secret, a private key or a raw peer payload.
//! Parsing errors deliberately discard the offending JSON text so a malformed
//! QR cannot leak its bootstrap credential into logs or diagnostics.

use std::error::Error;
use std::fmt;

/// Maximum encoded QR payload length accepted by [`crate::PairingInvite::decode`].
pub const MAX_QR_BYTES: usize = 2048;

/// Maximum encoded pairing request length.
pub const MAX_PAIRING_REQUEST_BYTES: usize = 512;

/// Maximum message body accepted by the pairing handshake and transport.
pub const MAX_PAIRING_FRAME_BYTES: usize = 4096;

/// Failure of a pairing, trust or key-store operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairingError {
    /// Encoded invite exceeds [`MAX_QR_BYTES`].
    InviteOversized { max: usize },
    /// The invite is not valid JSON or contains an unknown/missing field.
    InviteMalformed,
    /// The invite version is not supported by this pairing generation.
    UnsupportedInviteVersion { found: u8 },
    /// The relay URL could not be parsed.
    InvalidRelayUrl,
    /// The relay URL is neither `https` nor `wss`.
    RelaySchemeNotAllowed,
    /// The relay URL contains a username or password.
    RelayUserInfoForbidden,
    /// The relay URL contains a fragment.
    RelayFragmentForbidden,
    /// The relay URL contains a query.
    RelayQueryForbidden,
    /// An opaque identifier is malformed.
    InvalidId,
    /// A static public key is not 32 bytes of canonical base64url.
    InvalidPublicKey,
    /// The invite secret is not 32 bytes of canonical base64url.
    InvalidInviteSecret,
    /// The invite lifetime was zero, negative or longer than the maximum.
    InvalidTtl,
    /// The invite has expired.
    InviteExpired,
    /// No invitation with this id is outstanding.
    UnknownInvite,
    /// The invitation was already started, completed or failed.
    InviteAlreadyUsed,
    /// Too many outstanding invitations are held by this Host.
    TooManyInvites,
    /// A Noise handshake step failed: wrong PSK, tampered message, wrong key
    /// or a transcript mismatch all fail closed with this coarse error.
    HandshakeFailed,
    /// The Snow configuration or resolver could not be built.
    NoiseConfig,
    /// The operating system random source failed.
    RandomUnavailable,
    /// A client handshake payload was malformed or had an unsupported version.
    ClientHelloMalformed,
    /// The decrypted device id does not match the expected device.
    DeviceIdMismatch,
    /// The decrypted device public key or fingerprint does not match.
    KeyFingerprintMismatch,
    /// The Host key recovered from Noise does not match the key pinned in the QR.
    HostKeyMismatch,
    /// The challenge is malformed, expired or extends past the invite lifetime.
    ChallengeInvalid,
    /// The decrypted pairing success names another Host.
    HostMismatch,
    /// The requested grant set was not a subset of what the device requested.
    ScopeEscalation,
    /// A scope cannot be granted during pairing.
    ScopeNotGrantable,
    /// A grant has expired.
    GrantExpired,
    /// The device id is already registered with another key.
    DeviceAlreadyRegistered,
    /// A rotation supplied the already-registered public key.
    SameKey,
    /// The device is not registered.
    UnknownDevice,
    /// The device was revoked.
    DeviceRevoked,
    /// A rotation or revocation made the presented identity stale.
    StaleIdentity,
    /// The identity generation counter overflowed.
    GenerationOverflow,
    /// Stored key material is corrupt or has an invalid length.
    KeyStoreCorrupt,
    /// The device key store is unavailable.
    KeyStoreUnavailable,
    /// A pairing frame is larger than the accepted bound.
    FrameTooLarge { max: usize },
    /// The encrypted pairing success payload was malformed.
    PairingSuccessMalformed,
}

impl fmt::Display for PairingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InviteOversized { max } => {
                write!(formatter, "pairing invite exceeds {max} bytes")
            }
            Self::InviteMalformed => formatter.write_str("pairing invite is malformed"),
            Self::UnsupportedInviteVersion { found } => {
                write!(formatter, "pairing invite version {found} is not supported")
            }
            Self::InvalidRelayUrl => formatter.write_str("relay URL is invalid"),
            Self::RelaySchemeNotAllowed => formatter.write_str("relay URL must use https or wss"),
            Self::RelayUserInfoForbidden => {
                formatter.write_str("relay URL must not contain user information")
            }
            Self::RelayFragmentForbidden => {
                formatter.write_str("relay URL must not contain a fragment")
            }
            Self::RelayQueryForbidden => formatter.write_str("relay URL must not contain a query"),
            Self::InvalidId => formatter.write_str("pairing identifier is malformed"),
            Self::InvalidPublicKey => formatter.write_str("static public key is malformed"),
            Self::InvalidInviteSecret => formatter.write_str("invite secret is malformed"),
            Self::InvalidTtl => formatter.write_str("pairing or grant lifetime is out of range"),
            Self::InviteExpired => formatter.write_str("pairing invite has expired"),
            Self::UnknownInvite => formatter.write_str("pairing invite is not outstanding"),
            Self::InviteAlreadyUsed => formatter.write_str("pairing invite was already used"),
            Self::TooManyInvites => formatter.write_str("too many outstanding pairing invites"),
            Self::HandshakeFailed => formatter.write_str("pairing handshake failed"),
            Self::NoiseConfig => formatter.write_str("pairing handshake configuration failed"),
            Self::RandomUnavailable => {
                formatter.write_str("operating system random source is unavailable")
            }
            Self::ClientHelloMalformed => formatter.write_str("client pairing hello is malformed"),
            Self::DeviceIdMismatch => formatter.write_str("device identity does not match"),
            Self::KeyFingerprintMismatch => {
                formatter.write_str("device key fingerprint does not match")
            }
            Self::HostKeyMismatch => {
                formatter.write_str("Host static key does not match the invite")
            }
            Self::ChallengeInvalid => formatter.write_str("pairing challenge is invalid"),
            Self::HostMismatch => formatter.write_str("pairing success names another Host"),
            Self::ScopeEscalation => {
                formatter.write_str("approved scopes exceed the requested scopes")
            }
            Self::ScopeNotGrantable => {
                formatter.write_str("scope cannot be granted during pairing")
            }
            Self::GrantExpired => formatter.write_str("device grant has expired"),
            Self::DeviceAlreadyRegistered => formatter.write_str("device is already registered"),
            Self::SameKey => formatter.write_str("rotated key equals the registered key"),
            Self::UnknownDevice => formatter.write_str("device is not registered"),
            Self::DeviceRevoked => formatter.write_str("device is revoked"),
            Self::StaleIdentity => formatter.write_str("device identity is stale"),
            Self::GenerationOverflow => formatter.write_str("identity generation overflow"),
            Self::KeyStoreCorrupt => formatter.write_str("stored device key is corrupt"),
            Self::KeyStoreUnavailable => formatter.write_str("device key store is unavailable"),
            Self::FrameTooLarge { max } => write!(formatter, "pairing frame exceeds {max} bytes"),
            Self::PairingSuccessMalformed => {
                formatter.write_str("pairing success payload is malformed")
            }
        }
    }
}

impl Error for PairingError {}

impl From<ukk_protocol::IdError> for PairingError {
    fn from(_: ukk_protocol::IdError) -> Self {
        Self::InvalidId
    }
}
