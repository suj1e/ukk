//! Bounded, typed signalling fields: SDP, ICE candidates, ICE servers, codecs
//! and transport capabilities.
//!
//! Every opaque field validates its size and character set on construction and
//! again when a [`crate::MediaCapabilities`] or signalling message is checked.
//! `Debug`/`Display` for SDP, ICE candidates and TURN credentials redact the
//! content; only explicit transport accessors expose it.

use crate::error::SignallingError;
use crate::limits::{
    MAX_CAPABILITY_CODECS, MAX_CODEC_NAME_BYTES, MAX_CREDENTIAL_BYTES, MAX_ICE_CANDIDATE_BYTES,
    MAX_ICE_SERVERS, MAX_ICE_URLS_PER_SERVER, MAX_ICE_URL_BYTES, MAX_SDP_BYTES,
};
use serde::{Deserialize, Serialize};
use std::fmt;
use ukk_protocol::LeaseId;

/// Absolute Unix-epoch millisecond timestamp that must be positive.
fn positive_expiry(value: i64) -> Result<i64, SignallingError> {
    if value <= 0 {
        return Err(SignallingError::Invalid(
            "absolute expiry must be a positive Unix-epoch millisecond timestamp",
        ));
    }
    Ok(value)
}

/// Opaque media session id: `[A-Za-z0-9_-]{16,128}`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct MediaSessionId(String);

impl MediaSessionId {
    pub fn parse(raw: &str) -> Result<Self, SignallingError> {
        if !(16..=128).contains(&raw.len()) {
            return Err(SignallingError::Invalid(
                "media session id must be 16..=128 bytes",
            ));
        }
        if !raw
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        {
            return Err(SignallingError::Invalid(
                "media session id must use [A-Za-z0-9_-]",
            ));
        }
        Ok(Self(raw.to_owned()))
    }

    /// Generates an opaque id from the protocol's UUIDv4-based opaque id
    /// generator. The media crate does not add a separate RNG dependency.
    #[must_use]
    pub fn new_random() -> Self {
        Self(format!("media-{}", LeaseId::new_random()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for MediaSessionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl TryFrom<String> for MediaSessionId {
    type Error = SignallingError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<MediaSessionId> for String {
    fn from(value: MediaSessionId) -> Self {
        value.0
    }
}

/// SDP body. Bounded to [`MAX_SDP_BYTES`] and required to start with a `v=`
/// line. `Debug`/`Display` expose only the byte length because SDP contains ICE
/// ufrag/password and DTLS fingerprint material.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Sdp(String);

impl Sdp {
    pub fn new(value: impl Into<String>) -> Result<Self, SignallingError> {
        let value = value.into();
        if value.is_empty() {
            return Err(SignallingError::Invalid("SDP must not be empty"));
        }
        if value.len() > MAX_SDP_BYTES {
            return Err(SignallingError::Invalid("SDP exceeds the size bound"));
        }
        if value.contains('\0') {
            return Err(SignallingError::Invalid("SDP must not contain NUL"));
        }
        let first_line = value.lines().next().unwrap_or_default();
        if !first_line.starts_with("v=") {
            return Err(SignallingError::Invalid("SDP must start with a v= line"));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn byte_len(&self) -> usize {
        self.0.len()
    }
}

impl fmt::Debug for Sdp {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Sdp(<redacted, {} bytes>)", self.0.len())
    }
}

impl fmt::Display for Sdp {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "<redacted SDP, {} bytes>", self.0.len())
    }
}

impl TryFrom<String> for Sdp {
    type Error = SignallingError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<Sdp> for String {
    fn from(value: Sdp) -> Self {
        value.0
    }
}

/// One trickled ICE candidate line. Bounded, printable ASCII and single-line.
///
/// `Debug`/`Display` expose only the byte length.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct IceCandidate(String);

impl IceCandidate {
    pub fn new(value: impl Into<String>) -> Result<Self, SignallingError> {
        let value = value.into();
        if value.is_empty() {
            return Err(SignallingError::Invalid("ICE candidate must not be empty"));
        }
        if value.len() > MAX_ICE_CANDIDATE_BYTES {
            return Err(SignallingError::Invalid(
                "ICE candidate exceeds the size bound",
            ));
        }
        if !value.bytes().all(|byte| (0x20..=0x7e).contains(&byte)) {
            return Err(SignallingError::Invalid(
                "ICE candidate must be single-line printable ASCII",
            ));
        }
        Ok(Self(value))
    }

    /// The candidate line for the ICE adapter. Never log this value.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn byte_len(&self) -> usize {
        self.0.len()
    }
}

impl fmt::Debug for IceCandidate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "IceCandidate(<redacted, {} bytes>)",
            self.0.len()
        )
    }
}

impl fmt::Display for IceCandidate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "<redacted ICE candidate, {} bytes>",
            self.0.len()
        )
    }
}

impl TryFrom<String> for IceCandidate {
    type Error = SignallingError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<IceCandidate> for String {
    fn from(value: IceCandidate) -> Self {
        value.0
    }
}

/// Validated STUN/TURN URL. Only `stun:`, `turn:` and `turns:` schemes are
/// accepted; userinfo, fragments and unknown query parameters are rejected.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct IceUrl(String);

impl IceUrl {
    pub fn parse(value: impl Into<String>) -> Result<Self, SignallingError> {
        let value = value.into();
        validate_ice_url(&value)?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for IceUrl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // A validated URL cannot carry userinfo; it is safe to print.
        write!(formatter, "IceUrl({})", self.0)
    }
}

impl fmt::Display for IceUrl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl TryFrom<String> for IceUrl {
    type Error = SignallingError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

impl From<IceUrl> for String {
    fn from(value: IceUrl) -> Self {
        value.0
    }
}

fn validate_ice_url(value: &str) -> Result<(), SignallingError> {
    if value.is_empty() || value.len() > MAX_ICE_URL_BYTES {
        return Err(SignallingError::Invalid(
            "ICE server URL length is out of range",
        ));
    }
    if !value.bytes().all(|byte| (0x21..=0x7e).contains(&byte)) {
        return Err(SignallingError::Invalid(
            "ICE server URL must be printable ASCII without spaces",
        ));
    }
    if value.contains('@') || value.contains('#') {
        return Err(SignallingError::Invalid(
            "ICE server URL must not carry userinfo or a fragment",
        ));
    }

    let (scheme, rest) = if let Some(rest) = value.strip_prefix("turns:") {
        ("turns", rest)
    } else if let Some(rest) = value.strip_prefix("turn:") {
        ("turn", rest)
    } else if let Some(rest) = value.strip_prefix("stun:") {
        ("stun", rest)
    } else {
        return Err(SignallingError::Invalid(
            "ICE server URL must use stun:, turn: or turns:",
        ));
    };
    if rest.is_empty() || rest.starts_with("//") {
        return Err(SignallingError::Invalid("ICE server URL has no host"));
    }

    let (authority, query) = match rest.split_once('?') {
        Some((authority, query)) => (authority, Some(query)),
        None => (rest, None),
    };
    match (scheme, query) {
        ("stun", Some(_)) => {
            return Err(SignallingError::Invalid(
                "stun: URLs do not accept a transport query",
            ));
        }
        ("turns", Some("transport=tcp")) | ("turn", Some("transport=tcp" | "transport=udp")) => {}
        (_, Some(_)) => {
            return Err(SignallingError::Invalid(
                "ICE server URL query must be transport=udp or transport=tcp",
            ));
        }
        (_, None) => {}
    }

    let (host, port) = split_authority(authority)?;
    if host.is_empty() {
        return Err(SignallingError::Invalid("ICE server URL has no host"));
    }
    if let Some(port) = port {
        if port == 0 {
            return Err(SignallingError::Invalid(
                "ICE server URL port must be 1..=65535",
            ));
        }
    }
    Ok(())
}

fn split_authority(authority: &str) -> Result<(&str, Option<u16>), SignallingError> {
    if authority.is_empty() {
        return Err(SignallingError::Invalid("ICE server URL has no host"));
    }
    if let Some(rest) = authority.strip_prefix('[') {
        let (host, tail) = rest
            .split_once(']')
            .ok_or(SignallingError::Invalid("unterminated IPv6 host"))?;
        if host.is_empty() || !host.bytes().all(|byte| (0x21..=0x7e).contains(&byte)) {
            return Err(SignallingError::Invalid("invalid IPv6 host"));
        }
        if tail.is_empty() {
            return Ok((host, None));
        }
        let port = tail
            .strip_prefix(':')
            .ok_or(SignallingError::Invalid("invalid IPv6 authority"))?;
        return Ok((host, Some(parse_port(port)?)));
    }
    match authority.split_once(':') {
        Some((host, port)) => {
            if host.is_empty()
                || !host
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b".-_".contains(&byte))
            {
                return Err(SignallingError::Invalid("invalid ICE server host"));
            }
            Ok((host, Some(parse_port(port)?)))
        }
        None => {
            if !authority
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b".-_".contains(&byte))
            {
                return Err(SignallingError::Invalid("invalid ICE server host"));
            }
            Ok((authority, None))
        }
    }
}

fn parse_port(value: &str) -> Result<u16, SignallingError> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(SignallingError::Invalid("invalid ICE server port"));
    }
    value
        .parse::<u16>()
        .map_err(|_| SignallingError::Invalid("ICE server port is out of range"))
}

/// TURN username or credential. Bounded printable ASCII; `Debug` and `Display`
/// always print `<redacted>`.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Credential(String);

impl Credential {
    pub fn new(value: impl Into<String>) -> Result<Self, SignallingError> {
        let value = value.into();
        if value.is_empty() || value.len() > MAX_CREDENTIAL_BYTES {
            return Err(SignallingError::Invalid(
                "TURN credential length is out of range",
            ));
        }
        if !value.bytes().all(|byte| (0x20..=0x7e).contains(&byte))
            || value.starts_with(' ')
            || value.ends_with(' ')
        {
            return Err(SignallingError::Invalid(
                "TURN credential must be printable ASCII without edge spaces",
            ));
        }
        Ok(Self(value))
    }

    /// The value a TURN adapter must pass to its ICE library. Never log it.
    #[must_use]
    pub fn expose_for_transport(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn byte_len(&self) -> usize {
        self.0.len()
    }
}

impl fmt::Debug for Credential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Credential(<redacted>)")
    }
}

impl fmt::Display for Credential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<redacted TURN credential>")
    }
}

impl TryFrom<String> for Credential {
    type Error = SignallingError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<Credential> for String {
    fn from(value: Credential) -> Self {
        value.0
    }
}

/// One STUN/TURN server entry.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IceServer {
    pub urls: Vec<IceUrl>,
    pub username: Option<Credential>,
    pub credential: Option<Credential>,
}

impl IceServer {
    pub fn new(
        urls: Vec<IceUrl>,
        username: Option<Credential>,
        credential: Option<Credential>,
    ) -> Result<Self, SignallingError> {
        let server = Self {
            urls,
            username,
            credential,
        };
        server.validate()?;
        Ok(server)
    }

    pub fn validate(&self) -> Result<(), SignallingError> {
        if self.urls.is_empty() || self.urls.len() > MAX_ICE_URLS_PER_SERVER {
            return Err(SignallingError::Invalid("ICE server must have 1..=8 URLs"));
        }
        for (index, url) in self.urls.iter().enumerate() {
            validate_ice_url(url.as_str())?;
            if self.urls[..index].contains(url) {
                return Err(SignallingError::Invalid("ICE server URLs must be unique"));
            }
        }
        let has_turn = self
            .urls
            .iter()
            .any(|url| !url.as_str().starts_with("stun:"));
        match (&self.username, &self.credential) {
            (Some(_), Some(_)) => {
                if !has_turn {
                    return Err(SignallingError::Invalid(
                        "STUN-only server must not carry TURN credentials",
                    ));
                }
            }
            (None, None) => {
                if has_turn {
                    return Err(SignallingError::Invalid(
                        "TURN server requires a username and credential pair",
                    ));
                }
            }
            _ => {
                return Err(SignallingError::Invalid(
                    "TURN username and credential must be supplied together",
                ));
            }
        }
        Ok(())
    }
}

impl fmt::Debug for IceServer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IceServer")
            .field("urls", &self.urls)
            .field("username", &self.username.as_ref().map(|_| "<redacted>"))
            .field(
                "credential",
                &self.credential.as_ref().map(|_| "<redacted>"),
            )
            .finish()
    }
}

impl fmt::Display for IceServer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "ICE server ({} URL(s), credentials {})",
            self.urls.len(),
            if self.credential.is_some() {
                "redacted"
            } else {
                "absent"
            }
        )
    }
}

/// One codec capability. Matching for negotiation is exact on all three
/// fields; the contract does not parse codec-specific `fmtp` lines.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodecDescriptor {
    pub name: CodecName,
    pub clock_rate: u32,
    pub channels: u8,
}

impl CodecDescriptor {
    pub fn new(
        name: impl Into<String>,
        clock_rate: u32,
        channels: u8,
    ) -> Result<Self, SignallingError> {
        let descriptor = Self {
            name: CodecName::new(name.into())?,
            clock_rate,
            channels,
        };
        descriptor.validate()?;
        Ok(descriptor)
    }

    pub fn validate(&self) -> Result<(), SignallingError> {
        self.name.validate()?;
        if self.clock_rate == 0 || self.clock_rate > 100_000 {
            return Err(SignallingError::Invalid(
                "codec clock rate must be 1..=100000",
            ));
        }
        if self.channels > 8 {
            return Err(SignallingError::Invalid("codec channels must be 0..=8"));
        }
        Ok(())
    }
}

/// Bounded codec name, printable ASCII without whitespace.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct CodecName(String);

impl CodecName {
    pub fn new(value: impl Into<String>) -> Result<Self, SignallingError> {
        let value = value.into();
        if value.is_empty() || value.len() > MAX_CODEC_NAME_BYTES {
            return Err(SignallingError::Invalid(
                "codec name length is out of range",
            ));
        }
        if !value.bytes().all(|byte| (0x21..=0x7e).contains(&byte)) {
            return Err(SignallingError::Invalid(
                "codec name must be printable ASCII without whitespace",
            ));
        }
        Ok(Self(value))
    }

    fn validate(&self) -> Result<(), SignallingError> {
        Self::new(self.0.clone()).map(|_| ())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CodecName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl TryFrom<String> for CodecName {
    type Error = SignallingError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<CodecName> for String {
    fn from(value: CodecName) -> Self {
        value.0
    }
}

/// Datagram or stream transport for encrypted media.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IceTransport {
    Udp,
    Tcp,
}

impl fmt::Display for IceTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Udp => "udp",
            Self::Tcp => "tcp",
        })
    }
}

/// One transport capability. v1 requires DTLS-SRTP for every transport; a
/// descriptor with `dtls_srtp: false` is invalid rather than silently accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransportCapability {
    pub transport: IceTransport,
    pub dtls_srtp: bool,
}

impl TransportCapability {
    pub fn new(transport: IceTransport, dtls_srtp: bool) -> Result<Self, SignallingError> {
        if !dtls_srtp {
            return Err(SignallingError::Invalid(
                "v1 media transport must be DTLS-SRTP protected",
            ));
        }
        Ok(Self {
            transport,
            dtls_srtp,
        })
    }
}

/// Codec, transport and ICE server capabilities exchanged in offer/answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaCapabilities {
    pub codecs: Vec<CodecDescriptor>,
    pub transports: Vec<TransportCapability>,
    /// Present in an offer (Host supplied). An answer must carry an empty list.
    pub ice_servers: Vec<IceServer>,
}

impl MediaCapabilities {
    pub fn new(
        codecs: Vec<CodecDescriptor>,
        transports: Vec<TransportCapability>,
        ice_servers: Vec<IceServer>,
    ) -> Result<Self, SignallingError> {
        let capabilities = Self {
            codecs,
            transports,
            ice_servers,
        };
        capabilities.validate()?;
        Ok(capabilities)
    }

    pub fn validate(&self) -> Result<(), SignallingError> {
        if self.codecs.is_empty() || self.codecs.len() > MAX_CAPABILITY_CODECS {
            return Err(SignallingError::Invalid(
                "capability descriptor must list 1..=16 codecs",
            ));
        }
        for (index, codec) in self.codecs.iter().enumerate() {
            codec.validate()?;
            if self.codecs[..index].contains(codec) {
                return Err(SignallingError::Invalid("capability codecs must be unique"));
            }
        }
        if self.transports.is_empty() || self.transports.len() > 2 {
            return Err(SignallingError::Invalid(
                "capability descriptor must list 1..=2 transports",
            ));
        }
        for (index, transport) in self.transports.iter().enumerate() {
            if !transport.dtls_srtp {
                return Err(SignallingError::Invalid(
                    "v1 media transport must be DTLS-SRTP protected",
                ));
            }
            if self.transports[..index]
                .iter()
                .any(|other| other.transport == transport.transport)
            {
                return Err(SignallingError::Invalid(
                    "capability transports must be unique",
                ));
            }
        }
        if self.ice_servers.len() > MAX_ICE_SERVERS {
            return Err(SignallingError::Invalid(
                "too many ICE servers in capability descriptor",
            ));
        }
        for server in &self.ice_servers {
            server.validate()?;
        }
        Ok(())
    }

    /// Intersection used by the viewer to answer an offer. The result carries
    /// no ICE servers: only the capture side supplies TURN configuration.
    pub fn intersect(&self, offer: &Self) -> Result<Self, SignallingError> {
        let codecs: Vec<_> = self
            .codecs
            .iter()
            .filter(|codec| offer.codecs.contains(codec))
            .cloned()
            .collect();
        if codecs.is_empty() {
            return Err(SignallingError::NoCommonCodec);
        }
        let transports: Vec<_> = self
            .transports
            .iter()
            .filter(|transport| offer.transports.contains(transport))
            .copied()
            .collect();
        if transports.is_empty() {
            return Err(SignallingError::NoCommonTransport);
        }
        Self::new(codecs, transports, Vec::new())
    }

    /// True when every codec/transport in `self` appears in `offer` and the
    /// answer carries no ICE servers.
    #[must_use]
    pub fn is_answer_subset_of(&self, offer: &Self) -> bool {
        self.ice_servers.is_empty()
            && !self.codecs.is_empty()
            && !self.transports.is_empty()
            && self.codecs.iter().all(|codec| offer.codecs.contains(codec))
            && self
                .transports
                .iter()
                .all(|transport| offer.transports.contains(transport))
    }

    #[must_use]
    pub fn without_ice_servers(&self) -> Self {
        let mut capabilities = self.clone();
        capabilities.ice_servers.clear();
        capabilities
    }
}

/// Validates a positive absolute expiry. Exposed for state machines that need
/// the same rule without constructing a full message.
pub fn validate_expiry(value: i64) -> Result<i64, SignallingError> {
    positive_expiry(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ice_urls_accept_only_the_declared_schemes() {
        for valid in [
            "stun:stun.example.invalid",
            "stun:stun.example.invalid:3478",
            "stun:[2001:db8::1]:3478",
            "turn:turn.example.invalid:3478?transport=udp",
            "turn:turn.example.invalid:3478?transport=tcp",
            "turns:turn.example.invalid:5349?transport=tcp",
            "turns:turn.example.invalid:5349",
        ] {
            assert!(IceUrl::parse(valid).is_ok(), "{valid}");
        }
        for invalid in [
            "http://example.invalid",
            "ws://example.invalid",
            "turn://example.invalid",
            "stun:user@example.invalid",
            "stun:example.invalid#fragment",
            "stun:example.invalid:0",
            "stun:example.invalid:99999",
            "stun:example.invalid?transport=udp",
            "turns:example.invalid?transport=udp",
            "turn:example.invalid?transport=sctp",
            "stun:",
            "stun: example.invalid",
        ] {
            assert!(IceUrl::parse(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn turn_credentials_are_redacted_and_require_turn() {
        let stun = IceUrl::parse("stun:stun.example.invalid").unwrap();
        let turn = IceUrl::parse("turn:turn.example.invalid:3478?transport=udp").unwrap();
        let username = Credential::new("user-1").unwrap();
        let credential = Credential::new("secret-value").unwrap();
        assert!(!format!("{username:?}").contains("user-1"));
        assert!(!format!("{credential}").contains("secret-value"));

        assert!(IceServer::new(
            vec![stun.clone()],
            Some(username.clone()),
            Some(credential.clone())
        )
        .is_err());
        assert!(IceServer::new(vec![turn.clone()], Some(username.clone()), None).is_err());
        assert!(IceServer::new(vec![turn.clone()], None, None).is_err());

        let server = IceServer::new(vec![turn], Some(username), Some(credential)).unwrap();
        let debug = format!("{server:?}");
        let display = format!("{server}");
        assert!(!debug.contains("user-1"));
        assert!(!debug.contains("secret-value"));
        assert!(!display.contains("secret-value"));
        assert!(debug.contains("redacted"));
    }

    #[test]
    fn sdp_and_ice_candidates_are_bounded_and_redacted() {
        let sdp = Sdp::new("v=0\r\no=- 0 0 IN IP4 127.0.0.1\r\n").unwrap();
        assert!(!format!("{sdp:?}").contains("127.0.0.1"));
        assert!(Sdp::new(format!("v=0\r\n{}", "a".repeat(MAX_SDP_BYTES))).is_err());
        assert!(Sdp::new("o=not-sdp\r\n").is_err());
        assert!(Sdp::new("v=0\r\n\0").is_err());

        let candidate = IceCandidate::new("candidate:1 1 udp 1 127.0.0.1 5000 typ host").unwrap();
        assert!(!format!("{candidate:?}").contains("127.0.0.1"));
        assert!(IceCandidate::new("candidate:1\nmalformed").is_err());
        assert!(IceCandidate::new("a".repeat(MAX_ICE_CANDIDATE_BYTES + 1)).is_err());
    }

    #[test]
    fn capabilities_require_secure_transport_and_intersect() {
        let codec = CodecDescriptor::new("h264", 90_000, 0).unwrap();
        assert!(TransportCapability::new(IceTransport::Udp, false).is_err());
        let udp = TransportCapability::new(IceTransport::Udp, true).unwrap();
        let tcp = TransportCapability::new(IceTransport::Tcp, true).unwrap();
        let offer =
            MediaCapabilities::new(vec![codec.clone()], vec![udp, tcp], Vec::new()).unwrap();
        let viewer = MediaCapabilities::new(
            vec![codec, CodecDescriptor::new("opus", 48_000, 2).unwrap()],
            vec![udp],
            Vec::new(),
        )
        .unwrap();
        let negotiated = viewer.intersect(&offer).unwrap();
        assert_eq!(negotiated.codecs.len(), 1);
        assert_eq!(negotiated.transports.len(), 1);
        assert!(negotiated.ice_servers.is_empty());
        assert!(negotiated.is_answer_subset_of(&offer));

        let no_common = MediaCapabilities::new(
            vec![CodecDescriptor::new("vp8", 90_000, 0).unwrap()],
            vec![udp],
            Vec::new(),
        )
        .unwrap();
        assert_eq!(
            no_common.intersect(&offer),
            Err(SignallingError::NoCommonCodec)
        );
    }
}
