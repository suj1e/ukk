//! Versioned Remote Screen signalling envelope (v1).
//!
//! One JSON object per message:
//!
//! ```text
//! {"type":"offer","body":{"v":1,"media_session_id":"…","expires_at_ms":…,
//!                         "capabilities":{…},"sdp":"…"}}
//! ```
//!
//! The envelope carries **signalling only**: session description, trickled ICE
//! candidates, capability descriptors, ICE/TURN configuration and lifecycle
//! transitions. It has no variant and no field for a media frame, sample or
//! encoded image, so a frame cannot enter durable event history or a relay
//! payload through this type.
//!
//! Decoding rejects oversized input before parsing, accepts only the single
//! JSON-object form, rejects unknown fields, and re-validates every typed field.

use crate::capability::{validate_expiry, IceCandidate};
use crate::capability::{MediaCapabilities, MediaSessionId, Sdp};
use crate::error::SignallingError;
use crate::limits::{MAX_MEDIA_SESSION_MS, MAX_SIGNALLING_MESSAGE_BYTES, SIGNALLING_VERSION};
use serde::{Deserialize, Serialize};
use std::fmt;

/// Why a media session paused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PauseReason {
    /// The local screen locked or the device became unavailable.
    ScreenLocked,
    /// The viewer application moved to the background.
    AppBackgrounded,
    /// The user pressed pause.
    UserPaused,
    /// The control transport reconnected and the media path is suspended.
    Reconnecting,
}

impl fmt::Display for PauseReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::ScreenLocked => "screen_locked",
            Self::AppBackgrounded => "app_backgrounded",
            Self::UserPaused => "user_paused",
            Self::Reconnecting => "reconnecting",
        })
    }
}

/// Why a media session stopped. A stopped session is terminal for its id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    UserClosed,
    LocalRevoked,
    DeviceRevoked,
    LeaseExpired,
    ScreenLocked,
    Superseded,
    MediaFailure,
    ProtocolError,
}

impl fmt::Display for StopReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::UserClosed => "user_closed",
            Self::LocalRevoked => "local_revoked",
            Self::DeviceRevoked => "device_revoked",
            Self::LeaseExpired => "lease_expired",
            Self::ScreenLocked => "screen_locked",
            Self::Superseded => "superseded",
            Self::MediaFailure => "media_failure",
            Self::ProtocolError => "protocol_error",
        })
    }
}

/// Why a media session failed. A failed session is terminal for its id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureReason {
    Expired,
    IceFailed,
    TransportFailed,
    NoCommonCodec,
    NoCommonTransport,
    ProtocolError,
}

impl fmt::Display for FailureReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Expired => "expired",
            Self::IceFailed => "ice_failed",
            Self::TransportFailed => "transport_failed",
            Self::NoCommonCodec => "no_common_codec",
            Self::NoCommonTransport => "no_common_transport",
            Self::ProtocolError => "protocol_error",
        })
    }
}

/// One versioned media signalling message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    content = "body",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum SignallingMessage {
    Offer {
        v: u8,
        media_session_id: MediaSessionId,
        expires_at_ms: i64,
        capabilities: MediaCapabilities,
        sdp: Sdp,
    },
    Answer {
        v: u8,
        media_session_id: MediaSessionId,
        expires_at_ms: i64,
        capabilities: MediaCapabilities,
        sdp: Sdp,
    },
    IceCandidate {
        v: u8,
        media_session_id: MediaSessionId,
        candidate: IceCandidate,
    },
    IceComplete {
        v: u8,
        media_session_id: MediaSessionId,
    },
    Pause {
        v: u8,
        media_session_id: MediaSessionId,
        reason: PauseReason,
    },
    Resume {
        v: u8,
        media_session_id: MediaSessionId,
    },
    Stop {
        v: u8,
        media_session_id: MediaSessionId,
        reason: StopReason,
    },
    Failed {
        v: u8,
        media_session_id: MediaSessionId,
        reason: FailureReason,
    },
}

impl SignallingMessage {
    #[must_use]
    pub fn version(&self) -> u8 {
        match self {
            Self::Offer { v, .. }
            | Self::Answer { v, .. }
            | Self::IceCandidate { v, .. }
            | Self::IceComplete { v, .. }
            | Self::Pause { v, .. }
            | Self::Resume { v, .. }
            | Self::Stop { v, .. }
            | Self::Failed { v, .. } => *v,
        }
    }

    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Offer { .. } => "offer",
            Self::Answer { .. } => "answer",
            Self::IceCandidate { .. } => "ice_candidate",
            Self::IceComplete { .. } => "ice_complete",
            Self::Pause { .. } => "pause",
            Self::Resume { .. } => "resume",
            Self::Stop { .. } => "stop",
            Self::Failed { .. } => "failed",
        }
    }

    #[must_use]
    pub fn media_session_id(&self) -> &MediaSessionId {
        match self {
            Self::Offer {
                media_session_id, ..
            }
            | Self::Answer {
                media_session_id, ..
            }
            | Self::IceCandidate {
                media_session_id, ..
            }
            | Self::IceComplete {
                media_session_id, ..
            }
            | Self::Pause {
                media_session_id, ..
            }
            | Self::Resume {
                media_session_id, ..
            }
            | Self::Stop {
                media_session_id, ..
            }
            | Self::Failed {
                media_session_id, ..
            } => media_session_id,
        }
    }

    /// The absolute expiry carried by offer/answer, if this message carries one.
    #[must_use]
    pub fn expiry(&self) -> Option<i64> {
        match self {
            Self::Offer { expires_at_ms, .. } | Self::Answer { expires_at_ms, .. } => {
                Some(*expires_at_ms)
            }
            _ => None,
        }
    }

    /// Structural validation. Does not need a clock; timestamps are checked by
    /// the session state machine with an injected `now_ms`.
    pub fn validate(&self) -> Result<(), SignallingError> {
        if self.version() != SIGNALLING_VERSION {
            return Err(SignallingError::UnsupportedVersion {
                found: self.version(),
            });
        }
        match self {
            Self::Offer {
                expires_at_ms,
                capabilities,
                ..
            } => {
                validate_expiry(*expires_at_ms)?;
                capabilities.validate()?;
            }
            Self::Answer {
                expires_at_ms,
                capabilities,
                ..
            } => {
                validate_expiry(*expires_at_ms)?;
                capabilities.validate()?;
                if !capabilities.ice_servers.is_empty() {
                    return Err(SignallingError::Invalid(
                        "an answer must not carry ICE server configuration",
                    ));
                }
            }
            Self::IceCandidate { .. } => {}
            Self::IceComplete { .. } | Self::Pause { .. } | Self::Resume { .. } => {}
            Self::Stop { .. } | Self::Failed { .. } => {}
        }
        Ok(())
    }

    /// Structural validation plus the absolute-lifetime checks that need a
    /// clock. Offer/answer expiries must be in the future and at most
    /// [`MAX_MEDIA_SESSION_MS`] away.
    pub fn validate_at(&self, now_ms: i64) -> Result<(), SignallingError> {
        self.validate()?;
        if let Some(expires_at_ms) = self.expiry() {
            if now_ms >= expires_at_ms {
                return Err(SignallingError::Expired);
            }
            if matches!(self, Self::Offer { .. } | Self::Answer { .. })
                && expires_at_ms > now_ms.saturating_add(MAX_MEDIA_SESSION_MS)
            {
                return Err(SignallingError::Invalid(
                    "media session expiry exceeds the maximum lifetime",
                ));
            }
        }
        Ok(())
    }

    /// Encodes the message, enforcing the version and size bounds.
    pub fn encode(&self) -> Result<Vec<u8>, SignallingError> {
        self.validate()?;
        let encoded = serde_json::to_vec(self).map_err(|_| SignallingError::Malformed)?;
        if encoded.len() > MAX_SIGNALLING_MESSAGE_BYTES {
            return Err(SignallingError::Oversized {
                actual: encoded.len(),
                max: MAX_SIGNALLING_MESSAGE_BYTES,
            });
        }
        Ok(encoded)
    }

    /// Decodes one message. Oversized input is rejected before parsing, and
    /// only the single JSON-object form is accepted.
    pub fn decode(bytes: &[u8]) -> Result<Self, SignallingError> {
        if bytes.len() > MAX_SIGNALLING_MESSAGE_BYTES {
            return Err(SignallingError::Oversized {
                actual: bytes.len(),
                max: MAX_SIGNALLING_MESSAGE_BYTES,
            });
        }
        match bytes.iter().find(|byte| !byte.is_ascii_whitespace()) {
            Some(b'{') => {}
            _ => return Err(SignallingError::Malformed),
        }
        let message: Self =
            serde_json::from_slice(bytes).map_err(|_| SignallingError::Malformed)?;
        message.validate()?;
        Ok(message)
    }
}

impl fmt::Display for SignallingMessage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.expiry() {
            Some(expiry) => write!(
                formatter,
                "{} for {} (expires {expiry})",
                self.kind(),
                self.media_session_id()
            ),
            None => write!(formatter, "{} for {}", self.kind(), self.media_session_id()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::{CodecDescriptor, IceTransport, TransportCapability};
    use crate::limits::MAX_SDP_BYTES;

    fn session_id() -> MediaSessionId {
        MediaSessionId::parse("media-000000000001").unwrap()
    }

    fn capabilities() -> MediaCapabilities {
        MediaCapabilities::new(
            vec![CodecDescriptor::new("h264", 90_000, 0).unwrap()],
            vec![TransportCapability::new(IceTransport::Udp, true).unwrap()],
            Vec::new(),
        )
        .unwrap()
    }

    fn sdp() -> Sdp {
        Sdp::new("v=0\r\no=- 0 0 IN IP4 127.0.0.1\r\n").unwrap()
    }

    fn offer() -> SignallingMessage {
        SignallingMessage::Offer {
            v: SIGNALLING_VERSION,
            media_session_id: session_id(),
            expires_at_ms: 2_000,
            capabilities: capabilities(),
            sdp: sdp(),
        }
    }

    #[test]
    fn envelope_round_trips_and_rejects_unknown_fields() {
        let message = offer();
        let encoded = message.encode().unwrap();
        assert_eq!(SignallingMessage::decode(&encoded).unwrap(), message);

        let mut value: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
        value["extra"] = serde_json::json!(true);
        assert_eq!(
            SignallingMessage::decode(value.to_string().as_bytes()),
            Err(SignallingError::Malformed)
        );

        let mut value: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
        value["body"]["extra"] = serde_json::json!(true);
        assert_eq!(
            SignallingMessage::decode(value.to_string().as_bytes()),
            Err(SignallingError::Malformed)
        );

        let mut trailing = encoded.clone();
        trailing.push(b'x');
        assert_eq!(
            SignallingMessage::decode(&trailing),
            Err(SignallingError::Malformed)
        );

        let array = br#"["offer",{"v":1}]"#;
        assert_eq!(
            SignallingMessage::decode(array),
            Err(SignallingError::Malformed)
        );
    }

    #[test]
    fn envelope_rejects_versions_and_oversized_input() {
        let mut value: serde_json::Value =
            serde_json::from_slice(&offer().encode().unwrap()).unwrap();
        value["body"]["v"] = serde_json::json!(2);
        assert_eq!(
            SignallingMessage::decode(value.to_string().as_bytes()),
            Err(SignallingError::UnsupportedVersion { found: 2 })
        );

        assert_eq!(
            SignallingMessage::decode(&vec![b'{'; MAX_SIGNALLING_MESSAGE_BYTES + 1]),
            Err(SignallingError::Oversized {
                actual: MAX_SIGNALLING_MESSAGE_BYTES + 1,
                max: MAX_SIGNALLING_MESSAGE_BYTES,
            })
        );

        let huge_sdp = format!("v=0\r\n{}", "a".repeat(MAX_SDP_BYTES));
        assert!(Sdp::new(huge_sdp).is_err());
    }

    #[test]
    fn answer_cannot_supply_ice_servers_and_no_frame_field_exists() {
        let answer = SignallingMessage::Answer {
            v: SIGNALLING_VERSION,
            media_session_id: session_id(),
            expires_at_ms: 2_000,
            capabilities: capabilities(),
            sdp: sdp(),
        };
        let raw = serde_json::json!({
            "type": "answer",
            "body": {
                "v": 1,
                "media_session_id": session_id().as_str(),
                "expires_at_ms": 2_000,
                "capabilities": {
                    "codecs": [{"name": "h264", "clock_rate": 90_000, "channels": 0}],
                    "transports": [{"transport": "udp", "dtls_srtp": true}],
                    "ice_servers": [{"urls": ["stun:stun.example.invalid"], "username": null, "credential": null}]
                },
                "sdp": "v=0\r\n"
            }
        });
        assert_eq!(
            SignallingMessage::decode(raw.to_string().as_bytes()),
            Err(SignallingError::Invalid(
                "an answer must not carry ICE server configuration"
            ))
        );
        assert!(answer.validate().is_ok());

        let framed = serde_json::json!({
            "type": "stop",
            "body": {
                "v": 1,
                "media_session_id": session_id().as_str(),
                "reason": "user_closed",
                "frame": "AAAA"
            }
        });
        assert_eq!(
            SignallingMessage::decode(framed.to_string().as_bytes()),
            Err(SignallingError::Malformed)
        );
        let frame_type = serde_json::json!({
            "type": "media_frame",
            "body": {"v": 1, "media_session_id": session_id().as_str(), "data": "AAAA"}
        });
        assert_eq!(
            SignallingMessage::decode(frame_type.to_string().as_bytes()),
            Err(SignallingError::Malformed)
        );
    }
}
