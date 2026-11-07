//! Claude Code `--output-format stream-json` fixture adapter.
//!
//! Wire sources:
//! - <https://code.claude.com/docs/en/headless> (stream-json lines)
//! - <https://code.claude.com/docs/en/agent-sdk/typescript> (`SDKMessage`
//!   shapes, `canUseTool` and permission-denied semantics)
//!
//! This adapter consumes stdout lines. It does not implement the in-process
//! SDK control channel, so approval prompts are explicitly Unsupported.

use crate::adapter::{AdapterConfig, ParseOutcome, ProviderAdapter};
use crate::capability::{CapabilityDescriptor, ProviderKind};
use crate::contract::{
    AdapterEvent, BoundedText, EventPayload, Message, MessagePart, Notice, NoticeSeverity,
    PermissionDenied, Progress, ProgressKind, Provenance, ProviderError, Role, SessionInfo,
    ToolCall, ToolInput, ToolKind, ToolStatus, TurnInfo, TurnStatus, UnsupportedAction,
    UnsupportedScope,
};
use crate::error::AdapterError;
use crate::ids::{ItemId, SessionId, StreamId, TurnId};
use crate::util::{
    bounded_json, checked_opt_id, optional_str, redact_bounded, required_str, sanitize_action_name,
    stable_fingerprint,
};
use serde_json::Value;
use std::collections::HashMap;

#[derive(Default)]
struct StreamState {
    message_id: Option<String>,
    tool_blocks: HashMap<u64, ItemId>,
}

pub struct ClaudeStreamJsonAdapter {
    config: AdapterConfig,
    descriptor: CapabilityDescriptor,
    streams: HashMap<StreamId, StreamState>,
}

impl ClaudeStreamJsonAdapter {
    pub fn new(config: AdapterConfig, descriptor: CapabilityDescriptor) -> Self {
        Self {
            config,
            descriptor,
            streams: HashMap::new(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn provenance(
        &self,
        native_type: &str,
        native_id: Option<String>,
        parent_native_id: Option<String>,
        session_id: Option<&str>,
        turn_id: Option<&str>,
        item_id: Option<&str>,
        stream: &StreamId,
        sequence: u64,
    ) -> Result<Provenance, AdapterError> {
        let native_id = match native_id {
            Some(id) => {
                self.config.limits.check_id("native_id", &id)?;
                Some(id)
            }
            None => None,
        };
        let parent_native_id = match parent_native_id {
            Some(id) => {
                self.config.limits.check_id("parent_native_id", &id)?;
                Some(id)
            }
            None => None,
        };
        Ok(Provenance {
            native_type: sanitize_action_name(native_type),
            native_id,
            parent_native_id,
            session_id: checked_opt_id(&self.config, "session_id", session_id)?
                .map(SessionId::from),
            turn_id: checked_opt_id(&self.config, "turn_id", turn_id)?.map(TurnId::from),
            item_id: checked_opt_id(&self.config, "item_id", item_id)?.map(ItemId::from),
            stream_id: stream.clone(),
            sequence,
        })
    }

    fn require_session<'a>(
        &self,
        value: &'a Value,
        native_type: &str,
    ) -> Result<&'a str, AdapterError> {
        let session = required_str(value, "session_id", native_type)?;
        self.config.limits.check_id("session_id", session)?;
        Ok(session)
    }

    fn event_uuid(value: &Value) -> Option<String> {
        value.get("uuid").and_then(Value::as_str).map(str::to_owned)
    }

    fn parse_value(
        &mut self,
        value: &Value,
        stream: &StreamId,
        sequence: u64,
    ) -> Result<Vec<AdapterEvent>, AdapterError> {
        let message_type = required_str(value, "type", "claude")?;
        match message_type {
            "system" => self.parse_system(value, stream, sequence),
            "assistant" => self.parse_assistant(value, stream, sequence),
            "user" => self.parse_user(value, stream, sequence),
            "result" => self.parse_result(value, stream, sequence),
            "stream_event" => self.parse_stream_event(value, stream, sequence),
            "tool_progress" => self.parse_tool_progress(value, stream, sequence),
            other => Ok(vec![self.unsupported_event(
                other,
                "stream-json message type not modelled by this contract version",
                UnsupportedScope::Action,
                value,
                stream,
                sequence,
            )?]),
        }
    }

    fn parse_system(
        &self,
        value: &Value,
        stream: &StreamId,
        sequence: u64,
    ) -> Result<Vec<AdapterEvent>, AdapterError> {
        let subtype = value
            .get("subtype")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let session_id = self.require_session(value, "system")?;
        match subtype {
            "init" => {
                let info = SessionInfo {
                    id: SessionId::from(session_id),
                    native_session_id: Some(session_id.to_owned()),
                    model: optional_str(value, "model").map(str::to_owned),
                    cwd: optional_str(value, "cwd").map(|cwd| {
                        redact_bounded(&self.config, cwd, self.config.limits.max_text_bytes)
                    }),
                    tools: value
                        .get("tools")
                        .and_then(Value::as_array)
                        .map(|tools| {
                            tools
                                .iter()
                                .take(256)
                                .filter_map(Value::as_str)
                                .map(sanitize_action_name)
                                .collect()
                        })
                        .unwrap_or_default(),
                };
                let resumed = self.config.resume_session.as_ref() == Some(&info.id);
                let provenance = self.provenance(
                    "system/init",
                    Some(session_id.to_owned()),
                    None,
                    Some(session_id),
                    None,
                    None,
                    stream,
                    sequence,
                )?;
                let payload = if resumed {
                    EventPayload::SessionResumed {
                        session: info.clone(),
                        previous_native_id: info.id.clone(),
                    }
                } else {
                    EventPayload::SessionStarted {
                        session: info,
                        resumed_from: None,
                    }
                };
                Ok(vec![AdapterEvent::new(
                    ProviderKind::ClaudeStreamJson,
                    provenance,
                    payload,
                )])
            }
            "api_retry" => {
                let text = optional_str(value, "error")
                    .map(|category| BoundedText::from_text(sanitize_action_name(category)));
                let provenance = self.provenance(
                    "system/api_retry",
                    Self::event_uuid(value),
                    None,
                    Some(session_id),
                    None,
                    None,
                    stream,
                    sequence,
                )?;
                Ok(vec![AdapterEvent::new(
                    ProviderKind::ClaudeStreamJson,
                    provenance,
                    EventPayload::Progress {
                        progress: Progress {
                            kind: ProgressKind::Retry,
                            text,
                            tool_call_id: None,
                            attempt: value
                                .get("attempt")
                                .and_then(Value::as_u64)
                                .and_then(|attempt| u32::try_from(attempt).ok()),
                            max_attempts: value
                                .get("max_retries")
                                .and_then(Value::as_u64)
                                .and_then(|attempt| u32::try_from(attempt).ok()),
                            retry_delay_ms: value.get("retry_delay_ms").and_then(Value::as_u64),
                            elapsed_seconds: None,
                            heartbeat: None,
                        },
                    },
                )])
            }
            "permission_denied" => {
                let tool_use_id = required_str(value, "tool_use_id", "system/permission_denied")?;
                self.config.limits.check_id("tool_use_id", tool_use_id)?;
                let tool_name = optional_str(value, "tool_name").unwrap_or("");
                let message = optional_str(value, "message").unwrap_or("");
                let message =
                    redact_bounded(&self.config, message, self.config.limits.max_text_bytes);
                let provenance = self.provenance(
                    "system/permission_denied",
                    Self::event_uuid(value),
                    None,
                    Some(session_id),
                    None,
                    Some(tool_use_id),
                    stream,
                    sequence,
                )?;
                Ok(vec![AdapterEvent::new(
                    ProviderKind::ClaudeStreamJson,
                    provenance,
                    EventPayload::PermissionDenied {
                        denial: PermissionDenied {
                            tool_call_id: ItemId::from(tool_use_id),
                            tool_name: sanitize_action_name(tool_name),
                            message,
                            reason: optional_str(value, "decision_reason").map(|reason| {
                                redact_bounded(
                                    &self.config,
                                    reason,
                                    self.config.limits.max_text_bytes,
                                )
                                .text
                            }),
                            reason_type: optional_str(value, "decision_reason_type")
                                .map(sanitize_action_name),
                        },
                    },
                )])
            }
            "informational" => {
                let content = optional_str(value, "content").unwrap_or("");
                let message =
                    redact_bounded(&self.config, content, self.config.limits.max_text_bytes);
                let severity = match optional_str(value, "level") {
                    Some("warning") => NoticeSeverity::Warning,
                    _ => NoticeSeverity::Info,
                };
                let provenance = self.provenance(
                    "system/informational",
                    Self::event_uuid(value),
                    None,
                    Some(session_id),
                    None,
                    None,
                    stream,
                    sequence,
                )?;
                Ok(vec![AdapterEvent::new(
                    ProviderKind::ClaudeStreamJson,
                    provenance,
                    EventPayload::Notice {
                        notice: Notice {
                            severity,
                            message,
                            native_type: "system/informational".to_owned(),
                        },
                    },
                )])
            }
            "compact_boundary" => {
                let provenance = self.provenance(
                    "system/compact_boundary",
                    Self::event_uuid(value),
                    None,
                    Some(session_id),
                    None,
                    None,
                    stream,
                    sequence,
                )?;
                Ok(vec![AdapterEvent::new(
                    ProviderKind::ClaudeStreamJson,
                    provenance,
                    EventPayload::Progress {
                        progress: Progress {
                            kind: ProgressKind::Status,
                            text: Some(BoundedText::from_text("compact_boundary")),
                            tool_call_id: None,
                            attempt: None,
                            max_attempts: None,
                            retry_delay_ms: None,
                            elapsed_seconds: None,
                            heartbeat: None,
                        },
                    },
                )])
            }
            other => Ok(vec![self.unsupported_event(
                &format!("system/{other}"),
                "system subtype not modelled by this contract version",
                UnsupportedScope::Action,
                value,
                stream,
                sequence,
            )?]),
        }
    }

    fn parse_assistant(
        &self,
        value: &Value,
        stream: &StreamId,
        sequence: u64,
    ) -> Result<Vec<AdapterEvent>, AdapterError> {
        let session_id = self.require_session(value, "assistant")?;
        let message = value
            .get("message")
            .ok_or_else(|| AdapterError::MissingField {
                native_type: "assistant".to_owned(),
                field: "message",
            })?;
        let message_id = required_str(message, "id", "assistant")?;
        self.config.limits.check_id("message_id", message_id)?;
        let native_id = Self::event_uuid(value).or_else(|| {
            if message_id.is_empty() {
                None
            } else {
                let serialized = message
                    .get("content")
                    .map(ToString::to_string)
                    .unwrap_or_default();
                Some(format!(
                    "{message_id}#{:x}",
                    stable_fingerprint(&serialized)
                ))
            }
        });
        let parent = optional_str(value, "parent_tool_use_id");
        let mut events = Vec::new();
        let mut parts = Vec::new();
        let mut truncated = false;
        if let Some(content) = message.get("content") {
            match content {
                Value::String(text) => {
                    let bounded =
                        redact_bounded(&self.config, text, self.config.limits.max_text_bytes);
                    truncated |= bounded.truncated;
                    parts.push(MessagePart::Text { text: bounded.text });
                }
                Value::Array(blocks) => {
                    for block in blocks {
                        let block_type = block
                            .get("type")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown");
                        match block_type {
                            "text" => {
                                let text = block.get("text").and_then(Value::as_str).unwrap_or("");
                                let bounded = redact_bounded(
                                    &self.config,
                                    text,
                                    self.config.limits.max_text_bytes,
                                );
                                truncated |= bounded.truncated;
                                parts.push(MessagePart::Text { text: bounded.text });
                            }
                            "thinking" => {
                                let text =
                                    block.get("thinking").and_then(Value::as_str).unwrap_or("");
                                let bounded = redact_bounded(
                                    &self.config,
                                    text,
                                    self.config.limits.max_text_bytes,
                                );
                                let provenance = self.provenance(
                                    "assistant/thinking",
                                    native_id.clone(),
                                    parent.map(str::to_owned),
                                    Some(session_id),
                                    None,
                                    None,
                                    stream,
                                    sequence,
                                )?;
                                events.push(AdapterEvent::new(
                                    ProviderKind::ClaudeStreamJson,
                                    provenance,
                                    EventPayload::Progress {
                                        progress: Progress {
                                            kind: ProgressKind::ReasoningRaw,
                                            text: Some(bounded),
                                            tool_call_id: None,
                                            attempt: None,
                                            max_attempts: None,
                                            retry_delay_ms: None,
                                            elapsed_seconds: None,
                                            heartbeat: None,
                                        },
                                    },
                                ));
                            }
                            "tool_use" => {
                                let tool_id = required_str(block, "id", "assistant/tool_use")?;
                                self.config.limits.check_id("tool_use_id", tool_id)?;
                                let name = optional_str(block, "name").unwrap_or("");
                                let arguments = block
                                    .get("input")
                                    .map(|input| {
                                        bounded_json(
                                            &self.config,
                                            input,
                                            self.config.limits.max_input_bytes,
                                        )
                                    })
                                    .unwrap_or_else(BoundedText::empty);
                                let provenance = self.provenance(
                                    "assistant/tool_use",
                                    Some(tool_id.to_owned()),
                                    parent.map(str::to_owned),
                                    Some(session_id),
                                    None,
                                    Some(tool_id),
                                    stream,
                                    sequence,
                                )?;
                                events.push(AdapterEvent::new(
                                    ProviderKind::ClaudeStreamJson,
                                    provenance,
                                    EventPayload::ToolCallStarted {
                                        call: ToolCall {
                                            id: ItemId::from(tool_id),
                                            name: Some(sanitize_action_name(name)),
                                            kind: ToolKind::ProviderNamed,
                                            status: ToolStatus::InProgress,
                                            input: Some(ToolInput::Named {
                                                name: sanitize_action_name(name),
                                                arguments,
                                            }),
                                            output: None,
                                            exit_code: None,
                                            duration_ms: None,
                                        },
                                    },
                                ));
                            }
                            "tool_result" => {
                                events.extend(self.tool_result_events(
                                    block,
                                    session_id,
                                    parent.map(str::to_owned),
                                    stream,
                                    sequence,
                                )?);
                            }
                            other => {
                                events.push(self.unsupported_event(
                                    &format!("content_block:{other}"),
                                    "content block not modelled by this contract version",
                                    UnsupportedScope::Action,
                                    value,
                                    stream,
                                    sequence,
                                )?);
                            }
                        }
                    }
                }
                _ => {
                    return Err(AdapterError::InvalidField {
                        native_type: "assistant".to_owned(),
                        field: "message.content",
                        detail: "expected string or array".to_owned(),
                    });
                }
            }
        }
        let message = Message {
            id: ItemId::from(message_id),
            role: Role::Assistant,
            parts,
            phase: None,
            complete: true,
            truncated,
        };
        let provenance = self.provenance(
            "assistant",
            native_id,
            parent.map(str::to_owned),
            Some(session_id),
            None,
            Some(message_id),
            stream,
            sequence,
        )?;
        events.push(AdapterEvent::new(
            ProviderKind::ClaudeStreamJson,
            provenance,
            EventPayload::MessageCompleted { message },
        ));

        if let Some(error) = value.get("error").and_then(Value::as_str) {
            let bounded = redact_bounded(&self.config, error, self.config.limits.max_text_bytes);
            let provenance = self.provenance(
                "assistant/error",
                None,
                None,
                Some(session_id),
                None,
                None,
                stream,
                sequence,
            )?;
            events.push(AdapterEvent::new(
                ProviderKind::ClaudeStreamJson,
                provenance,
                EventPayload::Notice {
                    notice: Notice {
                        severity: NoticeSeverity::Error,
                        message: bounded,
                        native_type: "assistant/error".to_owned(),
                    },
                },
            ));
        }
        if value.get("aborted").and_then(Value::as_bool) == Some(true) {
            let provenance = self.provenance(
                "assistant/aborted",
                None,
                None,
                Some(session_id),
                None,
                None,
                stream,
                sequence,
            )?;
            events.push(AdapterEvent::new(
                ProviderKind::ClaudeStreamJson,
                provenance,
                EventPayload::Notice {
                    notice: Notice {
                        severity: NoticeSeverity::Warning,
                        message: BoundedText::from_text("assistant message was aborted"),
                        native_type: "assistant/aborted".to_owned(),
                    },
                },
            ));
        }
        Ok(events)
    }

    fn parse_user(
        &self,
        value: &Value,
        stream: &StreamId,
        sequence: u64,
    ) -> Result<Vec<AdapterEvent>, AdapterError> {
        let session_id = self.require_session(value, "user")?;
        let message = value
            .get("message")
            .ok_or_else(|| AdapterError::MissingField {
                native_type: "user".to_owned(),
                field: "message",
            })?;
        let message_id = required_str(message, "id", "user")?;
        self.config.limits.check_id("message_id", message_id)?;
        let native_id = Self::event_uuid(value);
        let parent = optional_str(value, "parent_tool_use_id");
        let mut events = Vec::new();
        let mut parts = Vec::new();
        let mut truncated = false;
        let content = message.get("content");
        let blocks: Vec<&Value> = match content {
            Some(Value::String(text)) => {
                let bounded = redact_bounded(&self.config, text, self.config.limits.max_text_bytes);
                truncated |= bounded.truncated;
                parts.push(MessagePart::Text { text: bounded.text });
                Vec::new()
            }
            Some(Value::Array(blocks)) => blocks.iter().collect(),
            None => Vec::new(),
            _ => {
                return Err(AdapterError::InvalidField {
                    native_type: "user".to_owned(),
                    field: "message.content",
                    detail: "expected string or array".to_owned(),
                });
            }
        };
        for block in blocks {
            let block_type = block
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            match block_type {
                "tool_result" => {
                    events.extend(self.tool_result_events(
                        block,
                        session_id,
                        parent.map(str::to_owned),
                        stream,
                        sequence,
                    )?);
                }
                "text" => {
                    let text = block.get("text").and_then(Value::as_str).unwrap_or("");
                    let bounded =
                        redact_bounded(&self.config, text, self.config.limits.max_text_bytes);
                    truncated |= bounded.truncated;
                    parts.push(MessagePart::Text { text: bounded.text });
                }
                "image" | "document" => {
                    let reference = block
                        .get("source")
                        .and_then(|source| source.get("data").or_else(|| source.get("url")))
                        .and_then(Value::as_str)
                        .map(|reference| {
                            let bounded = redact_bounded(
                                &self.config,
                                reference,
                                self.config.limits.max_text_bytes,
                            );
                            truncated |= bounded.truncated;
                            bounded.text
                        });
                    parts.push(MessagePart::Attachment {
                        label: sanitize_action_name(block_type),
                        reference,
                    });
                }
                other => {
                    events.push(self.unsupported_event(
                        &format!("content_block:{other}"),
                        "content block not modelled by this contract version",
                        UnsupportedScope::Action,
                        value,
                        stream,
                        sequence,
                    )?);
                }
            }
        }
        if !parts.is_empty() {
            let message = Message {
                id: ItemId::from(message_id),
                role: Role::User,
                parts,
                phase: None,
                complete: true,
                truncated,
            };
            let provenance = self.provenance(
                "user",
                native_id,
                parent.map(str::to_owned),
                Some(session_id),
                None,
                Some(message_id),
                stream,
                sequence,
            )?;
            events.push(AdapterEvent::new(
                ProviderKind::ClaudeStreamJson,
                provenance,
                EventPayload::MessageCompleted { message },
            ));
        }
        Ok(events)
    }

    fn tool_result_events(
        &self,
        block: &Value,
        session_id: &str,
        parent_native_id: Option<String>,
        stream: &StreamId,
        sequence: u64,
    ) -> Result<Vec<AdapterEvent>, AdapterError> {
        let tool_use_id = required_str(block, "tool_use_id", "user/tool_result")?;
        self.config.limits.check_id("tool_use_id", tool_use_id)?;
        let output = match block.get("content") {
            Some(Value::String(text)) => {
                redact_bounded(&self.config, text, self.config.limits.max_output_bytes)
            }
            Some(Value::Array(blocks)) => {
                let texts: Vec<&str> = blocks
                    .iter()
                    .filter_map(|entry| {
                        if entry.get("type").and_then(Value::as_str) == Some("text") {
                            entry.get("text").and_then(Value::as_str)
                        } else {
                            None
                        }
                    })
                    .collect();
                if texts.is_empty() {
                    bounded_json(&self.config, block, self.config.limits.max_output_bytes)
                } else {
                    redact_bounded(
                        &self.config,
                        &texts.join("\n"),
                        self.config.limits.max_output_bytes,
                    )
                }
            }
            Some(other) => bounded_json(&self.config, other, self.config.limits.max_output_bytes),
            None => BoundedText::empty(),
        };
        let status = if block.get("is_error").and_then(Value::as_bool) == Some(true) {
            ToolStatus::Failed
        } else {
            ToolStatus::Completed
        };
        let provenance = self.provenance(
            "user/tool_result",
            Some(tool_use_id.to_owned()),
            parent_native_id,
            Some(session_id),
            None,
            Some(tool_use_id),
            stream,
            sequence,
        )?;
        Ok(vec![AdapterEvent::new(
            ProviderKind::ClaudeStreamJson,
            provenance,
            EventPayload::ToolCallCompleted {
                call: ToolCall {
                    id: ItemId::from(tool_use_id),
                    name: None,
                    kind: ToolKind::Unknown,
                    status,
                    input: None,
                    output: Some(output),
                    exit_code: None,
                    duration_ms: None,
                },
            },
        )])
    }

    fn parse_result(
        &self,
        value: &Value,
        stream: &StreamId,
        sequence: u64,
    ) -> Result<Vec<AdapterEvent>, AdapterError> {
        let session_id = self.require_session(value, "result")?;
        let subtype = optional_str(value, "subtype").unwrap_or("unknown");
        let is_error = value
            .get("is_error")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let status = if is_error || subtype == "error" {
            TurnStatus::Failed
        } else if matches!(subtype, "success" | "completion") {
            TurnStatus::Completed
        } else {
            TurnStatus::Unknown(sanitize_action_name(subtype))
        };
        let error = if is_error {
            let message = optional_str(value, "error")
                .or_else(|| optional_str(value, "result"))
                .unwrap_or("");
            Some(ProviderError {
                message: redact_bounded(&self.config, message, self.config.limits.max_text_bytes),
                code: None,
                details: None,
            })
        } else {
            None
        };
        let native_id = Self::event_uuid(value).or_else(|| {
            value
                .get("num_turns")
                .and_then(Value::as_u64)
                .map(|turns| format!("{session_id}:result:{turns}"))
        });
        let mut events = Vec::new();
        if let Some(denials) = value.get("permission_denials").and_then(Value::as_array) {
            for denial in denials {
                let Some(tool_use_id) = denial.get("tool_use_id").and_then(Value::as_str) else {
                    events.push(self.unsupported_event(
                        "result/permission_denials",
                        "permission denial is missing tool_use_id",
                        UnsupportedScope::Action,
                        value,
                        stream,
                        sequence,
                    )?);
                    continue;
                };
                self.config.limits.check_id("tool_use_id", tool_use_id)?;
                let tool_name = denial
                    .get("tool_name")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let provenance = self.provenance(
                    "result/permission_denials",
                    native_id.clone(),
                    None,
                    Some(session_id),
                    None,
                    Some(tool_use_id),
                    stream,
                    sequence,
                )?;
                events.push(AdapterEvent::new(
                    ProviderKind::ClaudeStreamJson,
                    provenance,
                    EventPayload::PermissionDenied {
                        denial: PermissionDenied {
                            tool_call_id: ItemId::from(tool_use_id),
                            tool_name: sanitize_action_name(tool_name),
                            message: redact_bounded(
                                &self.config,
                                optional_str(value, "error").unwrap_or(""),
                                self.config.limits.max_text_bytes,
                            ),
                            reason: Some("permission_denials".to_owned()),
                            reason_type: None,
                        },
                    },
                ));
            }
        }
        let provenance = self.provenance(
            "result",
            native_id,
            None,
            Some(session_id),
            None,
            None,
            stream,
            sequence,
        )?;
        events.push(AdapterEvent::new(
            ProviderKind::ClaudeStreamJson,
            provenance,
            EventPayload::TurnCompleted {
                turn: TurnInfo {
                    id: None,
                    status,
                    error,
                    duration_ms: value.get("duration_ms").and_then(Value::as_u64),
                },
            },
        ));
        Ok(events)
    }

    fn parse_stream_event(
        &mut self,
        value: &Value,
        stream: &StreamId,
        sequence: u64,
    ) -> Result<Vec<AdapterEvent>, AdapterError> {
        let session_id = self.require_session(value, "stream_event")?;
        let event = value
            .get("event")
            .ok_or_else(|| AdapterError::MissingField {
                native_type: "stream_event".to_owned(),
                field: "event",
            })?;
        let event_type = optional_str(event, "type").unwrap_or("unknown");
        let native_id = Self::event_uuid(value);
        let parent = optional_str(value, "parent_tool_use_id");
        match event_type {
            "message_start" => {
                let message = event
                    .get("message")
                    .ok_or_else(|| AdapterError::MissingField {
                        native_type: "stream_event/message_start".to_owned(),
                        field: "event.message",
                    })?;
                let message_id = required_str(message, "id", "stream_event/message_start")?;
                self.config.limits.check_id("message_id", message_id)?;
                let state = self.streams.entry(stream.clone()).or_default();
                state.message_id = Some(message_id.to_owned());
                state.tool_blocks.clear();
                let provenance = self.provenance(
                    "stream_event/message_start",
                    Some(message_id.to_owned()),
                    parent.map(str::to_owned),
                    Some(session_id),
                    None,
                    Some(message_id),
                    stream,
                    sequence,
                )?;
                Ok(vec![AdapterEvent::new(
                    ProviderKind::ClaudeStreamJson,
                    provenance,
                    EventPayload::MessageStarted {
                        message: Message {
                            id: ItemId::from(message_id),
                            role: Role::Assistant,
                            parts: Vec::new(),
                            phase: None,
                            complete: false,
                            truncated: false,
                        },
                    },
                )])
            }
            "content_block_start" => {
                let index = event.get("index").and_then(Value::as_u64).unwrap_or(0);
                let block = event.get("content_block").unwrap_or(&Value::Null);
                let block_type = block
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                if block_type != "tool_use" {
                    return Ok(Vec::new());
                }
                let tool_id = required_str(block, "id", "stream_event/tool_use")?;
                self.config.limits.check_id("tool_use_id", tool_id)?;
                let name = optional_str(block, "name").unwrap_or("");
                let arguments = block
                    .get("input")
                    .map(|input| {
                        bounded_json(&self.config, input, self.config.limits.max_input_bytes)
                    })
                    .unwrap_or_else(BoundedText::empty);
                let state = self.streams.entry(stream.clone()).or_default();
                state.tool_blocks.insert(index, ItemId::from(tool_id));
                let provenance = self.provenance(
                    "stream_event/tool_use",
                    Some(tool_id.to_owned()),
                    parent.map(str::to_owned),
                    Some(session_id),
                    None,
                    Some(tool_id),
                    stream,
                    sequence,
                )?;
                Ok(vec![AdapterEvent::new(
                    ProviderKind::ClaudeStreamJson,
                    provenance,
                    EventPayload::ToolCallStarted {
                        call: ToolCall {
                            id: ItemId::from(tool_id),
                            name: Some(sanitize_action_name(name)),
                            kind: ToolKind::ProviderNamed,
                            status: ToolStatus::InProgress,
                            input: Some(ToolInput::Named {
                                name: sanitize_action_name(name),
                                arguments,
                            }),
                            output: None,
                            exit_code: None,
                            duration_ms: None,
                        },
                    },
                )])
            }
            "content_block_delta" => {
                let index = event.get("index").and_then(Value::as_u64).unwrap_or(0);
                let delta = event.get("delta").unwrap_or(&Value::Null);
                let delta_type = delta
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                match delta_type {
                    "text_delta" => {
                        let text = delta.get("text").and_then(Value::as_str).unwrap_or("");
                        let bounded =
                            redact_bounded(&self.config, text, self.config.limits.max_text_bytes);
                        let message_id = self
                            .streams
                            .get(stream)
                            .and_then(|state| state.message_id.clone())
                            .ok_or_else(|| AdapterError::MissingField {
                                native_type: "stream_event/text_delta".to_owned(),
                                field: "message_start",
                            })?;
                        let provenance = self.provenance(
                            "stream_event/text_delta",
                            native_id,
                            parent.map(str::to_owned),
                            Some(session_id),
                            None,
                            Some(&message_id),
                            stream,
                            sequence,
                        )?;
                        Ok(vec![AdapterEvent::new(
                            ProviderKind::ClaudeStreamJson,
                            provenance,
                            EventPayload::MessageDelta {
                                message_id: ItemId::from(message_id),
                                role: Role::Assistant,
                                text: bounded,
                            },
                        )])
                    }
                    "thinking_delta" => {
                        let text = delta.get("thinking").and_then(Value::as_str).unwrap_or("");
                        let bounded =
                            redact_bounded(&self.config, text, self.config.limits.max_text_bytes);
                        let provenance = self.provenance(
                            "stream_event/thinking_delta",
                            native_id,
                            parent.map(str::to_owned),
                            Some(session_id),
                            None,
                            None,
                            stream,
                            sequence,
                        )?;
                        Ok(vec![AdapterEvent::new(
                            ProviderKind::ClaudeStreamJson,
                            provenance,
                            EventPayload::Progress {
                                progress: Progress {
                                    kind: ProgressKind::ReasoningRaw,
                                    text: Some(bounded),
                                    tool_call_id: None,
                                    attempt: None,
                                    max_attempts: None,
                                    retry_delay_ms: None,
                                    elapsed_seconds: None,
                                    heartbeat: None,
                                },
                            },
                        )])
                    }
                    "input_json_delta" => {
                        let text = delta
                            .get("partial_json")
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        let bounded =
                            redact_bounded(&self.config, text, self.config.limits.max_input_bytes);
                        let tool_id = self
                            .streams
                            .get(stream)
                            .and_then(|state| state.tool_blocks.get(&index))
                            .cloned()
                            .ok_or_else(|| AdapterError::MissingField {
                                native_type: "stream_event/input_json_delta".to_owned(),
                                field: "content_block_start.tool_use",
                            })?;
                        let provenance = self.provenance(
                            "stream_event/input_json_delta",
                            native_id,
                            parent.map(str::to_owned),
                            Some(session_id),
                            None,
                            Some(tool_id.as_str()),
                            stream,
                            sequence,
                        )?;
                        Ok(vec![AdapterEvent::new(
                            ProviderKind::ClaudeStreamJson,
                            provenance,
                            EventPayload::Progress {
                                progress: Progress {
                                    kind: ProgressKind::ToolInput,
                                    text: Some(bounded),
                                    tool_call_id: Some(tool_id),
                                    attempt: None,
                                    max_attempts: None,
                                    retry_delay_ms: None,
                                    elapsed_seconds: None,
                                    heartbeat: None,
                                },
                            },
                        )])
                    }
                    other => Ok(vec![self.unsupported_event(
                        &format!("stream_event/delta:{other}"),
                        "stream delta not modelled by this contract version",
                        UnsupportedScope::Action,
                        value,
                        stream,
                        sequence,
                    )?]),
                }
            }
            "message_delta" => {
                let stop_reason = event
                    .get("delta")
                    .and_then(|delta| delta.get("stop_reason"))
                    .and_then(Value::as_str)
                    .map(sanitize_action_name);
                let provenance = self.provenance(
                    "stream_event/message_delta",
                    native_id,
                    parent.map(str::to_owned),
                    Some(session_id),
                    None,
                    None,
                    stream,
                    sequence,
                )?;
                Ok(vec![AdapterEvent::new(
                    ProviderKind::ClaudeStreamJson,
                    provenance,
                    EventPayload::Progress {
                        progress: Progress {
                            kind: ProgressKind::Status,
                            text: stop_reason.map(BoundedText::from_text),
                            tool_call_id: None,
                            attempt: None,
                            max_attempts: None,
                            retry_delay_ms: None,
                            elapsed_seconds: None,
                            heartbeat: None,
                        },
                    },
                )])
            }
            "message_stop" => {
                if let Some(state) = self.streams.get_mut(stream) {
                    state.message_id = None;
                    state.tool_blocks.clear();
                }
                Ok(Vec::new())
            }
            "ping" | "content_block_stop" => Ok(Vec::new()),
            "error" => {
                let message = event
                    .get("error")
                    .and_then(|error| error.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("stream error");
                let message =
                    redact_bounded(&self.config, message, self.config.limits.max_text_bytes);
                let provenance = self.provenance(
                    "stream_event/error",
                    native_id,
                    parent.map(str::to_owned),
                    Some(session_id),
                    None,
                    None,
                    stream,
                    sequence,
                )?;
                Ok(vec![AdapterEvent::new(
                    ProviderKind::ClaudeStreamJson,
                    provenance,
                    EventPayload::Notice {
                        notice: Notice {
                            severity: NoticeSeverity::Error,
                            message,
                            native_type: "stream_event/error".to_owned(),
                        },
                    },
                )])
            }
            other => Ok(vec![self.unsupported_event(
                &format!("stream_event/{other}"),
                "stream event not modelled by this contract version",
                UnsupportedScope::Action,
                value,
                stream,
                sequence,
            )?]),
        }
    }

    fn parse_tool_progress(
        &self,
        value: &Value,
        stream: &StreamId,
        sequence: u64,
    ) -> Result<Vec<AdapterEvent>, AdapterError> {
        let session_id = self.require_session(value, "tool_progress")?;
        let tool_use_id = required_str(value, "tool_use_id", "tool_progress")?;
        self.config.limits.check_id("tool_use_id", tool_use_id)?;
        let retry = value.get("subagent_retry");
        let attempt = retry
            .and_then(|retry| retry.get("attempt"))
            .and_then(Value::as_u64)
            .and_then(|attempt| u32::try_from(attempt).ok());
        let max_attempts = retry
            .and_then(|retry| retry.get("max_retries"))
            .and_then(Value::as_u64)
            .and_then(|attempt| u32::try_from(attempt).ok());
        let retry_delay_ms = retry
            .and_then(|retry| retry.get("retry_delay_ms"))
            .and_then(Value::as_u64);
        let text = retry
            .and_then(|retry| retry.get("error_category"))
            .and_then(Value::as_str)
            .map(|category| BoundedText::from_text(sanitize_action_name(category)));
        let provenance = self.provenance(
            "tool_progress",
            Self::event_uuid(value),
            optional_str(value, "parent_tool_use_id").map(str::to_owned),
            Some(session_id),
            None,
            Some(tool_use_id),
            stream,
            sequence,
        )?;
        Ok(vec![AdapterEvent::new(
            ProviderKind::ClaudeStreamJson,
            provenance,
            EventPayload::Progress {
                progress: Progress {
                    kind: ProgressKind::ToolProgress,
                    text,
                    tool_call_id: Some(ItemId::from(tool_use_id)),
                    attempt,
                    max_attempts,
                    retry_delay_ms,
                    elapsed_seconds: value.get("elapsed_time_seconds").and_then(Value::as_u64),
                    heartbeat: value.get("heartbeat").and_then(Value::as_bool),
                },
            },
        )])
    }

    fn unsupported_event(
        &self,
        action: &str,
        reason: &str,
        scope: UnsupportedScope,
        value: &Value,
        stream: &StreamId,
        sequence: u64,
    ) -> Result<AdapterEvent, AdapterError> {
        let session_id = optional_str(value, "session_id");
        let provenance =
            self.provenance(action, None, None, session_id, None, None, stream, sequence)?;
        Ok(AdapterEvent::new(
            ProviderKind::ClaudeStreamJson,
            provenance,
            EventPayload::Unsupported {
                action: UnsupportedAction {
                    action: sanitize_action_name(action),
                    reason: reason.to_owned(),
                    scope,
                },
            },
        ))
    }
}

impl ProviderAdapter for ClaudeStreamJsonAdapter {
    fn provider(&self) -> ProviderKind {
        ProviderKind::ClaudeStreamJson
    }

    fn descriptor(&self) -> &CapabilityDescriptor {
        &self.descriptor
    }

    fn parse_line(&mut self, stream: &StreamId, sequence: u64, line: &str) -> ParseOutcome {
        if let Err(error) = self.config.limits.check_line(line) {
            return ParseOutcome::failed(error);
        }
        if line.trim().is_empty() {
            return ParseOutcome::empty();
        }
        let value: Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(_) => {
                return ParseOutcome::failed(AdapterError::Malformed {
                    native_type: None,
                    detail: "invalid JSON".to_owned(),
                });
            }
        };
        match self.parse_value(&value, stream, sequence) {
            Ok(events) => ParseOutcome::ok(events),
            Err(error) => ParseOutcome::failed(error),
        }
    }
}
