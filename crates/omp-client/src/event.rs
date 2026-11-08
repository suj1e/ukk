//! Typed interpretation of stdout event frames.
//!
//! Every event keeps the original JSON value so a caller can persist or audit
//! forward-compatible fields. Only documented fields are interpreted.

use serde_json::Value;

#[derive(Clone, Debug, PartialEq)]
pub struct MessageEvent {
    pub role: String,
    /// Text blocks only. Thinking blocks are surfaced through
    /// [`EventKind::MessageUpdate`] deltas and never merged into the answer.
    pub text: Option<String>,
    pub tool_name: Option<String>,
    pub tool_call_id: Option<String>,
    pub is_error: Option<bool>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolEvent {
    pub tool_call_id: Option<String>,
    pub tool_name: Option<String>,
    /// Start: compact JSON arguments. Update: partial result text. End:
    /// result text. Always bounded by the Host timeline, never inferred.
    pub text: Option<String>,
    pub is_error: Option<bool>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum EventKind {
    AgentStart,
    /// `is_terminal` is never `false` unless omp explicitly said so.
    AgentEnd {
        is_terminal: bool,
    },
    TurnStart,
    TurnEnd,
    MessageStart(MessageEvent),
    MessageUpdate {
        role: String,
        delta_kind: String,
        delta: String,
        content_index: Option<u64>,
    },
    MessageEnd(MessageEvent),
    ToolExecutionStart(ToolEvent),
    ToolExecutionUpdate(ToolEvent),
    ToolExecutionEnd(ToolEvent),
    PromptResult {
        id: Option<String>,
        agent_invoked: bool,
    },
    CommandOutput {
        text: String,
    },
    Notice {
        level: String,
        message: String,
        source: Option<String>,
    },
    SessionInfo {
        session_id: String,
        title: Option<String>,
    },
    AvailableCommandsUpdated {
        count: usize,
    },
    ConfigUpdate {
        model: Option<String>,
        thinking_level: Option<String>,
    },
    ModelChanged,
    ThinkingLevelChanged {
        level: Option<String>,
    },
    AutoRetry {
        phase: String,
        attempt: Option<u64>,
        message: Option<String>,
    },
    AutoCompaction {
        phase: String,
        reason: Option<String>,
    },
    /// An extension UI request. Fire-and-forget methods are recorded without
    /// a response; value-expecting methods are answered `cancelled` so the
    /// extension never deadlocks.
    ExtensionUi {
        id: String,
        method: String,
        requires_response: bool,
    },
    /// A frame type that is understood but deliberately not implemented
    /// (v2 chunks, host tools, host URIs, uncorrelated responses).
    Unsupported {
        frame_type: String,
        detail: String,
    },
    ProtocolError {
        message: String,
    },
    MalformedFrame {
        detail: String,
        bytes: usize,
    },
    OversizedFrame {
        limit: usize,
    },
    Unknown {
        frame_type: String,
    },
    ErrorFrame {
        message: String,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    /// Per-process monotonically increasing application-event sequence.
    pub sequence: u64,
    pub kind: EventKind,
    pub raw: Value,
}

impl Event {
    /// Documented turn completion:
    /// `agent_end` with `isTerminal != false`, or a local command
    /// (`prompt_result.agentInvoked == false`).
    pub fn is_turn_terminal(&self) -> bool {
        match &self.kind {
            EventKind::AgentEnd { is_terminal } => *is_terminal,
            EventKind::PromptResult { agent_invoked, .. } => !agent_invoked,
            _ => false,
        }
    }
}

fn field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

/// Join `text` blocks. Thinking and tool-call blocks are intentionally not
/// included in an answer's visible text.
pub(crate) fn content_text(content: Option<&Value>) -> Option<String> {
    match content? {
        Value::String(text) => Some(text.clone()),
        Value::Array(blocks) => {
            let mut text = String::new();
            for block in blocks {
                let kind = field(block, "type");
                if kind == Some("text") {
                    if let Some(part) = field(block, "text") {
                        text.push_str(part);
                    }
                }
            }
            (!text.is_empty()).then_some(text)
        }
        _ => None,
    }
}

fn message_event(message: Option<&Value>) -> Option<MessageEvent> {
    let message = message?;
    Some(MessageEvent {
        role: field(message, "role").unwrap_or("unknown").to_owned(),
        text: content_text(message.get("content")),
        tool_name: field(message, "toolName").map(str::to_owned),
        tool_call_id: field(message, "toolCallId").map(str::to_owned),
        is_error: message.get("isError").and_then(Value::as_bool),
    })
}

fn bounded_json(value: Option<&Value>) -> Option<String> {
    value.map(|value| value.to_string())
}

fn result_text(result: Option<&Value>) -> Option<String> {
    let result = result?;
    match result {
        Value::String(text) => Some(text.clone()),
        Value::Object(object) => {
            if let Some(text) = content_text(object.get("content")) {
                return Some(text);
            }
            if let Some(text) = field(result, "text") {
                return Some(text.to_owned());
            }
            Some(result.to_string())
        }
        _ => Some(result.to_string()),
    }
}

/// Interpret one non-response, non-handshake frame.
pub(crate) fn parse_event(value: &Value) -> EventKind {
    let frame_type = field(value, "type").unwrap_or("unknown");
    match frame_type {
        "agent_start" => EventKind::AgentStart,
        "agent_end" => EventKind::AgentEnd {
            // Absent means terminal-compatible; only an explicit `false`
            // keeps the run open for asynchronous delivery.
            is_terminal: value
                .get("isTerminal")
                .and_then(Value::as_bool)
                .unwrap_or(true),
        },
        "turn_start" => EventKind::TurnStart,
        "turn_end" => EventKind::TurnEnd,
        "message_start" => message_event(value.get("message"))
            .map(EventKind::MessageStart)
            .unwrap_or_else(|| EventKind::ProtocolError {
                message: "message_start without a message".into(),
            }),
        "message_end" => message_event(value.get("message"))
            .map(EventKind::MessageEnd)
            .unwrap_or_else(|| EventKind::ProtocolError {
                message: "message_end without a message".into(),
            }),
        "message_update" => {
            let message = value.get("message");
            let update = value.get("assistantMessageEvent");
            let delta_kind = update
                .and_then(|update| field(update, "type"))
                .unwrap_or("unknown")
                .to_owned();
            EventKind::MessageUpdate {
                role: message
                    .and_then(|message| field(message, "role"))
                    .unwrap_or("assistant")
                    .to_owned(),
                delta_kind,
                delta: update
                    .and_then(|update| field(update, "delta"))
                    .unwrap_or("")
                    .to_owned(),
                content_index: update
                    .and_then(|update| update.get("contentIndex"))
                    .and_then(Value::as_u64),
            }
        }
        "tool_execution_start" => EventKind::ToolExecutionStart(ToolEvent {
            tool_call_id: field(value, "toolCallId").map(str::to_owned),
            tool_name: field(value, "toolName").map(str::to_owned),
            text: bounded_json(value.get("args")),
            is_error: None,
        }),
        "tool_execution_update" => EventKind::ToolExecutionUpdate(ToolEvent {
            tool_call_id: field(value, "toolCallId").map(str::to_owned),
            tool_name: field(value, "toolName").map(str::to_owned),
            text: result_text(value.get("partialResult")),
            is_error: None,
        }),
        "tool_execution_end" => EventKind::ToolExecutionEnd(ToolEvent {
            tool_call_id: field(value, "toolCallId").map(str::to_owned),
            tool_name: field(value, "toolName").map(str::to_owned),
            text: result_text(value.get("result")),
            is_error: value.get("isError").and_then(Value::as_bool),
        }),
        "prompt_result" => EventKind::PromptResult {
            id: field(value, "id").map(str::to_owned),
            // Absent means "keep watching lifecycle events".
            agent_invoked: value
                .get("agentInvoked")
                .and_then(Value::as_bool)
                .unwrap_or(true),
        },
        "command_output" => EventKind::CommandOutput {
            text: field(value, "text").unwrap_or("").to_owned(),
        },
        "notice" => EventKind::Notice {
            level: field(value, "level").unwrap_or("info").to_owned(),
            message: field(value, "message").unwrap_or("").to_owned(),
            source: field(value, "source").map(str::to_owned),
        },
        "session_info_update" => EventKind::SessionInfo {
            session_id: field(value, "sessionId").unwrap_or("").to_owned(),
            title: field(value, "title").map(str::to_owned),
        },
        "available_commands_update" => EventKind::AvailableCommandsUpdated {
            count: value
                .get("commands")
                .and_then(Value::as_array)
                .map(Vec::len)
                .unwrap_or(0),
        },
        "config_update" => EventKind::ConfigUpdate {
            model: field(value, "model").map(str::to_owned),
            thinking_level: field(value, "thinkingLevel").map(str::to_owned),
        },
        "model_changed" => EventKind::ModelChanged,
        "thinking_level_changed" => EventKind::ThinkingLevelChanged {
            level: field(value, "thinkingLevel").map(str::to_owned),
        },
        "auto_retry_start" | "auto_retry_end" => EventKind::AutoRetry {
            phase: frame_type
                .strip_prefix("auto_retry_")
                .unwrap_or(frame_type)
                .to_owned(),
            attempt: value.get("attempt").and_then(Value::as_u64),
            message: value
                .get("errorMessage")
                .or_else(|| value.get("finalError"))
                .and_then(Value::as_str)
                .map(str::to_owned),
        },
        "auto_compaction_start" | "auto_compaction_end" => EventKind::AutoCompaction {
            phase: frame_type
                .strip_prefix("auto_compaction_")
                .unwrap_or(frame_type)
                .to_owned(),
            reason: field(value, "reason")
                .or_else(|| field(value, "action"))
                .map(str::to_owned),
        },
        "extension_error" => EventKind::ErrorFrame {
            message: format!(
                "extension error in {}: {}",
                field(value, "event").unwrap_or("unknown event"),
                field(value, "error").unwrap_or("unknown error")
            ),
        },
        "error" => EventKind::ErrorFrame {
            message: field(value, "error")
                .or_else(|| field(value, "message"))
                .unwrap_or("omp reported an unspecified error")
                .to_owned(),
        },
        "rpc_frame_error" => EventKind::ProtocolError {
            message: field(value, "error")
                .unwrap_or("omp could not encode a frame")
                .to_owned(),
        },
        other => EventKind::Unknown {
            frame_type: other.to_owned(),
        },
    }
}
