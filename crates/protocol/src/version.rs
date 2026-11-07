//! Protocol generation negotiation.
//!
//! Version negotiation happens once per connection. A negotiated generation
//! pins the envelope schema for that connection; every subsequent envelope must
//! carry exactly that generation. There is deliberately no partial downgrade or
//! "best effort" parse: unknown generations fail closed.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;

/// Wire protocol generation. Generation zero is invalid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProtocolGeneration(u16);

impl ProtocolGeneration {
    pub const fn from_raw(raw: u16) -> Self {
        Self(raw)
    }

    #[must_use]
    pub const fn raw(self) -> u16 {
        self.0
    }
}

impl fmt::Display for ProtocolGeneration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "generation {}", self.0)
    }
}

/// Generation implemented by this crate.
pub const CURRENT_GENERATION: ProtocolGeneration = ProtocolGeneration::from_raw(1);
/// Oldest generation this crate still understands.
pub const MIN_SUPPORTED_GENERATION: ProtocolGeneration = ProtocolGeneration::from_raw(1);

/// Negotiated generation for one connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Negotiated {
    pub generation: ProtocolGeneration,
}

impl Negotiated {
    #[must_use]
    pub const fn new(generation: ProtocolGeneration) -> Self {
        Self { generation }
    }

    /// Ensures an envelope was written for the negotiated generation.
    pub fn ensure(&self, actual: ProtocolGeneration) -> Result<(), NegotiationError> {
        if actual != self.generation {
            return Err(NegotiationError::GenerationMismatch {
                expected: self.generation,
                actual,
            });
        }
        Ok(())
    }
}

/// Closed set of protocol features advertised after negotiation.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Features(BTreeSet<String>);

/// Structured agent approval decisions.
pub const FEATURE_AGENT_APPROVAL: &str = "agent.approval";
/// Durable snapshot + replay event synchronization.
pub const FEATURE_DURABLE_SYNC: &str = "events.durable_sync";
/// Remote screen preview media is available.
pub const FEATURE_SCREEN_PREVIEW: &str = "screen.preview";
/// Remote screen input is available behind a controller lease.
pub const FEATURE_SCREEN_CONTROL: &str = "screen.control";

impl Features {
    #[must_use]
    pub fn from_static(features: &[&str]) -> Self {
        Self(
            features
                .iter()
                .map(|feature| (*feature).to_owned())
                .collect(),
        )
    }

    #[must_use]
    pub fn default_server() -> Self {
        Self::from_static(&[
            FEATURE_AGENT_APPROVAL,
            FEATURE_DURABLE_SYNC,
            FEATURE_SCREEN_PREVIEW,
        ])
    }

    pub fn insert(&mut self, feature: impl Into<String>) {
        self.0.insert(feature.into());
    }

    #[must_use]
    pub fn contains(&self, feature: &str) -> bool {
        self.0.contains(feature)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Inclusive generation range served by one Host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProtocolRange {
    pub min: ProtocolGeneration,
    pub max: ProtocolGeneration,
}

impl ProtocolRange {
    #[must_use]
    pub const fn new(min: ProtocolGeneration, max: ProtocolGeneration) -> Self {
        Self { min, max }
    }

    #[must_use]
    pub const fn current() -> Self {
        Self::new(MIN_SUPPORTED_GENERATION, CURRENT_GENERATION)
    }

    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.min.raw() > 0 && self.min <= self.max
    }

    #[must_use]
    pub fn contains(&self, generation: ProtocolGeneration) -> bool {
        generation.raw() > 0 && generation >= self.min && generation <= self.max
    }
}

impl fmt::Display for ProtocolRange {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}..={}", self.min.raw(), self.max.raw())
    }
}

/// Typed negotiation failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NegotiationError {
    EmptyClientOffer,
    InvalidGeneration(ProtocolGeneration),
    NoCommonGeneration {
        server: ProtocolRange,
    },
    GenerationMismatch {
        expected: ProtocolGeneration,
        actual: ProtocolGeneration,
    },
}

impl fmt::Display for NegotiationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyClientOffer => formatter.write_str("client offered no protocol generations"),
            Self::InvalidGeneration(generation) => {
                write!(formatter, "invalid protocol generation {generation}")
            }
            Self::NoCommonGeneration { server } => write!(
                formatter,
                "no common protocol generation; server supports {server}"
            ),
            Self::GenerationMismatch { expected, actual } => {
                write!(formatter, "envelope is {actual}, connection is {expected}")
            }
        }
    }
}

impl Error for NegotiationError {}

/// Server policy: supported generations plus advertised features.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerPolicy {
    pub supported: ProtocolRange,
    pub features: Features,
}

impl Default for ServerPolicy {
    fn default() -> Self {
        Self {
            supported: ProtocolRange::current(),
            features: Features::default_server(),
        }
    }
}

impl ServerPolicy {
    /// Picks the highest generation offered by the client *and* served here.
    pub fn negotiate(
        &self,
        client_supported: &[ProtocolGeneration],
    ) -> Result<Negotiated, NegotiationError> {
        if client_supported.is_empty() {
            return Err(NegotiationError::EmptyClientOffer);
        }
        if !self.supported.is_valid() {
            return Err(NegotiationError::NoCommonGeneration {
                server: self.supported,
            });
        }
        let mut best = None;
        for generation in client_supported {
            if generation.raw() == 0 {
                return Err(NegotiationError::InvalidGeneration(*generation));
            }
            if self.supported.contains(*generation) {
                best = Some(best.map_or(*generation, |current: ProtocolGeneration| {
                    current.max(*generation)
                }));
            }
        }
        best.map(Negotiated::new)
            .ok_or(NegotiationError::NoCommonGeneration {
                server: self.supported,
            })
    }
}
