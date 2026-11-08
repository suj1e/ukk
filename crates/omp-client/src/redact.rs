//! Conservative credential redaction for error and diagnostic text.
//!
//! Only provider-style token shapes and obvious `key=value` secrets are
//! replaced. This is intentionally not a general-purpose scanner: it must not
//! rewrite ordinary model output or source code.

const REDACTED: &str = "<redacted>";

pub fn redact(input: &str) -> String {
    let cleaned = strip_ansi(input);
    let mut out = String::with_capacity(cleaned.len());
    let bytes = cleaned.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if let Some((end, replacement)) = match_secret(&cleaned, bytes, index) {
            out.push_str(&replacement);
            index = end;
            continue;
        }
        let character = cleaned[index..].chars().next().unwrap_or('\u{fffd}');
        out.push(character);
        index += character.len_utf8();
    }
    out
}

/// Strip CSI/OSC escape sequences. Terminal escapes are transport noise, not
/// content: omp writes structured JSON on stdout and never terminal graphics.
pub fn strip_ansi(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == 0x1b {
            if bytes.get(index + 1) == Some(&b'[') {
                index += 2;
                while index < bytes.len() {
                    let byte = bytes[index];
                    index += 1;
                    if (0x40..=0x7e).contains(&byte) {
                        break;
                    }
                }
                continue;
            }
            if bytes.get(index + 1) == Some(&b']') {
                index += 2;
                while index < bytes.len() {
                    if bytes[index] == 0x07 {
                        index += 1;
                        break;
                    }
                    if bytes[index] == 0x1b && bytes.get(index + 1) == Some(&b'\\') {
                        index += 2;
                        break;
                    }
                    index += 1;
                }
                continue;
            }
            index += 1;
            continue;
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Redact, drop terminal control characters and bound the result. Used for
/// errors, stderr lines and UI notices that may quote provider failures.
pub fn bounded_message(input: &str, max_bytes: usize) -> String {
    let redacted = redact(input);
    let mut out = String::with_capacity(redacted.len().min(max_bytes));
    for character in redacted.chars() {
        if character == '\n' || character == '\t' || !character.is_control() {
            out.push(character);
        }
        if out.len() >= max_bytes {
            out.push('…');
            out.push_str(" [truncated]");
            break;
        }
    }
    out
}

fn is_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'+' | b'/' | b'=' | b'~')
}

fn is_ident_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')
}

fn run_end(bytes: &[u8], mut index: usize, allowed: impl Fn(u8) -> bool) -> usize {
    while index < bytes.len() && allowed(bytes[index]) {
        index += 1;
    }
    index
}

fn at_word_start(bytes: &[u8], index: usize) -> bool {
    index == 0 || !is_ident_byte(bytes[index - 1])
}

fn starts_with_ignore_case(bytes: &[u8], index: usize, needle: &[u8]) -> bool {
    bytes.len() >= index + needle.len()
        && bytes[index..index + needle.len()].eq_ignore_ascii_case(needle)
}

fn match_secret(source: &str, bytes: &[u8], index: usize) -> Option<(usize, String)> {
    if !at_word_start(bytes, index) {
        return None;
    }
    // Provider token prefixes.
    for (prefix, minimum) in [
        (b"sk-".as_slice(), 20usize),
        (b"ghp_".as_slice(), 20),
        (b"gho_".as_slice(), 20),
        (b"ghu_".as_slice(), 20),
        (b"ghs_".as_slice(), 20),
        (b"ghr_".as_slice(), 20),
        (b"github_pat_".as_slice(), 20),
        (b"xoxa-".as_slice(), 10),
        (b"xoxb-".as_slice(), 10),
        (b"xoxp-".as_slice(), 10),
        (b"xoxr-".as_slice(), 10),
        (b"xoxs-".as_slice(), 10),
    ] {
        if starts_with_ignore_case(bytes, index, prefix) {
            let end = run_end(bytes, index + prefix.len(), is_token_byte);
            if end - (index + prefix.len()) >= minimum {
                return Some((end, format!("{}REDACTED", String::from_utf8_lossy(prefix))));
            }
        }
    }
    // AWS access key id.
    if starts_with_ignore_case(bytes, index, b"AKIA") {
        let end = run_end(bytes, index + 4, |byte| byte.is_ascii_alphanumeric());
        if end - (index + 4) >= 16 {
            return Some((end, "AKIA".to_owned() + REDACTED));
        }
    }
    // JWT-shaped bearer value (`eyJ...` with two `.` separators).
    if starts_with_ignore_case(bytes, index, b"eyJ") {
        let end = run_end(bytes, index, |byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')
        });
        let value = &source[index..end];
        if value.len() >= 40 && value.matches('.').count() >= 2 {
            return Some((end, format!("eyJ{REDACTED}")));
        }
    }
    // `Bearer <token>` header.
    if starts_with_ignore_case(bytes, index, b"bearer") {
        let mut cursor = index + 6;
        while cursor < bytes.len() && bytes[cursor] == b' ' {
            cursor += 1;
        }
        if cursor > index + 6 {
            let end = run_end(bytes, cursor, is_token_byte);
            if end - cursor >= 8 {
                return Some((end, "Bearer <redacted>".into()));
            }
        }
    }
    // `password=...`, `api_key: ...`, `token = "..."`.
    let key_end = run_end(bytes, index, is_ident_byte);
    if key_end > index {
        let key = source[index..key_end].to_ascii_lowercase();
        let sensitive = ["key", "token", "secret", "password", "credential", "passwd"]
            .iter()
            .any(|needle| key.contains(needle));
        if sensitive && key_end < bytes.len() {
            let separator = bytes[key_end];
            let next = bytes.get(key_end + 1).copied();
            let looks_like_url = separator == b':' && next == Some(b'/');
            if (separator == b'=' || separator == b':') && !looks_like_url {
                let mut value_start = key_end + 1;
                while value_start < bytes.len() && bytes[value_start] == b' ' {
                    value_start += 1;
                }
                let quote = bytes
                    .get(value_start)
                    .copied()
                    .filter(|byte| *byte == b'"' || *byte == b'\'');
                if quote.is_some() {
                    value_start += 1;
                }
                let value_end = run_end(bytes, value_start, is_token_byte);
                if value_end - value_start >= 8 {
                    let mut replacement = source[index..=key_end].to_owned();
                    if let Some(quote) = quote {
                        replacement.push(quote as char);
                    }
                    replacement.push_str(REDACTED);
                    return Some((value_end, replacement));
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_provider_tokens_are_redacted() {
        let token = format!("sk-{}", "abcDEF0123456789xyzEXTRA");
        let text = format!("request failed: invalid key {token} token");
        let redacted = redact(&text);
        assert!(!redacted.contains(&token));
        assert!(redacted.contains("sk-REDACTED"));
        assert!(redacted.contains("request failed"));
    }

    #[test]
    fn bearer_and_assignment_secrets_are_redacted() {
        let text = "Authorization: Bearer abcdef1234567890 api_key=\"super-secret-value\"";
        let redacted = redact(text);
        assert!(!redacted.contains("abcdef1234567890"));
        assert!(!redacted.contains("super-secret-value"));
        assert!(redacted.contains("api_key="));
    }

    #[test]
    fn ordinary_text_and_short_values_are_untouched() {
        let text = "Bash failed: exit code 1. key=value models like gpt-5 are fine.";
        assert_eq!(redact(text), text);
    }

    #[test]
    fn ansi_sequences_are_removed() {
        assert_eq!(redact("\u{1b}[31mred\u{1b}[0m text"), "red text");
    }

    #[test]
    fn bounded_message_truncates_on_a_character_boundary() {
        let text = "é".repeat(100);
        let bounded = bounded_message(&text, 16);
        assert!(bounded.len() <= 16 + "… [truncated]".len());
        assert!(bounded.ends_with("[truncated]"));
    }
}
