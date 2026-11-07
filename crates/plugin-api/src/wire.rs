//! Helper stdio protocol, version 1.
//!
//! One JSON object per line over the helper's stdin/stdout. The host always
//! initiates; a helper may only answer a request or emit a well-known event.
//! Frames are bounded by [`crate::limits::MAX_FRAME_BYTES`] and every request
//! carries an id that must be echoed so unsolicited replies are detectable.
//!
//! This is a deliberately small protocol: it carries no capability grants, no
//! approval decisions and no process-control instructions. Anything a helper
//! reports is data to validate, never authority.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::HELPER_PROTOCOL_VERSION;

/// Request method: first message after spawn, reports helper identity.
pub const METHOD_HELLO: &str = "plugin.hello";
/// Request method: liveness probe with a short timeout.
pub const METHOD_HEALTH: &str = "plugin.health";
/// Request method: run one declared command.
pub const METHOD_INVOKE: &str = "command.invoke";
/// Request method: graceful stop; host kills after the grace period.
pub const METHOD_SHUTDOWN: &str = "plugin.shutdown";
/// Notification (no response): one subscribed host event.
pub const METHOD_EVENT: &str = "event.deliver";

/// Host → helper request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestFrame {
    pub id: u64,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

impl RequestFrame {
    pub fn hello(id: u64, plugin_id: &str) -> Self {
        Self {
            id,
            method: METHOD_HELLO.into(),
            params: json!({ "protocol": HELPER_PROTOCOL_VERSION, "plugin_id": plugin_id }),
        }
    }

    pub fn health(id: u64) -> Self {
        Self {
            id,
            method: METHOD_HEALTH.into(),
            params: Value::Null,
        }
    }

    pub fn invoke(id: u64, command: &str, session_id: Option<&str>, args: Value) -> Self {
        Self {
            id,
            method: METHOD_INVOKE.into(),
            params: json!({ "command": command, "session_id": session_id, "args": args }),
        }
    }

    pub fn shutdown(id: u64) -> Self {
        Self {
            id,
            method: METHOD_SHUTDOWN.into(),
            params: Value::Null,
        }
    }
}

/// Helper → host answer to one request id.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResponseFrame {
    pub id: u64,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<WireError>,
}

impl ResponseFrame {
    pub fn ok(id: u64, result: Value) -> Self {
        Self {
            id,
            ok: true,
            result: Some(result),
            error: None,
        }
    }

    pub fn err(id: u64, code: &str, message: &str) -> Self {
        Self {
            id,
            ok: false,
            result: None,
            error: Some(WireError {
                code: code.to_owned(),
                message: message.to_owned(),
            }),
        }
    }
}

/// Structured failure reported by a helper.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireError {
    pub code: String,
    pub message: String,
}

/// Host → helper notification; never answered.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventFrame {
    pub event: String,
    #[serde(default)]
    pub params: Value,
}

impl EventFrame {
    pub fn deliver(kind: &str, payload: Value) -> Self {
        Self {
            event: METHOD_EVENT.into(),
            params: json!({ "kind": kind, "payload": payload }),
        }
    }
}

/// A frame read from a helper.
#[derive(Debug, Clone, PartialEq)]
pub enum IncomingFrame {
    Response(ResponseFrame),
    Event(EventFrame),
}

impl<'de> Deserialize<'de> for IncomingFrame {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // Classify on the authoritative shape instead of trusting field order:
        // responses carry `id`, events carry `event`.
        let value = Value::deserialize(deserializer)?;
        let is_event = value.get("event").is_some() && value.get("id").is_none();
        if is_event {
            serde_json::from_value(value)
                .map(IncomingFrame::Event)
                .map_err(serde::de::Error::custom)
        } else {
            serde_json::from_value(value)
                .map(IncomingFrame::Response)
                .map_err(serde::de::Error::custom)
        }
    }
}

impl Serialize for IncomingFrame {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Response(frame) => frame.serialize(serializer),
            Self::Event(frame) => frame.serialize(serializer),
        }
    }
}

/// Result body a helper must return from `plugin.hello`.
///
/// The reported contribution ids are cross-checked against the manifest: a
/// helper can confirm what it implements but can never add to it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HelloResult {
    pub protocol: u32,
    pub plugin_id: String,
    #[serde(default)]
    pub contributions: HelloContributions,
}

/// Contribution ids a helper claims to implement.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HelloContributions {
    #[serde(default)]
    pub commands: Vec<String>,
    #[serde(default)]
    pub views: Vec<String>,
}

impl HelloResult {
    pub fn new(plugin_id: &str) -> Self {
        Self {
            protocol: HELPER_PROTOCOL_VERSION,
            plugin_id: plugin_id.to_owned(),
            contributions: HelloContributions::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_frames_round_trip() {
        let request = RequestFrame::invoke(7, "sample.session.ping", Some("session-1"), json!({}));
        let encoded = serde_json::to_string(&request).unwrap();
        let decoded: RequestFrame = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, request);
        assert_eq!(decoded.params["command"], "sample.session.ping");
        assert_eq!(decoded.params["session_id"], "session-1");
    }

    #[test]
    fn incoming_frames_are_classified_by_shape() {
        let response: IncomingFrame =
            serde_json::from_str(r#"{"id":1,"ok":true,"result":{"a":1}}"#).unwrap();
        assert!(matches!(response, IncomingFrame::Response(_)));

        let event: IncomingFrame =
            serde_json::from_str(r#"{"event":"event.deliver","params":{}}"#).unwrap();
        assert!(matches!(event, IncomingFrame::Event(_)));
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let outcome = serde_json::from_str::<RequestFrame>(
            r#"{"id":1,"method":"plugin.hello","params":{"protocol":1,"plugin_id":"a.b"},"grant":"screen.view"}"#,
        );
        assert!(outcome.is_err());
    }

    #[test]
    fn hello_result_requires_identity() {
        let outcome = serde_json::from_str::<HelloResult>(r#"{"protocol":1,"plugin_id":"a.b"}"#);
        assert!(outcome.is_ok());
        assert!(serde_json::from_str::<HelloResult>(r#"{"protocol":1}"#).is_err());
    }
}
