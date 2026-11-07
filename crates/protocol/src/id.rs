//! Validated opaque identifiers used on the wire.
//!
//! The protocol never derives identity from an unvalidated free-form string.
//! Every identifier is non-empty ASCII graphic text of bounded length, which
//! keeps framing and log output predictable and prevents control characters
//! from entering durable records.

use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fmt;

/// Maximum wire identifier length in bytes.
pub const MAX_ID_BYTES: usize = 128;

/// Typed validation failure for opaque identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdError {
    Empty,
    TooLong,
    InvalidCharacter,
}

impl fmt::Display for IdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("identifier must not be empty"),
            Self::TooLong => formatter.write_str("identifier exceeds the wire length limit"),
            Self::InvalidCharacter => {
                formatter.write_str("identifier must contain printable ASCII characters only")
            }
        }
    }
}

impl Error for IdError {}

fn validate_id(value: &str) -> Result<(), IdError> {
    if value.is_empty() {
        return Err(IdError::Empty);
    }
    if value.len() > MAX_ID_BYTES {
        return Err(IdError::TooLong);
    }
    if !value.bytes().all(|byte| (0x21..=0x7e).contains(&byte)) {
        return Err(IdError::InvalidCharacter);
    }
    Ok(())
}

macro_rules! opaque_id {
    ($name:ident, $label:literal) => {
        #[doc = concat!("Opaque wire identifier: ", $label, ".")]
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            /// Validates and wraps a wire identifier.
            pub fn new(value: impl Into<String>) -> Result<Self, IdError> {
                let value = value.into();
                validate_id(&value)?;
                Ok(Self(value))
            }

            /// Generates a fresh random identifier.
            #[must_use]
            pub fn new_random() -> Self {
                Self(uuid::Uuid::new_v4().to_string())
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }

        impl TryFrom<String> for $name {
            type Error = IdError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.0
            }
        }
    };
}

opaque_id!(RequestId, "command request correlation id");
opaque_id!(EventId, "durable event identity used for deduplication");
opaque_id!(IdempotencyKey, "client supplied retry key");
opaque_id!(DomainId, "Host domain entity id");
opaque_id!(StreamId, "durable per-stream cursor namespace");
opaque_id!(DeviceId, "paired device identity");
opaque_id!(KeyFingerprint, "device public-key fingerprint");
opaque_id!(LeaseId, "remote screen controller lease");
