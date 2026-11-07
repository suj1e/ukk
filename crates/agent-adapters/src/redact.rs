//! Redaction hooks applied before adapter state retains provider text.
//!
//! The default hook is a no-op. Host integrations that know credential
//! formats can supply a literal rule set. Redaction happens before truncation
//! so a secret cannot survive as a truncated prefix.

use std::borrow::Cow;

pub trait Redactor: Send + Sync {
    fn redact<'a>(&self, text: &'a str) -> Cow<'a, str>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NoopRedactor;

impl Redactor for NoopRedactor {
    fn redact<'a>(&self, text: &'a str) -> Cow<'a, str> {
        Cow::Borrowed(text)
    }
}

/// Literal substring replacement.
///
/// This is intentionally not a regular-expression engine: callers should
/// provide exact secret values or well-known token prefixes. An empty source
/// pattern is ignored so it cannot match every position.
#[derive(Clone, Debug, Default)]
pub struct RuleRedactor {
    rules: Vec<(String, String)>,
}

impl RuleRedactor {
    pub fn new<I, S, R>(rules: I) -> Self
    where
        I: IntoIterator<Item = (S, R)>,
        S: Into<String>,
        R: Into<String>,
    {
        Self {
            rules: rules
                .into_iter()
                .map(|(source, replacement)| (source.into(), replacement.into()))
                .filter(|(source, _)| !source.is_empty())
                .collect(),
        }
    }

    pub fn push(&mut self, source: impl Into<String>, replacement: impl Into<String>) {
        let source = source.into();
        if !source.is_empty() {
            self.rules.push((source, replacement.into()));
        }
    }
}

impl Redactor for RuleRedactor {
    fn redact<'a>(&self, text: &'a str) -> Cow<'a, str> {
        let mut current = Cow::Borrowed(text);
        for (source, replacement) in &self.rules {
            if current.contains(source) {
                current = Cow::Owned(current.replace(source, replacement));
            }
        }
        current
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_rules_replace_all_occurrences() {
        let redactor = RuleRedactor::new([("sk-test", "[REDACTED]")]);
        let redacted = redactor.redact("key=sk-test other=sk-test");
        assert_eq!(redacted, "key=[REDACTED] other=[REDACTED]");
    }

    #[test]
    fn empty_pattern_is_ignored() {
        let redactor = RuleRedactor::new([("", "x")]);
        assert_eq!(redactor.redact("abc"), "abc");
    }
}
