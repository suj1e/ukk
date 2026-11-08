//! Wire-level frames: `ready` handshake and command responses.

use serde_json::Value;
use std::fmt;

/// The only protocol version negotiated by this client.
pub const PROTOCOL_VERSION: u32 = 1;

/// Advertised transport limits. The child is authoritative for its own frame
/// cap, clamped by the client's absolute ceiling.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadyInfo {
    pub protocol_version: u32,
    pub supported_protocol_versions: Vec<u32>,
    pub max_frame_bytes: usize,
    pub max_reassembled_frame_bytes: usize,
}

impl ReadyInfo {
    /// Parse and validate a `ready` frame. Any mismatch is a handshake
    /// failure; the caller kills the child.
    pub fn from_value(value: &Value) -> Result<Self, String> {
        if value.get("type").and_then(Value::as_str) != Some("ready") {
            return Err(format!(
                "expected a ready frame, got `{}`",
                value
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("missing type")
            ));
        }
        let protocol_version = value
            .get("protocolVersion")
            .and_then(Value::as_u64)
            .ok_or("ready frame is missing protocolVersion")?;
        if protocol_version != PROTOCOL_VERSION as u64 {
            return Err(format!(
                "unsupported omp protocol version {protocol_version}; this client speaks v{PROTOCOL_VERSION}"
            ));
        }
        let supported: Vec<u32> = value
            .get("supportedProtocolVersions")
            .and_then(Value::as_array)
            .map(|versions| {
                versions
                    .iter()
                    .filter_map(Value::as_u64)
                    .filter(|version| *version <= u32::MAX as u64)
                    .map(|version| version as u32)
                    .collect()
            })
            .unwrap_or_default();
        if !supported.is_empty() && !supported.contains(&PROTOCOL_VERSION) {
            return Err("omp advertises no supported protocol v1".into());
        }
        let max_frame_bytes = value
            .get("maxFrameBytes")
            .and_then(Value::as_u64)
            .ok_or("ready frame is missing maxFrameBytes")? as usize;
        if max_frame_bytes < 1024 {
            return Err(format!(
                "omp advertised an unusable maxFrameBytes ({max_frame_bytes})"
            ));
        }
        let max_reassembled_frame_bytes = value
            .get("maxReassembledFrameBytes")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        Ok(Self {
            protocol_version: protocol_version as u32,
            supported_protocol_versions: supported,
            max_frame_bytes,
            max_reassembled_frame_bytes,
        })
    }
}

/// One correlated command response.
#[derive(Clone, Debug, PartialEq)]
pub struct ResponseFrame {
    pub id: Option<String>,
    pub command: String,
    pub success: bool,
    pub error: Option<String>,
    pub code: Option<String>,
    pub data: Option<Value>,
}

impl ResponseFrame {
    pub fn from_value(value: &Value) -> Result<Self, String> {
        let command = value
            .get("command")
            .and_then(Value::as_str)
            .ok_or("response frame is missing command")?
            .to_owned();
        let success = value
            .get("success")
            .and_then(Value::as_bool)
            .ok_or("response frame is missing success")?;
        Ok(Self {
            id: value.get("id").and_then(Value::as_str).map(str::to_owned),
            command,
            success,
            error: value
                .get("error")
                .and_then(Value::as_str)
                .map(str::to_owned),
            code: value.get("code").and_then(Value::as_str).map(str::to_owned),
            data: value.get("data").cloned(),
        })
    }
}

impl fmt::Display for ResponseFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} response for `{}`",
            if self.success { "successful" } else { "failed" },
            self.command
        )
    }
}
