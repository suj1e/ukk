//! Small typed identifiers used by the provider-neutral contract.
//!
//! Provider identities are opaque strings. These newtypes prevent accidentally
//! mixing a turn id with a tool-call id while keeping serialization simple.

use serde::{Deserialize, Serialize};
use std::fmt;

macro_rules! define_id {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn is_empty(&self) -> bool {
                self.0.is_empty()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_owned())
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }
    };
}

define_id!(SessionId, "A provider session or conversation id.");
define_id!(
    TurnId,
    "A provider turn id, when the provider documents one."
);
define_id!(ItemId, "A provider item, message or tool-call id.");
define_id!(
    StreamId,
    "A transport stream id assigned by the host integration."
);
define_id!(
    RequestId,
    "A provider request id (JSON-RPC id or control request id)."
);
define_id!(ApprovalId, "A provider approval id.");
