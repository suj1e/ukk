//! Plugin API version negotiation, independent of the plugin package version.
//!
//! A manifest states a range such as `>=0.1,<0.2`. The host computes its own
//! [`HOST_API_VERSION`](crate::HOST_API_VERSION) and refuses to load a plugin
//! whose range excludes it. Patch numbers are accepted but ignored when the
//! manifest writes a two-component version (`0.1` == `0.1.0`).
use std::fmt;
use std::str::FromStr;

/// A semantic API version: major.minor.patch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ApiVersion {
    major: u32,
    minor: u32,
    patch: u32,
}

impl ApiVersion {
    pub const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    pub const fn major(self) -> u32 {
        self.major
    }

    pub const fn minor(self) -> u32 {
        self.minor
    }

    pub const fn patch(self) -> u32 {
        self.patch
    }
}

impl fmt::Display for ApiVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Rejection reason for [`ApiVersion::from_str`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiVersionError(pub String);

impl fmt::Display for ApiVersionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for ApiVersionError {}

impl FromStr for ApiVersion {
    type Err = ApiVersionError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let mut parts = text.trim().split('.');
        let major = parse_component(parts.next(), text)?;
        let minor = parse_component(parts.next(), text)?;
        let patch = match parts.next() {
            Some(value) => parse_component(Some(value), text)?,
            None => 0,
        };
        if parts.next().is_some() {
            return Err(ApiVersionError(format!("too many components in `{text}`")));
        }
        Ok(Self::new(major, minor, patch))
    }
}

fn parse_component(part: Option<&str>, whole: &str) -> Result<u32, ApiVersionError> {
    let part = part.ok_or_else(|| ApiVersionError(format!("incomplete version `{whole}`")))?;
    if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(ApiVersionError(format!(
            "`{part}` is not a numeric version component in `{whole}`"
        )));
    }
    part.parse::<u32>()
        .map_err(|_| ApiVersionError(format!("version component `{part}` overflows u32")))
}

/// Inclusive/exclusive bound used by [`ApiRange`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Bound<T> {
    Inclusive(T),
    Exclusive(T),
}

impl Bound<ApiVersion> {
    fn allows(&self, version: ApiVersion, is_lower: bool) -> bool {
        match (self, is_lower) {
            (Bound::Inclusive(bound), true) => version >= *bound,
            (Bound::Exclusive(bound), true) => version > *bound,
            (Bound::Inclusive(bound), false) => version <= *bound,
            (Bound::Exclusive(bound), false) => version < *bound,
        }
    }
}

/// Accepted API range, e.g. `>=0.1,<0.2`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiRange {
    lower: Option<Bound<ApiVersion>>,
    upper: Option<Bound<ApiVersion>>,
    /// Original, normalized constraint text kept for diagnostics and audit.
    text: String,
}

impl ApiRange {
    /// Parse a comma-separated list of `>=`, `>`, `<=`, `<` or `=` constraints.
    pub fn parse(text: &str) -> Result<Self, ApiRangeError> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Err(ApiRangeError::Empty);
        }
        let mut lower = None;
        let mut upper = None;
        let mut parts = Vec::new();
        for raw in trimmed.split(',') {
            let raw = raw.trim();
            if raw.is_empty() {
                return Err(ApiRangeError::EmptyPart(trimmed.to_owned()));
            }
            let (operator, version_text) = split_operator(raw)?;
            if version_text.is_empty() {
                return Err(ApiRangeError::MissingVersion(raw.to_owned()));
            }
            let version = ApiVersion::from_str(version_text)
                .map_err(|err| ApiRangeError::InvalidVersion(err.to_string()))?;
            parts.push(format!("{operator}{version}"));
            match operator {
                ">=" => assign_lower(&mut lower, Bound::Inclusive(version)),
                ">" => assign_lower(&mut lower, Bound::Exclusive(version)),
                "<=" => assign_upper(&mut upper, Bound::Inclusive(version)),
                "<" => assign_upper(&mut upper, Bound::Exclusive(version)),
                "=" => {
                    assign_lower(&mut lower, Bound::Inclusive(version));
                    assign_upper(&mut upper, Bound::Inclusive(version));
                }
                other => return Err(ApiRangeError::UnknownOperator(other.to_owned())),
            }
        }
        let range = Self {
            lower,
            upper,
            text: parts.join(","),
        };
        range.check_not_empty()?;
        Ok(range)
    }

    fn check_not_empty(&self) -> Result<(), ApiRangeError> {
        // A range is empty when its lower bound is already above its upper
        // bound; `0.1` alone can never be satisfied for `>0.1,<=0.1` etc.
        let Some(lower) = self.lower else {
            return Ok(());
        };
        let Some(upper) = self.upper else {
            return Ok(());
        };
        let empty = match (lower, upper) {
            (Bound::Inclusive(low), Bound::Inclusive(high)) => low > high,
            (Bound::Inclusive(low), Bound::Exclusive(high)) => low >= high,
            (Bound::Exclusive(low), Bound::Inclusive(high)) => low >= high,
            (Bound::Exclusive(low), Bound::Exclusive(high)) => low >= high,
        };
        if empty {
            return Err(ApiRangeError::EmptyRange(self.text.clone()));
        }
        Ok(())
    }

    /// Whether `version` lies inside the range.
    pub fn contains(&self, version: ApiVersion) -> bool {
        let lower_ok = self
            .lower
            .map(|bound| bound.allows(version, true))
            .unwrap_or(true);
        let upper_ok = self
            .upper
            .map(|bound| bound.allows(version, false))
            .unwrap_or(true);
        lower_ok && upper_ok
    }

    /// Normalized text of the range as parsed.
    pub fn as_str(&self) -> &str {
        &self.text
    }
}

fn split_operator(raw: &str) -> Result<(&'static str, &str), ApiRangeError> {
    for operator in [">=", "<=", ">", "<", "="] {
        if let Some(rest) = raw.strip_prefix(operator) {
            return Ok((operator, rest.trim()));
        }
    }
    Err(ApiRangeError::MissingOperator(raw.to_owned()))
}

/// Keep the more restrictive lower bound (the larger version).
fn assign_lower(slot: &mut Option<Bound<ApiVersion>>, bound: Bound<ApiVersion>) {
    let Some(existing) = *slot else {
        *slot = Some(bound);
        return;
    };
    let keep_existing = match (existing, bound) {
        (Bound::Inclusive(a), Bound::Inclusive(b)) => a >= b,
        (Bound::Inclusive(a), Bound::Exclusive(b)) => a > b,
        (Bound::Exclusive(a), Bound::Exclusive(b)) => a >= b,
        (Bound::Exclusive(a), Bound::Inclusive(b)) => a >= b,
    };
    if !keep_existing {
        *slot = Some(bound);
    }
}

/// Keep the more restrictive upper bound (the smaller version).
fn assign_upper(slot: &mut Option<Bound<ApiVersion>>, bound: Bound<ApiVersion>) {
    let Some(existing) = *slot else {
        *slot = Some(bound);
        return;
    };
    let keep_existing = match (existing, bound) {
        (Bound::Inclusive(a), Bound::Inclusive(b)) => a <= b,
        (Bound::Inclusive(a), Bound::Exclusive(b)) => a < b,
        (Bound::Exclusive(a), Bound::Exclusive(b)) => a <= b,
        (Bound::Exclusive(a), Bound::Inclusive(b)) => a <= b,
    };
    if !keep_existing {
        *slot = Some(bound);
    }
}

impl fmt::Display for ApiRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

/// Rejection reason for [`ApiRange::parse`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiRangeError {
    Empty,
    EmptyPart(String),
    MissingOperator(String),
    UnknownOperator(String),
    MissingVersion(String),
    InvalidVersion(String),
    EmptyRange(String),
}

impl fmt::Display for ApiRangeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("api range is empty"),
            Self::EmptyPart(text) => write!(f, "api range `{text}` contains an empty constraint"),
            Self::MissingOperator(raw) => {
                write!(f, "api constraint `{raw}` is missing an operator")
            }
            Self::UnknownOperator(operator) => {
                write!(f, "unsupported api operator `{operator}`")
            }
            Self::MissingVersion(raw) => write!(f, "api constraint `{raw}` has no version"),
            Self::InvalidVersion(detail) => write!(f, "invalid api version: {detail}"),
            Self::EmptyRange(text) => write!(f, "api range `{text}` can never be satisfied"),
        }
    }
}

impl std::error::Error for ApiRangeError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_two_and_three_component_versions() {
        assert_eq!(
            "0.1".parse::<ApiVersion>().unwrap(),
            ApiVersion::new(0, 1, 0)
        );
        assert_eq!(
            "0.1.7".parse::<ApiVersion>().unwrap(),
            ApiVersion::new(0, 1, 7)
        );
        assert!("0.1.2.3".parse::<ApiVersion>().is_err());
        assert!("0.x".parse::<ApiVersion>().is_err());
        assert!("".parse::<ApiVersion>().is_err());
    }

    #[test]
    fn contains_uses_inclusive_lower_and_exclusive_upper() {
        let range = ApiRange::parse(">=0.1,<0.2").unwrap();
        assert!(range.contains(ApiVersion::new(0, 1, 0)));
        assert!(range.contains(ApiVersion::new(0, 1, 9)));
        assert!(!range.contains(ApiVersion::new(0, 2, 0)));
        assert!(!range.contains(ApiVersion::new(0, 0, 9)));
    }

    #[test]
    fn rejects_malformed_ranges() {
        assert_eq!(ApiRange::parse("").unwrap_err(), ApiRangeError::Empty);
        assert!(matches!(
            ApiRange::parse("0.1").unwrap_err(),
            ApiRangeError::MissingOperator(_)
        ));
        assert!(matches!(
            ApiRange::parse("~>0.1").unwrap_err(),
            ApiRangeError::MissingOperator(_)
        ));
        assert!(matches!(
            ApiRange::parse(">=0.1,").unwrap_err(),
            ApiRangeError::EmptyPart(_)
        ));
        assert!(ApiRange::parse(">0.2,<0.1").is_err());
    }

    #[test]
    fn exact_range_is_a_point() {
        let range = ApiRange::parse("=0.1.0").unwrap();
        assert!(range.contains(ApiVersion::new(0, 1, 0)));
        assert!(!range.contains(ApiVersion::new(0, 1, 1)));
    }
}
