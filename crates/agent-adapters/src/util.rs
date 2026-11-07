//! Internal parsing helpers shared by provider adapters.

use crate::adapter::AdapterConfig;
use crate::error::AdapterError;
use crate::limits::BoundedText;
use serde_json::Value;

pub(crate) fn redact_bounded(config: &AdapterConfig, text: &str, limit: usize) -> BoundedText {
    let redacted = config.redactor.redact(text);
    config.limits.bounded(&redacted, limit)
}

pub(crate) fn bounded_json(config: &AdapterConfig, value: &Value, limit: usize) -> BoundedText {
    match serde_json::to_string(value) {
        Ok(serialized) => redact_bounded(config, &serialized, limit),
        Err(_) => BoundedText {
            text: String::new(),
            truncated: true,
        },
    }
}

/// Keeps unsupported-action names safe for display and bounded.
pub(crate) fn sanitize_action_name(value: &str) -> String {
    let mut sanitized: String = value
        .chars()
        .take(128)
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '/' | '_' | '-' | '.') {
                character
            } else {
                '?'
            }
        })
        .collect();
    if value.chars().count() > 128 {
        sanitized.push('…');
    }
    sanitized
}

pub(crate) fn required_str<'a>(
    value: &'a Value,
    field: &'static str,
    native_type: &str,
) -> Result<&'a str, AdapterError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| AdapterError::MissingField {
            native_type: native_type.to_owned(),
            field,
        })
}

pub(crate) fn optional_str<'a>(value: &'a Value, field: &str) -> Option<&'a str> {
    value.get(field).and_then(Value::as_str)
}

pub(crate) fn checked_id(
    config: &AdapterConfig,
    field: &'static str,
    value: &str,
) -> Result<String, AdapterError> {
    config.limits.check_id(field, value)?;
    Ok(value.to_owned())
}

pub(crate) fn checked_opt_id(
    config: &AdapterConfig,
    field: &'static str,
    value: Option<&str>,
) -> Result<Option<String>, AdapterError> {
    match value {
        Some(value) => checked_id(config, field, value).map(Some),
        None => Ok(None),
    }
}

pub(crate) fn checked_str_id<'a>(
    config: &AdapterConfig,
    value: &'a Value,
    field: &'static str,
    native_type: &str,
) -> Result<&'a str, AdapterError> {
    let id = required_str(value, field, native_type)?;
    config.limits.check_id(field, id)?;
    Ok(id)
}

/// Converts a JSON-RPC id that can be a string or a number.
pub(crate) fn request_id(value: &Value) -> Option<String> {
    match value {
        Value::String(id) => Some(id.clone()),
        Value::Number(id) => Some(id.to_string()),
        _ => None,
    }
}

/// Stable within-process fingerprint used only when a provider message has no
/// documented unique event id. It is not a content hash for security purposes.
pub(crate) fn stable_fingerprint(text: &str) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    hash
}
