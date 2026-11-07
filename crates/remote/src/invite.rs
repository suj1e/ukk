//! Version 1 QR invitation payload.
//!
//! The invitation is a short-lived bootstrap credential. It is single use and
//! expires in minutes; it never contains a Host private key or a permanent
//! bearer token. The invite secret is a 32-byte Noise PSK and is zeroized on
//! drop. `Debug`, `Display` and every error redact it.

use crate::encoding::{decode_exact, encode};
use crate::error::{PairingError, MAX_QR_BYTES};
use crate::keys::PublicKey;
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use ukk_protocol::DomainId;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

/// Current QR invite version.
pub const INVITE_VERSION: u8 = 1;
/// Default invitation lifetime: two minutes.
pub const DEFAULT_INVITE_TTL_MS: i64 = 2 * 60 * 1000;
/// Hard cap on an invitation lifetime: five minutes.
pub const MAX_INVITE_TTL_MS: i64 = 5 * 60 * 1000;
/// Maximum relay URL length inside an invite.
pub const MAX_RELAY_URL_BYTES: usize = 512;
/// Bytes of entropy in the one-time invite secret.
pub const INVITE_SECRET_BYTES: usize = 32;

macro_rules! opaque_pairing_id {
    ($name:ident, $label:literal) => {
        #[doc = concat!("Opaque pairing identifier: ", $label, ".")]
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(DomainId);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, PairingError> {
                DomainId::new(value).map(Self).map_err(PairingError::from)
            }

            /// Generates a fresh random identifier (122 bits from UUIDv4).
            #[must_use]
            pub fn new_random() -> Self {
                Self(DomainId::new_random())
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                self.0.as_str()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }

        impl TryFrom<String> for $name {
            type Error = PairingError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.0.into()
            }
        }
    };
}

opaque_pairing_id!(HostId, "stable Host identity used in the transcript");
opaque_pairing_id!(InviteId, "one-time invitation identity");

/// Relay endpoint restricted to `https` and `wss`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RelayUrl(url::Url);

impl RelayUrl {
    pub fn parse(raw: &str) -> Result<Self, PairingError> {
        if raw.len() > MAX_RELAY_URL_BYTES {
            return Err(PairingError::InvalidRelayUrl);
        }
        let url = url::Url::parse(raw).map_err(|_| PairingError::InvalidRelayUrl)?;
        if !matches!(url.scheme(), "https" | "wss") {
            return Err(PairingError::RelaySchemeNotAllowed);
        }
        if url.host_str().is_none() {
            return Err(PairingError::InvalidRelayUrl);
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(PairingError::RelayUserInfoForbidden);
        }
        if url.fragment().is_some() {
            return Err(PairingError::RelayFragmentForbidden);
        }
        if url.query().is_some() {
            return Err(PairingError::RelayQueryForbidden);
        }
        if url.port() == Some(0) {
            return Err(PairingError::InvalidRelayUrl);
        }
        Ok(Self(url))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    #[must_use]
    pub fn scheme(&self) -> &str {
        self.0.scheme()
    }
}

impl fmt::Display for RelayUrl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl TryFrom<String> for RelayUrl {
    type Error = PairingError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<RelayUrl> for String {
    fn from(value: RelayUrl) -> Self {
        value.0.into()
    }
}

/// A 32-byte one-time Noise PSK carried in the QR.
///
/// The value is intentionally not `Display`ed, `Debug`ged or included in
/// errors. It is zeroized when dropped or explicitly zeroized.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct InviteSecret([u8; INVITE_SECRET_BYTES]);

impl InviteSecret {
    pub fn generate() -> Result<Self, PairingError> {
        let mut secret = [0_u8; INVITE_SECRET_BYTES];
        getrandom::fill(&mut secret).map_err(|_| PairingError::RandomUnavailable)?;
        Ok(Self(secret))
    }

    #[cfg(test)]
    pub(crate) fn from_bytes(bytes: [u8; INVITE_SECRET_BYTES]) -> Self {
        Self(bytes)
    }

    pub(crate) fn as_bytes(&self) -> &[u8; INVITE_SECRET_BYTES] {
        &self.0
    }

    fn to_base64(&self) -> Zeroizing<String> {
        Zeroizing::new(encode(&self.0))
    }

    fn from_base64(value: &str) -> Result<Self, PairingError> {
        decode_exact::<INVITE_SECRET_BYTES>(value)
            .map(Self)
            .ok_or(PairingError::InvalidInviteSecret)
    }
}

impl fmt::Debug for InviteSecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("InviteSecret([redacted])")
    }
}

impl fmt::Display for InviteSecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[redacted]")
    }
}

impl Serialize for InviteSecret {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let encoded = self.to_base64();
        serializer.serialize_str(encoded.as_str())
    }
}

impl<'de> Deserialize<'de> for InviteSecret {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::from_base64(&value).map_err(|_| {
            D::Error::custom("invite secret must be 32 bytes of canonical unpadded base64url")
        })
    }
}

/// Version 1 QR payload.
///
/// The wire field names are `v`, `relay`, `host_id`, `host_public_key`,
/// `invite_id`, `invite_secret` and `expires_at_ms`. Unknown fields are
/// rejected. `expires_at_ms` is Unix epoch milliseconds.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairingInvite {
    #[serde(rename = "v")]
    version: u8,
    relay: RelayUrl,
    host_id: HostId,
    host_public_key: PublicKey,
    invite_id: InviteId,
    invite_secret: InviteSecret,
    expires_at_ms: i64,
}

impl PairingInvite {
    pub(crate) fn new(
        relay: RelayUrl,
        host_id: HostId,
        host_public_key: PublicKey,
        invite_id: InviteId,
        invite_secret: InviteSecret,
        expires_at_ms: i64,
    ) -> Result<Self, PairingError> {
        let invite = Self {
            version: INVITE_VERSION,
            relay,
            host_id,
            host_public_key,
            invite_id,
            invite_secret,
            expires_at_ms,
        };
        invite.validate()?;
        Ok(invite)
    }

    #[must_use]
    pub fn version(&self) -> u8 {
        self.version
    }

    #[must_use]
    pub fn relay(&self) -> &RelayUrl {
        &self.relay
    }

    #[must_use]
    pub fn host_id(&self) -> &HostId {
        &self.host_id
    }

    #[must_use]
    pub fn host_public_key(&self) -> PublicKey {
        self.host_public_key
    }

    #[must_use]
    pub fn invite_id(&self) -> &InviteId {
        &self.invite_id
    }

    #[must_use]
    pub fn expires_at_ms(&self) -> i64 {
        self.expires_at_ms
    }

    pub(crate) fn secret(&self) -> &InviteSecret {
        &self.invite_secret
    }

    /// Encodes the invite as the bounded QR payload.
    pub fn encode(&self) -> Result<String, PairingError> {
        self.validate()?;
        let encoded = serde_json::to_string(self).map_err(|_| PairingError::InviteMalformed)?;
        if encoded.len() > MAX_QR_BYTES {
            return Err(PairingError::InviteOversized { max: MAX_QR_BYTES });
        }
        Ok(encoded)
    }

    /// Parses and strictly validates a scanned QR payload.
    pub fn decode(input: &str) -> Result<Self, PairingError> {
        let trimmed = input.trim();
        if trimmed.len() > MAX_QR_BYTES {
            return Err(PairingError::InviteOversized { max: MAX_QR_BYTES });
        }
        let invite: Self =
            serde_json::from_str(trimmed).map_err(|_| PairingError::InviteMalformed)?;
        invite.validate()?;
        Ok(invite)
    }

    fn validate(&self) -> Result<(), PairingError> {
        if self.version != INVITE_VERSION {
            return Err(PairingError::UnsupportedInviteVersion {
                found: self.version,
            });
        }
        if self.expires_at_ms <= 0 {
            return Err(PairingError::InvalidTtl);
        }
        if self.host_public_key.is_zero() {
            return Err(PairingError::InvalidPublicKey);
        }
        // RelayUrl, HostId, InviteId, PublicKey and InviteSecret are already
        // validated by their deserializers/constructors.
        Ok(())
    }

    pub fn validate_at(&self, now_ms: i64) -> Result<(), PairingError> {
        if now_ms >= self.expires_at_ms {
            return Err(PairingError::InviteExpired);
        }
        Ok(())
    }

    #[must_use]
    pub fn is_expired_at(&self, now_ms: i64) -> bool {
        now_ms >= self.expires_at_ms
    }
}

impl fmt::Debug for PairingInvite {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PairingInvite")
            .field("version", &self.version)
            .field("relay", &self.relay)
            .field("host_id", &self.host_id)
            .field("invite_id", &self.invite_id)
            .field("expires_at_ms", &self.expires_at_ms)
            .field("invite_secret", &"[redacted]")
            .finish()
    }
}

impl fmt::Display for PairingInvite {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "PairingInvite(v{}, host {}, invite {}, relay {}, expires_at_ms {})",
            self.version, self.host_id, self.invite_id, self.relay, self.expires_at_ms
        )
    }
}

/// Typed validation failure for a relay URL, exposed for transport adapters.
pub fn validate_relay_url(raw: &str) -> Result<RelayUrl, PairingError> {
    RelayUrl::parse(raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroize::Zeroize;

    fn invite_at(expires_at_ms: i64) -> PairingInvite {
        PairingInvite::new(
            RelayUrl::parse("https://relay.example.invalid").unwrap(),
            HostId::new("host-1").unwrap(),
            PublicKey::from_bytes([1_u8; crate::keys::STATIC_KEY_BYTES]),
            InviteId::new("invite-1").unwrap(),
            InviteSecret::from_bytes([2_u8; INVITE_SECRET_BYTES]),
            expires_at_ms,
        )
        .unwrap()
    }

    #[test]
    fn round_trips_and_redacts_the_secret() {
        let invite = invite_at(10_000);
        let encoded = invite.encode().unwrap();
        let decoded = PairingInvite::decode(&encoded).unwrap();
        assert_eq!(decoded.invite_id(), invite.invite_id());
        assert_eq!(decoded.host_id(), invite.host_id());
        assert_eq!(decoded.expires_at_ms(), 10_000);
        assert_eq!(decoded.secret().as_bytes(), invite.secret().as_bytes());

        let secret = encode(&[2_u8; INVITE_SECRET_BYTES]);
        assert!(encoded.contains(&secret));
        for rendered in [format!("{invite:?}"), format!("{invite}")] {
            assert!(rendered.contains("redacted") || !rendered.contains(&secret));
            assert!(!rendered.contains(&secret));
        }
        assert!(!format!("{:?}", invite.secret()).contains(&secret));
        assert!(!format!("{}", invite.secret()).contains(&secret));
    }

    #[test]
    fn expiry_is_enforced() {
        let invite = invite_at(10_000);
        assert!(invite.validate_at(9_999).is_ok());
        assert_eq!(invite.validate_at(10_000), Err(PairingError::InviteExpired));
        assert!(invite.is_expired_at(10_000));
    }

    #[test]
    fn rejects_unknown_fields_bad_versions_and_bad_secrets() {
        let encoded = invite_at(10_000).encode().unwrap();
        let mut value: serde_json::Value = serde_json::from_str(&encoded).unwrap();
        value["extra"] = serde_json::json!(true);
        assert!(matches!(
            PairingInvite::decode(&value.to_string()),
            Err(PairingError::InviteMalformed)
        ));

        let mut value: serde_json::Value = serde_json::from_str(&encoded).unwrap();
        value["v"] = serde_json::json!(2);
        assert!(matches!(
            PairingInvite::decode(&value.to_string()),
            Err(PairingError::UnsupportedInviteVersion { found: 2 })
        ));

        let mut value: serde_json::Value = serde_json::from_str(&encoded).unwrap();
        value["invite_secret"] = serde_json::json!("AAAA");
        assert!(matches!(
            PairingInvite::decode(&value.to_string()),
            Err(PairingError::InviteMalformed)
        ));
    }

    #[test]
    fn rejects_oversized_input_and_noncanonical_secret() {
        let encoded = invite_at(10_000).encode().unwrap();
        let oversized = format!("{encoded}{}", "x".repeat(MAX_QR_BYTES));
        assert!(matches!(
            PairingInvite::decode(&oversized),
            Err(PairingError::InviteOversized { max: MAX_QR_BYTES })
        ));

        let mut value: serde_json::Value = serde_json::from_str(&encoded).unwrap();
        let secret = value["invite_secret"].as_str().unwrap();
        value["invite_secret"] = serde_json::json!(format!("{secret}="));
        assert!(matches!(
            PairingInvite::decode(&value.to_string()),
            Err(PairingError::InviteMalformed)
        ));
    }

    #[test]
    fn rejects_non_https_relays() {
        for raw in [
            "http://relay.example.invalid",
            "ws://relay.example.invalid",
            "file:///tmp/relay",
            "https://user:pw@relay.example.invalid",
            "https://relay.example.invalid/#frag",
            "https://relay.example.invalid/?token=leak",
            "https://relay.example.invalid:0",
            "not a url",
        ] {
            assert!(RelayUrl::parse(raw).is_err(), "{raw} should be rejected");
        }
        assert!(RelayUrl::parse("https://relay.example.invalid").is_ok());
        assert!(RelayUrl::parse("wss://relay.example.invalid/path").is_ok());
    }

    #[test]
    fn secret_zeroizes_on_demand() {
        let mut secret = InviteSecret::from_bytes([5_u8; INVITE_SECRET_BYTES]);
        secret.zeroize();
        assert_eq!(secret.as_bytes(), &[0_u8; INVITE_SECRET_BYTES]);
    }
}
