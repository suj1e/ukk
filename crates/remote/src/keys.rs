//! Static X25519 identity keys and device key storage.
//!
//! The Host static key is pinned by the QR invitation. The device static key
//! is generated on the phone and persisted through [`DeviceKeyStore`]. This
//! crate only ships an in-memory test store; an OS Keychain/Keyring adapter is
//! deliberately a separate slice and must store the secret in the platform
//! protected store, never as plaintext in SQLite.

use crate::encoding::{decode_exact, encode};
use crate::error::PairingError;
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::sync::Mutex;
use ukk_protocol::{DeviceId, KeyFingerprint};
use x25519_dalek::{PublicKey as X25519PublicKey, StaticSecret};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

/// Number of bytes in an X25519 static key.
pub const STATIC_KEY_BYTES: usize = 32;

/// A validated X25519 public key, encoded as canonical unpadded base64url.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PublicKey([u8; STATIC_KEY_BYTES]);

impl PublicKey {
    #[must_use]
    pub fn from_bytes(bytes: [u8; STATIC_KEY_BYTES]) -> Self {
        Self(bytes)
    }

    /// Parses exactly 32 bytes of canonical unpadded base64url.
    #[must_use]
    pub fn from_base64(value: &str) -> Option<Self> {
        decode_exact(value).map(Self)
    }

    #[must_use]
    pub fn to_base64(self) -> String {
        encode(&self.0)
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8; STATIC_KEY_BYTES] {
        &self.0
    }

    /// The all-zero encoding is the identity/low-order X25519 input and is
    /// never produced by this crate's generator.
    #[must_use]
    pub fn is_zero(&self) -> bool {
        self.0 == [0_u8; STATIC_KEY_BYTES]
    }

    /// SHA-256 fingerprint in the protocol wire form `sha256:<hex>`.
    pub fn fingerprint(&self) -> Result<KeyFingerprint, PairingError> {
        let mut hasher = Sha256::new();
        hasher.update(b"ukk-pairing-key-v1\0");
        hasher.update(self.0);
        let digest = hasher.finalize();
        let mut hex = String::with_capacity(64);
        for byte in digest {
            use fmt::Write as _;
            write!(&mut hex, "{byte:02x}").map_err(|_| PairingError::InvalidId)?;
        }
        KeyFingerprint::new(format!("sha256:{hex}")).map_err(PairingError::from)
    }
}

impl fmt::Debug for PublicKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("PublicKey")
            .field(&self.to_base64())
            .finish()
    }
}

impl fmt::Display for PublicKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.to_base64())
    }
}

impl Serialize for PublicKey {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_base64())
    }
}

impl<'de> Deserialize<'de> for PublicKey {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::from_base64(&value)
            .ok_or_else(|| D::Error::custom("static public key must be 32 bytes of base64url"))
    }
}

/// An X25519 static key pair whose secret is zeroized on drop.
///
/// `Debug` prints only the public half. The secret accessor exists for the
/// Noise builder and platform key-store adapters; callers must never log or
/// persist it outside a protected store.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct StaticKeypair {
    secret: [u8; STATIC_KEY_BYTES],
    public: [u8; STATIC_KEY_BYTES],
}

impl StaticKeypair {
    /// Generates a fresh key pair from the OS random source.
    pub fn generate() -> Result<Self, PairingError> {
        let mut secret = [0_u8; STATIC_KEY_BYTES];
        getrandom::fill(&mut secret).map_err(|_| PairingError::RandomUnavailable)?;
        Ok(Self::from_secret(secret))
    }

    /// Reconstructs a key pair from its private scalar.
    #[must_use]
    pub fn from_secret(secret: [u8; STATIC_KEY_BYTES]) -> Self {
        let static_secret = StaticSecret::from(secret);
        let public = X25519PublicKey::from(&static_secret).to_bytes();
        Self { secret, public }
    }

    /// Loads a key pair from persisted private bytes.
    pub fn from_store_bytes(bytes: &[u8]) -> Result<Self, PairingError> {
        let secret: [u8; STATIC_KEY_BYTES] = bytes
            .try_into()
            .map_err(|_| PairingError::KeyStoreCorrupt)?;
        Ok(Self::from_secret(secret))
    }

    /// Copies the private scalar into a zeroizing buffer for a key-store write.
    #[must_use]
    pub fn to_store_bytes(&self) -> Zeroizing<[u8; STATIC_KEY_BYTES]> {
        Zeroizing::new(self.secret)
    }

    /// Private scalar. Never log or transmit this value.
    #[must_use]
    pub fn secret_bytes(&self) -> &[u8; STATIC_KEY_BYTES] {
        &self.secret
    }

    /// Returns whether X25519 with `public` yields a non-identity shared
    /// secret. Clamped X25519 scalars are multiples of the cofactor, so every
    /// low-order point (including the all-zero encoding and `u = 1`) collapses
    /// to the identity here. `snow` 0.10.0 does not perform this check itself.
    #[must_use]
    pub fn is_contributory_with(&self, public: &PublicKey) -> bool {
        let secret = StaticSecret::from(self.secret);
        let peer = X25519PublicKey::from(*public.as_bytes());
        secret.diffie_hellman(&peer).was_contributory()
    }

    #[must_use]
    pub fn public_key(&self) -> PublicKey {
        PublicKey(self.public)
    }

    pub fn fingerprint(&self) -> Result<KeyFingerprint, PairingError> {
        self.public_key().fingerprint()
    }
}

impl fmt::Debug for StaticKeypair {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StaticKeypair")
            .field("public", &self.public_key())
            .field("secret", &"[redacted]")
            .finish()
    }
}

/// Typed key-store failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyStoreError {
    /// The platform store could not be reached.
    Unavailable,
    /// The stored value is not a valid static key.
    Corrupt,
}

impl fmt::Display for KeyStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable => formatter.write_str("device key store is unavailable"),
            Self::Corrupt => formatter.write_str("stored device key is corrupt"),
        }
    }
}

impl Error for KeyStoreError {}

impl From<KeyStoreError> for PairingError {
    fn from(error: KeyStoreError) -> Self {
        match error {
            KeyStoreError::Unavailable => Self::KeyStoreUnavailable,
            KeyStoreError::Corrupt => Self::KeyStoreCorrupt,
        }
    }
}

/// Per-device storage for the device static private key.
///
/// Implementations must store the key in the OS-protected key store. The
/// trait is intentionally synchronous and free of I/O specifics; this crate
/// does not implement Keychain/Keyring.
pub trait DeviceKeyStore {
    fn store(&self, device_id: &DeviceId, key: &StaticKeypair) -> Result<(), KeyStoreError>;
    fn load(&self, device_id: &DeviceId) -> Result<Option<StaticKeypair>, KeyStoreError>;
    /// Deletes the key and returns whether an entry existed.
    fn delete(&self, device_id: &DeviceId) -> Result<bool, KeyStoreError>;
}

/// Deterministic in-memory key store for tests and fixtures.
///
/// This is **not** a production key store: keys are lost when the process
/// exits. A production binary must wire an OS Keychain/Keyring adapter.
#[doc(hidden)]
#[derive(Default)]
pub struct InMemoryDeviceKeyStore {
    keys: Mutex<BTreeMap<String, StaticKeypair>>,
}

impl InMemoryDeviceKeyStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.keys.lock().map(|keys| keys.len()).unwrap_or_default()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl fmt::Debug for InMemoryDeviceKeyStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InMemoryDeviceKeyStore")
            .field("keys", &self.len())
            .finish()
    }
}

impl DeviceKeyStore for InMemoryDeviceKeyStore {
    fn store(&self, device_id: &DeviceId, key: &StaticKeypair) -> Result<(), KeyStoreError> {
        let mut keys = self.keys.lock().map_err(|_| KeyStoreError::Unavailable)?;
        keys.insert(device_id.to_string(), key.clone());
        Ok(())
    }

    fn load(&self, device_id: &DeviceId) -> Result<Option<StaticKeypair>, KeyStoreError> {
        let keys = self.keys.lock().map_err(|_| KeyStoreError::Unavailable)?;
        Ok(keys.get(device_id.as_str()).cloned())
    }

    fn delete(&self, device_id: &DeviceId) -> Result<bool, KeyStoreError> {
        let mut keys = self.keys.lock().map_err(|_| KeyStoreError::Unavailable)?;
        Ok(keys.remove(device_id.as_str()).is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_key_base64_is_canonical() {
        let key = PublicKey::from_bytes([0x42; 32]);
        let encoded = key.to_base64();
        assert_eq!(PublicKey::from_base64(&encoded), Some(key));
        assert_eq!(PublicKey::from_base64(&format!("{encoded}=")), None);
        assert_eq!(PublicKey::from_base64("not base64!"), None);
    }

    #[test]
    fn keypair_debug_never_prints_the_secret() {
        let key = StaticKeypair::from_secret([9_u8; 32]);
        let rendered = format!("{key:?}");
        assert!(rendered.contains("[redacted]"));
        assert!(!rendered.contains(&encode(&[9_u8; 32])));
        assert_eq!(key.public_key().fingerprint().unwrap().as_str().len(), 71);
    }

    #[test]
    fn in_memory_store_round_trips_without_leaking_in_debug() {
        let store = InMemoryDeviceKeyStore::new();
        let device_id = DeviceId::new("device-1").unwrap();
        let key = StaticKeypair::from_secret([3_u8; 32]);
        store.store(&device_id, &key).unwrap();
        let loaded = store.load(&device_id).unwrap().unwrap();
        assert_eq!(loaded.public_key(), key.public_key());
        assert!(!format!("{store:?}").contains(&encode(&[3_u8; 32])));
        assert!(store.delete(&device_id).unwrap());
        assert!(store.load(&device_id).unwrap().is_none());
    }

    #[test]
    fn stored_private_bytes_reconstruct_the_same_public_key() {
        let key = StaticKeypair::from_secret([1_u8; 32]);
        let bytes = key.to_store_bytes();
        let restored = StaticKeypair::from_store_bytes(bytes.as_ref()).unwrap();
        assert_eq!(restored.public_key(), key.public_key());
        assert!(matches!(
            StaticKeypair::from_store_bytes(&[1_u8; 31]),
            Err(PairingError::KeyStoreCorrupt)
        ));
    }

    #[test]
    fn rejects_non_contributory_x25519_public_keys() {
        let key = StaticKeypair::from_secret([7_u8; 32]);
        let zero = PublicKey::from_bytes([0_u8; 32]);
        let mut one = [0_u8; 32];
        one[0] = 1;
        let small_order = PublicKey::from_bytes(one);
        assert!(!key.is_contributory_with(&zero));
        assert!(!key.is_contributory_with(&small_order));
        assert!(key.is_contributory_with(&StaticKeypair::from_secret([8_u8; 32]).public_key()));
    }
}
