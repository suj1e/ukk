//! Typed adapter errors.
//!
//! Errors deliberately exclude raw provider payloads: they identify the action
//! and field that failed so diagnostics cannot leak credentials.

use crate::contract::{FallbackReason, TerminalFallback};
use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AdapterError {
    Malformed {
        native_type: Option<String>,
        detail: String,
    },
    PayloadTooLarge {
        field: &'static str,
        limit: usize,
        actual: usize,
    },
    FieldTooLong {
        field: &'static str,
        limit: usize,
        actual: usize,
    },
    MissingField {
        native_type: String,
        field: &'static str,
    },
    InvalidField {
        native_type: String,
        field: &'static str,
        detail: String,
    },
    UnsupportedProvider {
        provider: String,
    },
    UnsupportedProviderVersion {
        provider: String,
        version: String,
        supported: String,
    },
    MissingCapability {
        provider: String,
        method: String,
    },
    ContractVersionMismatch {
        expected: u32,
        actual: u32,
    },
    StreamBufferOverflow {
        limit: usize,
    },
}

impl AdapterError {
    pub fn fallback(&self) -> TerminalFallback {
        let (reason, detail) = match self {
            Self::Malformed {
                native_type,
                detail,
            } => (
                FallbackReason::MalformedEvent,
                format!(
                    "malformed {}: {detail}",
                    native_type.as_deref().unwrap_or("provider event")
                ),
            ),
            Self::PayloadTooLarge {
                field,
                limit,
                actual,
            } => (
                FallbackReason::PayloadTooLarge,
                format!("{field} exceeded {limit} bytes (got {actual})"),
            ),
            Self::FieldTooLong {
                field,
                limit,
                actual,
            } => (
                FallbackReason::FieldTooLong,
                format!("{field} exceeded {limit} bytes (got {actual})"),
            ),
            Self::MissingField { native_type, field } => (
                FallbackReason::MissingField,
                format!("{native_type} is missing {field}"),
            ),
            Self::InvalidField {
                native_type,
                field,
                detail,
            } => (
                FallbackReason::MalformedEvent,
                format!("{native_type}.{field} is invalid: {detail}"),
            ),
            Self::UnsupportedProvider { provider } => (
                FallbackReason::UnknownProvider,
                format!("provider {provider} has no structured adapter"),
            ),
            Self::UnsupportedProviderVersion {
                provider,
                version,
                supported,
            } => (
                FallbackReason::UnsupportedVersion,
                format!("{provider} {version} is not supported (supported: {supported})"),
            ),
            Self::MissingCapability { provider, method } => (
                FallbackReason::UnsupportedVersion,
                format!("{provider} did not report required method {method}"),
            ),
            Self::ContractVersionMismatch { expected, actual } => (
                FallbackReason::ContractVersionMismatch,
                format!("contract version {actual} is not {expected}"),
            ),
            Self::StreamBufferOverflow { limit } => (
                FallbackReason::StreamBufferOverflow,
                format!("more than {limit} out-of-order line batches buffered"),
            ),
        };
        TerminalFallback::raw_terminal(reason, detail)
    }
}

impl fmt::Display for AdapterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let fallback = self.fallback();
        formatter.write_str(&fallback.detail)
    }
}

impl std::error::Error for AdapterError {}
