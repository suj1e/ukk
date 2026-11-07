//! Bounded payload helpers.
//!
//! Provider streams are untrusted input. A single line or field must never be
//! able to grow adapter state without a limit. Truncation is reported with a
//! flag instead of appending a synthetic marker to provider text.

use crate::AdapterError;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Limits {
    /// Maximum bytes accepted for one transport line.
    pub max_line_bytes: usize,
    /// Maximum bytes retained for message/progress text.
    pub max_text_bytes: usize,
    /// Maximum bytes retained for tool input and artifact diffs.
    pub max_input_bytes: usize,
    /// Maximum bytes retained for tool output.
    pub max_output_bytes: usize,
    /// Maximum bytes accepted for a provider identifier.
    pub max_id_bytes: usize,
    /// Maximum number of out-of-order line batches buffered per stream.
    pub max_pending_events: usize,
    /// Maximum number of recorded progress observations.
    pub max_progress_entries: usize,
    /// Maximum number of deduplication keys retained.
    pub max_dedupe_entries: usize,
    /// Maximum number of transport streams retained by merge/adapter state.
    /// Evicting an idle stream never fabricates events; its history is already
    /// merged into the conversation.
    pub max_streams: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_line_bytes: 1024 * 1024,
            max_text_bytes: 64 * 1024,
            max_input_bytes: 32 * 1024,
            max_output_bytes: 64 * 1024,
            max_id_bytes: 1024,
            max_pending_events: 256,
            max_progress_entries: 512,
            max_dedupe_entries: 4096,
            max_streams: 64,
        }
    }
}

/// A string that was redacted and then bounded.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoundedText {
    pub text: String,
    pub truncated: bool,
}

impl BoundedText {
    pub fn empty() -> Self {
        Self {
            text: String::new(),
            truncated: false,
        }
    }

    pub fn from_text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            truncated: false,
        }
    }
}

impl Limits {
    pub fn check_line(&self, line: &str) -> Result<(), AdapterError> {
        if line.len() > self.max_line_bytes {
            return Err(AdapterError::PayloadTooLarge {
                field: "line",
                limit: self.max_line_bytes,
                actual: line.len(),
            });
        }
        Ok(())
    }

    pub fn check_id(&self, field: &'static str, value: &str) -> Result<(), AdapterError> {
        if value.len() > self.max_id_bytes {
            return Err(AdapterError::FieldTooLong {
                field,
                limit: self.max_id_bytes,
                actual: value.len(),
            });
        }
        Ok(())
    }

    pub fn bounded(&self, text: &str, limit: usize) -> BoundedText {
        let (bounded, truncated) = truncate_utf8(text, limit);
        BoundedText {
            text: bounded.to_owned(),
            truncated,
        }
    }
}

pub(crate) fn truncate_utf8(text: &str, limit: usize) -> (&str, bool) {
    if text.len() <= limit {
        return (text, false);
    }
    let mut end = limit;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    (&text[..end], true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncates_at_utf8_boundary() {
        let text = "aé€";
        let (bounded, truncated) = truncate_utf8(text, 3);
        assert!(truncated);
        assert!(bounded.is_char_boundary(bounded.len()));
        assert_eq!(bounded, "aé");
    }

    #[test]
    fn leaves_short_text_untouched() {
        let (bounded, truncated) = truncate_utf8("ok", 8);
        assert!(!truncated);
        assert_eq!(bounded, "ok");
    }
}
