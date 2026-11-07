//! Single redaction helper for text that crosses a trust boundary.
//!
//! Anything a helper process writes (error messages, identities, frames) is
//! untrusted: it may contain newlines for log forgery, ANSI escapes for
//! terminal injection, bidi overrides for display spoofing, or simply be very
//! long. Audit records and user-facing errors must be one line and bounded, so
//! every such string goes through [`redact`].
//!
//! Redaction never drops provenance silently: it escapes the character, so
//! `evil\nforged-record` becomes `evil\\nforged-record` rather than two lines.
use std::fmt::Write as _;

/// Maximum number of characters kept from untrusted text.
pub const MAX_TRUSTED_TEXT_CHARS: usize = 200;

/// Escape control/invisible characters and cap the result at
/// [`MAX_TRUSTED_TEXT_CHARS`] characters, appending `...` when truncated.
///
/// The output is always a single line and contains no raw ESC, newline, tab or
/// bidi override.
pub fn redact(text: &str) -> String {
    let mut out = String::with_capacity(text.len().min(MAX_TRUSTED_TEXT_CHARS));
    let mut written = 0_usize;
    for character in text.chars() {
        if written >= MAX_TRUSTED_TEXT_CHARS {
            out.push_str("...");
            return out;
        }
        if is_invisible(character) {
            for escaped in character.escape_debug() {
                if written >= MAX_TRUSTED_TEXT_CHARS {
                    out.push_str("...");
                    return out;
                }
                let _ = out.write_char(escaped);
                written += 1;
            }
        } else {
            let _ = out.write_char(character);
            written += 1;
        }
    }
    out
}

fn is_invisible(character: char) -> bool {
    match character {
        // C0/C1 controls, including ESC, newline, carriage return and TAB.
        c if c.is_control() => true,
        // Bidi controls and isolates can reorder rendered text.
        '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' => true,
        // Line/paragraph separators are not `is_control` but break lines.
        '\u{2028}' | '\u{2029}' => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newlines_and_escapes_are_escaped_not_passed_through() {
        let redacted = redact("evil\nforged\u{1b}[31mred");
        assert!(!redacted.contains('\n'));
        assert!(!redacted.contains('\u{1b}'));
        assert!(redacted.contains("\\n"));
        assert!(redacted.contains("\\u{1b}"));
    }

    #[test]
    fn bidi_overrides_are_escaped() {
        let redacted = redact("safe\u{202e}gnp.exe");
        assert!(!redacted.contains('\u{202e}'));
        assert!(redacted.contains("\\u{202e}"));
    }

    #[test]
    fn long_text_is_capped() {
        let redacted = redact(&"x".repeat(10_000));
        assert_eq!(redacted.chars().count(), MAX_TRUSTED_TEXT_CHARS + 3);
        assert!(redacted.ends_with("..."));
    }

    #[test]
    fn normal_text_is_unchanged() {
        assert_eq!(
            redact("command `sample.session.ping`"),
            "command `sample.session.ping`"
        );
    }

    #[test]
    fn every_output_is_single_line_and_bounded() {
        for input in [
            "\r\n",
            "\u{1b}]0;title\u{7}",
            "\u{2028}\u{2029}",
            &"a\n".repeat(500),
        ] {
            let redacted = redact(input);
            assert!(!redacted
                .chars()
                .any(|c| c == '\n' || c == '\r' || c == '\u{1b}'));
            assert!(redacted.chars().count() <= MAX_TRUSTED_TEXT_CHARS + 3);
        }
    }
}
