//! Codex app-server v2 fixture adapter.
//!
//! Wire source: <https://learn.chatgpt.com/docs/app-server.md> and the
//! generated protocol types in `openai/codex` (`app-server-protocol` v2).
//! The JSON-RPC `"jsonrpc":"2.0"` header may be omitted on the wire and is
//! accepted either way.

use crate::adapter::{AdapterConfig, ParseOutcome, ProviderAdapter};
use crate::capability::{CapabilityDescriptor, ProviderKind};
use crate::contract::{
    AdapterEvent, ApprovalDecision, ApprovalEvidence, ApprovalKind, ApprovalRequest,
    ApprovalSubject, Artifact, ArtifactKind, BoundedText, EventPayload, FileChange, Message,
    MessagePart, NetworkApproval, Notice, NoticeSeverity, Plan, PlanStep, PlanStepStatus, Progress,
    ProgressKind, Provenance, ProviderError, Role, SessionInfo, SessionStatus, ToolCall, ToolInput,
    ToolKind, ToolStatus, TurnInfo, TurnStatus, UnsupportedAction, UnsupportedScope,
};
use crate::error::AdapterError;
use crate::ids::{ApprovalId, ItemId, RequestId, SessionId, StreamId, TurnId};
use crate::util::{
    bounded_json, checked_opt_id, checked_str_id, optional_str, redact_bounded, request_id,
    required_str, sanitize_action_name,
};
use serde_json::Value;
use std::collections::{HashMap, VecDeque};

pub struct CodexAppServerAdapter {
    config: AdapterConfig,
    descriptor: CapabilityDescriptor,
    approvals: HashMap<String, String>,
    approval_order: VecDeque<String>,
}

impl CodexAppServerAdapter {
    pub fn new(config: AdapterConfig, descriptor: CapabilityDescriptor) -> Self {
        Self {
            config,
            descriptor,
            approvals: HashMap::new(),
            approval_order: VecDeque::new(),
        }
    }

    fn remember_approval(&mut self, request_id: &str, approval_id: &str) {
        if self.approvals.len() >= 1024 {
            if let Some(oldest) = self.approval_order.pop_front() {
                self.approvals.remove(&oldest);
            }
        }
        if self
            .approvals
            .insert(request_id.to_owned(), approval_id.to_owned())
            .is_none()
        {
            self.approval_order.push_back(request_id.to_owned());
        }
    }

    fn approval_id_for(&self, request_id: &str) -> String {
        self.approvals
            .get(request_id)
            .cloned()
            .unwrap_or_else(|| request_id.to_owned())
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

    fn session_info(&self, thread: &Value, native_type: &str) -> Result<SessionInfo, AdapterError> {
        let id = checked_str_id(&self.config, thread, "id", native_type)?;
        let native_session_id = match optional_str(thread, "sessionId") {
            Some(native) => {
                self.config.limits.check_id("native_session_id", native)?;
                Some(native.to_owned())
            }
            None => Some(id.to_owned()),
        };
        let model = optional_str(thread, "model").map(|model| {
            redact_bounded(&self.config, model, self.config.limits.max_text_bytes).text
        });
        let cwd = optional_str(thread, "cwd")
            .map(|cwd| redact_bounded(&self.config, cwd, self.config.limits.max_text_bytes));
        Ok(SessionInfo {
            id: SessionId::from(id),
            native_session_id,
            model,
            cwd,
            tools: Vec::new(),
        })
    }

    fn session_event(
        &self,
        thread: &Value,
        native_type: &str,
        stream: &StreamId,
        sequence: u64,
    ) -> Result<AdapterEvent, AdapterError> {
        let info = self.session_info(thread, native_type)?;
        let resumed = self.config.resume_session.as_ref() == Some(&info.id);
        let provenance = self.provenance(
            native_type,
            Some(info.id.to_string()),
            None,
            Some(info.id.as_str()),
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
        Ok(AdapterEvent::new(
            ProviderKind::CodexAppServer,
            provenance,
            payload,
        ))
    }

    fn parse_value(
        &mut self,
        value: &Value,
        stream: &StreamId,
        sequence: u64,
    ) -> Result<Vec<AdapterEvent>, AdapterError> {
        if let Some(version) = value.get("jsonrpc") {
            if version.as_str() != Some("2.0") {
                return Err(AdapterError::Malformed {
                    native_type: Some("jsonrpc".to_owned()),
                    detail: "expected JSON-RPC 2.0".to_owned(),
                });
            }
        }

        if let Some(method) = value.get("method").and_then(Value::as_str) {
            let params = value.get("params").unwrap_or(&Value::Null);
            if let Some(request_id) = value.get("id").and_then(request_id) {
                return self.parse_server_request(method, params, &request_id, stream, sequence);
            }
            return self.parse_notification(method, params, stream, sequence);
        }

        if let Some(result) = value.get("result") {
            return self.parse_response(result, stream, sequence);
        }
        if let Some(error) = value.get("error") {
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("provider returned an error");
            let bounded = redact_bounded(&self.config, message, self.config.limits.max_text_bytes);
            let provenance = self.provenance(
                "response/error",
                None,
                None,
                None,
                None,
                None,
                stream,
                sequence,
            )?;
            return Ok(vec![AdapterEvent::new(
                ProviderKind::CodexAppServer,
                provenance,
                EventPayload::Notice {
                    notice: Notice {
                        severity: NoticeSeverity::Error,
                        message: bounded,
                        native_type: "response/error".to_owned(),
                    },
                },
            )]);
        }

        Err(AdapterError::Malformed {
            native_type: None,
            detail: "not a JSON-RPC request, notification or response".to_owned(),
        })
    }

    fn parse_response(
        &self,
        result: &Value,
        stream: &StreamId,
        sequence: u64,
    ) -> Result<Vec<AdapterEvent>, AdapterError> {
        if let Some(thread) = result
            .get("thread")
            .filter(|thread| thread.get("id").is_some())
        {
            return Ok(vec![self.session_event(
                thread,
                "response/thread",
                stream,
                sequence,
            )?]);
        }
        Ok(Vec::new())
    }

    fn parse_notification(
        &self,
        method: &str,
        params: &Value,
        stream: &StreamId,
        sequence: u64,
    ) -> Result<Vec<AdapterEvent>, AdapterError> {
        let thread_id = optional_str(params, "threadId");
        match method {
            "thread/started" => {
                let thread = params
                    .get("thread")
                    .ok_or_else(|| AdapterError::MissingField {
                        native_type: sanitize_action_name(method),
                        field: "thread",
                    })?;
                Ok(vec![self.session_event(thread, method, stream, sequence)?])
            }
            "thread/status/changed" => {
                let status = params
                    .get("status")
                    .map(parse_thread_status)
                    .unwrap_or(SessionStatus::Unknown);
                let provenance =
                    self.provenance(method, None, None, thread_id, None, None, stream, sequence)?;
                Ok(vec![AdapterEvent::new(
                    ProviderKind::CodexAppServer,
                    provenance,
                    EventPayload::SessionStatus { status },
                )])
            }
            "turn/started" | "turn/completed" => {
                let turn = params
                    .get("turn")
                    .ok_or_else(|| AdapterError::MissingField {
                        native_type: sanitize_action_name(method),
                        field: "turn",
                    })?;
                let info = self.turn_info(turn, method)?;
                let native_id = info.id.as_ref().map(ToString::to_string);
                let provenance = self.provenance(
                    method,
                    native_id,
                    None,
                    thread_id,
                    info.id.as_ref().map(TurnId::as_str),
                    None,
                    stream,
                    sequence,
                )?;
                let payload = if method == "turn/started" {
                    EventPayload::TurnStarted { turn: info }
                } else {
                    EventPayload::TurnCompleted { turn: info }
                };
                Ok(vec![AdapterEvent::new(
                    ProviderKind::CodexAppServer,
                    provenance,
                    payload,
                )])
            }
            "turn/plan/updated" => {
                let turn_id = checked_str_id(&self.config, params, "turnId", method)?;
                let steps = params
                    .get("plan")
                    .and_then(Value::as_array)
                    .map(|steps| {
                        steps
                            .iter()
                            .filter_map(|step| {
                                let text = step.get("step").and_then(Value::as_str)?;
                                Some(PlanStep {
                                    step: redact_bounded(
                                        &self.config,
                                        text,
                                        self.config.limits.max_text_bytes,
                                    )
                                    .text,
                                    status: parse_plan_step_status(
                                        step.get("status").and_then(Value::as_str),
                                    ),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let explanation = optional_str(params, "explanation").map(|text| {
                    redact_bounded(&self.config, text, self.config.limits.max_text_bytes)
                });
                let provenance = self.provenance(
                    method,
                    Some(turn_id.to_owned()),
                    None,
                    thread_id,
                    Some(turn_id),
                    None,
                    stream,
                    sequence,
                )?;
                Ok(vec![AdapterEvent::new(
                    ProviderKind::CodexAppServer,
                    provenance,
                    EventPayload::Plan {
                        plan: Plan { explanation, steps },
                    },
                )])
            }
            "turn/diff/updated" => {
                let turn_id = checked_str_id(&self.config, params, "turnId", method)?;
                let diff = optional_str(params, "diff").unwrap_or("");
                let content =
                    redact_bounded(&self.config, diff, self.config.limits.max_input_bytes);
                let provenance = self.provenance(
                    method,
                    Some(format!("turn_diff:{turn_id}")),
                    None,
                    thread_id,
                    Some(turn_id),
                    None,
                    stream,
                    sequence,
                )?;
                Ok(vec![AdapterEvent::new(
                    ProviderKind::CodexAppServer,
                    provenance,
                    EventPayload::Artifacts {
                        artifacts: vec![Artifact {
                            path: None,
                            kind: ArtifactKind::TurnDiff,
                            change_kind: None,
                            content,
                        }],
                    },
                )])
            }
            "item/started" | "item/completed" => self.parse_item(method, params, stream, sequence),
            "item/agentMessage/delta" => {
                let item_id = checked_str_id(&self.config, params, "itemId", method)?;
                let delta = optional_str(params, "delta").unwrap_or("");
                let text = redact_bounded(&self.config, delta, self.config.limits.max_text_bytes);
                let provenance = self.provenance(
                    method,
                    Some(item_id.to_owned()),
                    None,
                    thread_id,
                    optional_str(params, "turnId"),
                    Some(item_id),
                    stream,
                    sequence,
                )?;
                Ok(vec![AdapterEvent::new(
                    ProviderKind::CodexAppServer,
                    provenance,
                    EventPayload::MessageDelta {
                        message_id: ItemId::from(item_id),
                        role: Role::Assistant,
                        text,
                    },
                )])
            }
            "item/plan/delta" => {
                let item_id = checked_str_id(&self.config, params, "itemId", method)?;
                let delta = optional_str(params, "delta").unwrap_or("");
                let text = redact_bounded(&self.config, delta, self.config.limits.max_text_bytes);
                let provenance = self.provenance(
                    method,
                    Some(item_id.to_owned()),
                    None,
                    thread_id,
                    optional_str(params, "turnId"),
                    Some(item_id),
                    stream,
                    sequence,
                )?;
                Ok(vec![AdapterEvent::new(
                    ProviderKind::CodexAppServer,
                    provenance,
                    EventPayload::PlanText {
                        item_id: ItemId::from(item_id),
                        text,
                        delta: true,
                        final_item: false,
                    },
                )])
            }
            "item/reasoning/summaryTextDelta" | "item/reasoning/textDelta" => {
                let item_id = checked_str_id(&self.config, params, "itemId", method)?;
                let delta = optional_str(params, "delta").unwrap_or("");
                let text = redact_bounded(&self.config, delta, self.config.limits.max_text_bytes);
                let kind = if method.ends_with("summaryTextDelta") {
                    ProgressKind::ReasoningSummary
                } else {
                    ProgressKind::ReasoningRaw
                };
                let provenance = self.provenance(
                    method,
                    Some(item_id.to_owned()),
                    None,
                    thread_id,
                    optional_str(params, "turnId"),
                    Some(item_id),
                    stream,
                    sequence,
                )?;
                Ok(vec![AdapterEvent::new(
                    ProviderKind::CodexAppServer,
                    provenance,
                    EventPayload::Progress {
                        progress: Progress {
                            kind,
                            text: Some(text),
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
            "item/commandExecution/outputDelta" => {
                let item_id = checked_str_id(&self.config, params, "itemId", method)?;
                let delta = optional_str(params, "delta").unwrap_or("");
                let text = redact_bounded(&self.config, delta, self.config.limits.max_output_bytes);
                let provenance = self.provenance(
                    method,
                    Some(item_id.to_owned()),
                    None,
                    thread_id,
                    optional_str(params, "turnId"),
                    Some(item_id),
                    stream,
                    sequence,
                )?;
                Ok(vec![AdapterEvent::new(
                    ProviderKind::CodexAppServer,
                    provenance,
                    EventPayload::Progress {
                        progress: Progress {
                            kind: ProgressKind::ToolOutput,
                            text: Some(text),
                            tool_call_id: Some(ItemId::from(item_id)),
                            attempt: None,
                            max_attempts: None,
                            retry_delay_ms: None,
                            elapsed_seconds: None,
                            heartbeat: None,
                        },
                    },
                )])
            }
            "serverRequest/resolved" => {
                let request_id = params
                    .get("requestId")
                    .and_then(request_id)
                    .ok_or_else(|| AdapterError::MissingField {
                        native_type: sanitize_action_name(method),
                        field: "requestId",
                    })?;
                let approval_id = self.approval_id_for(&request_id);
                let provenance = self.provenance(
                    method,
                    Some(request_id.clone()),
                    None,
                    thread_id,
                    None,
                    None,
                    stream,
                    sequence,
                )?;
                Ok(vec![AdapterEvent::new(
                    ProviderKind::CodexAppServer,
                    provenance,
                    EventPayload::ApprovalCleared {
                        approval_id: ApprovalId::from(approval_id),
                        request_id: RequestId::from(request_id),
                        evidence: ApprovalEvidence::ServerRequestResolved,
                    },
                )])
            }
            "thread/closed" => {
                let provenance =
                    self.provenance(method, None, None, thread_id, None, None, stream, sequence)?;
                Ok(vec![AdapterEvent::new(
                    ProviderKind::CodexAppServer,
                    provenance,
                    EventPayload::SessionStatus {
                        status: SessionStatus::Closed,
                    },
                )])
            }
            "warning" | "configWarning" => {
                let message = optional_str(params, "message")
                    .or_else(|| optional_str(params, "summary"))
                    .unwrap_or("");
                let bounded =
                    redact_bounded(&self.config, message, self.config.limits.max_text_bytes);
                let provenance =
                    self.provenance(method, None, None, thread_id, None, None, stream, sequence)?;
                Ok(vec![AdapterEvent::new(
                    ProviderKind::CodexAppServer,
                    provenance,
                    EventPayload::Notice {
                        notice: Notice {
                            severity: NoticeSeverity::Warning,
                            message: bounded,
                            native_type: sanitize_action_name(method),
                        },
                    },
                )])
            }
            "error" => {
                let message = params
                    .get("error")
                    .and_then(|error| error.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("provider error");
                let bounded =
                    redact_bounded(&self.config, message, self.config.limits.max_text_bytes);
                let provenance = self.provenance(
                    method,
                    None,
                    None,
                    thread_id,
                    optional_str(params, "turnId"),
                    None,
                    stream,
                    sequence,
                )?;
                Ok(vec![AdapterEvent::new(
                    ProviderKind::CodexAppServer,
                    provenance,
                    EventPayload::Notice {
                        notice: Notice {
                            severity: NoticeSeverity::Error,
                            message: bounded,
                            native_type: sanitize_action_name(method),
                        },
                    },
                )])
            }
            other => Ok(vec![self.unsupported_event(
                other,
                "notification not modelled by this contract version",
                UnsupportedScope::Action,
                stream,
                sequence,
            )?]),
        }
    }

    fn parse_item(
        &self,
        method: &str,
        params: &Value,
        stream: &StreamId,
        sequence: u64,
    ) -> Result<Vec<AdapterEvent>, AdapterError> {
        let item = params
            .get("item")
            .ok_or_else(|| AdapterError::MissingField {
                native_type: sanitize_action_name(method),
                field: "item",
            })?;
        let native_type = sanitize_action_name(method);
        let item_id = checked_str_id(&self.config, item, "id", &native_type)?;
        let item_type = required_str(item, "type", &native_type)?;
        let thread_id = optional_str(params, "threadId");
        let turn_id = optional_str(params, "turnId");
        let completed = method == "item/completed";

        match item_type {
            "userMessage" | "agentMessage" => {
                let role = if item_type == "userMessage" {
                    Role::User
                } else {
                    Role::Assistant
                };
                let (parts, truncated) = if item_type == "userMessage" {
                    self.content_parts(item.get("content"))?
                } else {
                    let text = optional_str(item, "text").unwrap_or("");
                    let bounded =
                        redact_bounded(&self.config, text, self.config.limits.max_text_bytes);
                    (
                        vec![MessagePart::Text { text: bounded.text }],
                        bounded.truncated,
                    )
                };
                let phase = optional_str(item, "phase").map(str::to_owned);
                let provenance = self.provenance(
                    method,
                    Some(item_id.to_owned()),
                    None,
                    thread_id,
                    turn_id,
                    Some(item_id),
                    stream,
                    sequence,
                )?;
                let message = Message {
                    id: ItemId::from(item_id),
                    role,
                    parts,
                    phase,
                    complete: completed,
                    truncated,
                };
                let payload = if completed {
                    EventPayload::MessageCompleted { message }
                } else {
                    EventPayload::MessageStarted { message }
                };
                Ok(vec![AdapterEvent::new(
                    ProviderKind::CodexAppServer,
                    provenance,
                    payload,
                )])
            }
            "reasoning" => {
                let mut events = Vec::new();
                for (field, kind) in [
                    ("summary", ProgressKind::ReasoningSummary),
                    ("content", ProgressKind::ReasoningRaw),
                ] {
                    let text = item
                        .get(field)
                        .and_then(Value::as_array)
                        .map(|entries| {
                            entries
                                .iter()
                                .filter_map(Value::as_str)
                                .collect::<Vec<_>>()
                                .join("\n")
                        })
                        .unwrap_or_default();
                    if text.is_empty() {
                        continue;
                    }
                    let bounded =
                        redact_bounded(&self.config, &text, self.config.limits.max_text_bytes);
                    let provenance = self.provenance(
                        method,
                        Some(item_id.to_owned()),
                        None,
                        thread_id,
                        turn_id,
                        Some(item_id),
                        stream,
                        sequence,
                    )?;
                    events.push(AdapterEvent::new(
                        ProviderKind::CodexAppServer,
                        provenance,
                        EventPayload::Progress {
                            progress: Progress {
                                kind,
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
                Ok(events)
            }
            "commandExecution" => {
                let command = optional_str(item, "command").unwrap_or("");
                let command_text =
                    redact_bounded(&self.config, command, self.config.limits.max_input_bytes);
                let cwd = optional_str(item, "cwd").map(|cwd| {
                    redact_bounded(&self.config, cwd, self.config.limits.max_input_bytes)
                });
                let output = optional_str(item, "aggregatedOutput").map(|output| {
                    redact_bounded(&self.config, output, self.config.limits.max_output_bytes)
                });
                let status = parse_tool_status(optional_str(item, "status"));
                let call = ToolCall {
                    id: ItemId::from(item_id),
                    name: Some("commandExecution".to_owned()),
                    kind: ToolKind::Command,
                    status,
                    input: Some(ToolInput::Command {
                        command: command_text,
                        cwd,
                    }),
                    output,
                    exit_code: item
                        .get("exitCode")
                        .and_then(Value::as_i64)
                        .and_then(|code| i32::try_from(code).ok()),
                    duration_ms: item.get("durationMs").and_then(Value::as_u64),
                };
                let provenance = self.provenance(
                    method,
                    Some(item_id.to_owned()),
                    None,
                    thread_id,
                    turn_id,
                    Some(item_id),
                    stream,
                    sequence,
                )?;
                let payload = if completed {
                    EventPayload::ToolCallCompleted { call }
                } else {
                    EventPayload::ToolCallStarted { call }
                };
                Ok(vec![AdapterEvent::new(
                    ProviderKind::CodexAppServer,
                    provenance,
                    payload,
                )])
            }
            "fileChange" => {
                let changes = parse_file_changes(&self.config, item.get("changes"));
                let status = parse_tool_status(optional_str(item, "status"));
                let call = ToolCall {
                    id: ItemId::from(item_id),
                    name: Some("fileChange".to_owned()),
                    kind: ToolKind::FileChange,
                    status,
                    input: Some(ToolInput::FileChange {
                        changes: changes.clone(),
                    }),
                    output: None,
                    exit_code: None,
                    duration_ms: None,
                };
                let provenance = self.provenance(
                    method,
                    Some(item_id.to_owned()),
                    None,
                    thread_id,
                    turn_id,
                    Some(item_id),
                    stream,
                    sequence,
                )?;
                let mut events = vec![AdapterEvent::new(
                    ProviderKind::CodexAppServer,
                    provenance.clone(),
                    if completed {
                        EventPayload::ToolCallCompleted { call }
                    } else {
                        EventPayload::ToolCallStarted { call }
                    },
                )];
                if !changes.is_empty() {
                    let artifacts = changes
                        .iter()
                        .map(|change| Artifact {
                            path: Some(change.path.clone()),
                            kind: ArtifactKind::FileChange,
                            change_kind: change.kind.clone(),
                            content: change.diff.clone(),
                        })
                        .collect();
                    events.push(AdapterEvent::new(
                        ProviderKind::CodexAppServer,
                        provenance,
                        EventPayload::Artifacts { artifacts },
                    ));
                }
                Ok(events)
            }
            "mcpToolCall" => {
                let server = optional_str(item, "server").unwrap_or("");
                let tool = optional_str(item, "tool").unwrap_or("");
                let arguments = item
                    .get("arguments")
                    .map(|arguments| {
                        bounded_json(&self.config, arguments, self.config.limits.max_input_bytes)
                    })
                    .unwrap_or_else(BoundedText::empty);
                let error_message = item
                    .get("error")
                    .and_then(|error| error.get("message"))
                    .and_then(Value::as_str);
                let output = if let Some(message) = error_message {
                    Some(redact_bounded(
                        &self.config,
                        message,
                        self.config.limits.max_output_bytes,
                    ))
                } else {
                    item.get("result").map(|result| {
                        bounded_json(&self.config, result, self.config.limits.max_output_bytes)
                    })
                };
                let status = parse_tool_status(optional_str(item, "status"));
                let call = ToolCall {
                    id: ItemId::from(item_id),
                    name: Some(tool.to_owned()),
                    kind: ToolKind::Mcp,
                    status,
                    input: Some(ToolInput::Mcp {
                        server: sanitize_action_name(server),
                        tool: sanitize_action_name(tool),
                        arguments,
                    }),
                    output,
                    exit_code: None,
                    duration_ms: item.get("durationMs").and_then(Value::as_u64),
                };
                let provenance = self.provenance(
                    method,
                    Some(item_id.to_owned()),
                    None,
                    thread_id,
                    turn_id,
                    Some(item_id),
                    stream,
                    sequence,
                )?;
                let payload = if completed {
                    EventPayload::ToolCallCompleted { call }
                } else {
                    EventPayload::ToolCallStarted { call }
                };
                Ok(vec![AdapterEvent::new(
                    ProviderKind::CodexAppServer,
                    provenance,
                    payload,
                )])
            }
            "dynamicToolCall" => {
                let tool = optional_str(item, "tool").unwrap_or("");
                let arguments = item
                    .get("arguments")
                    .map(|arguments| {
                        bounded_json(&self.config, arguments, self.config.limits.max_input_bytes)
                    })
                    .unwrap_or_else(BoundedText::empty);
                let call = ToolCall {
                    id: ItemId::from(item_id),
                    name: Some(tool.to_owned()),
                    kind: ToolKind::Dynamic,
                    status: parse_tool_status(optional_str(item, "status")),
                    input: Some(ToolInput::Named {
                        name: sanitize_action_name(tool),
                        arguments,
                    }),
                    output: None,
                    exit_code: None,
                    duration_ms: item.get("durationMs").and_then(Value::as_u64),
                };
                let provenance = self.provenance(
                    method,
                    Some(item_id.to_owned()),
                    None,
                    thread_id,
                    turn_id,
                    Some(item_id),
                    stream,
                    sequence,
                )?;
                let payload = if completed {
                    EventPayload::ToolCallCompleted { call }
                } else {
                    EventPayload::ToolCallStarted { call }
                };
                Ok(vec![AdapterEvent::new(
                    ProviderKind::CodexAppServer,
                    provenance,
                    payload,
                )])
            }
            "webSearch" => {
                let query = optional_str(item, "query").unwrap_or("");
                let call = ToolCall {
                    id: ItemId::from(item_id),
                    name: Some("webSearch".to_owned()),
                    kind: ToolKind::WebSearch,
                    status: if completed {
                        ToolStatus::Completed
                    } else {
                        ToolStatus::InProgress
                    },
                    input: Some(ToolInput::WebSearch {
                        query: redact_bounded(
                            &self.config,
                            query,
                            self.config.limits.max_input_bytes,
                        )
                        .text,
                    }),
                    output: None,
                    exit_code: None,
                    duration_ms: None,
                };
                let provenance = self.provenance(
                    method,
                    Some(item_id.to_owned()),
                    None,
                    thread_id,
                    turn_id,
                    Some(item_id),
                    stream,
                    sequence,
                )?;
                let payload = if completed {
                    EventPayload::ToolCallCompleted { call }
                } else {
                    EventPayload::ToolCallStarted { call }
                };
                Ok(vec![AdapterEvent::new(
                    ProviderKind::CodexAppServer,
                    provenance,
                    payload,
                )])
            }
            "functionCallOutput" => {
                let name = optional_str(item, "name").unwrap_or("");
                let output = item
                    .get("output")
                    .map(|output| {
                        bounded_json(&self.config, output, self.config.limits.max_output_bytes)
                    })
                    .unwrap_or_else(BoundedText::empty);
                let call = ToolCall {
                    id: ItemId::from(item_id),
                    name: Some(sanitize_action_name(name)),
                    kind: ToolKind::ProviderNamed,
                    status: ToolStatus::Completed,
                    input: None,
                    output: Some(output),
                    exit_code: None,
                    duration_ms: None,
                };
                let provenance = self.provenance(
                    method,
                    Some(item_id.to_owned()),
                    None,
                    thread_id,
                    turn_id,
                    Some(item_id),
                    stream,
                    sequence,
                )?;
                Ok(vec![AdapterEvent::new(
                    ProviderKind::CodexAppServer,
                    provenance,
                    EventPayload::ToolCallCompleted { call },
                )])
            }
            "plan" => {
                let text = optional_str(item, "text").unwrap_or("");
                let bounded = redact_bounded(&self.config, text, self.config.limits.max_text_bytes);
                let provenance = self.provenance(
                    method,
                    Some(item_id.to_owned()),
                    None,
                    thread_id,
                    turn_id,
                    Some(item_id),
                    stream,
                    sequence,
                )?;
                Ok(vec![AdapterEvent::new(
                    ProviderKind::CodexAppServer,
                    provenance,
                    EventPayload::PlanText {
                        item_id: ItemId::from(item_id),
                        text: bounded,
                        delta: false,
                        final_item: completed,
                    },
                )])
            }
            other => Ok(vec![self.unsupported_event(
                &format!("item:{other}"),
                "item type not modelled by this contract version",
                UnsupportedScope::Action,
                stream,
                sequence,
            )?]),
        }
    }

    fn content_parts(
        &self,
        content: Option<&Value>,
    ) -> Result<(Vec<MessagePart>, bool), AdapterError> {
        let Some(Value::Array(entries)) = content else {
            return Ok((Vec::new(), false));
        };
        let mut parts = Vec::new();
        let mut truncated = false;
        for entry in entries {
            let kind = entry
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            match kind {
                "text" => {
                    let text = entry.get("text").and_then(Value::as_str).unwrap_or("");
                    let bounded =
                        redact_bounded(&self.config, text, self.config.limits.max_text_bytes);
                    truncated |= bounded.truncated;
                    parts.push(MessagePart::Text { text: bounded.text });
                }
                other => {
                    let reference = entry
                        .get("url")
                        .or_else(|| entry.get("path"))
                        .or_else(|| entry.get("name"))
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
                        label: sanitize_action_name(other),
                        reference,
                    });
                }
            }
        }
        Ok((parts, truncated))
    }

    fn unsupported_event(
        &self,
        action: &str,
        reason: &str,
        scope: UnsupportedScope,
        stream: &StreamId,
        sequence: u64,
    ) -> Result<AdapterEvent, AdapterError> {
        let provenance = self.provenance(action, None, None, None, None, None, stream, sequence)?;
        Ok(AdapterEvent::new(
            ProviderKind::CodexAppServer,
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

    fn turn_info(&self, turn: &Value, native_type: &str) -> Result<TurnInfo, AdapterError> {
        let id = match optional_str(turn, "id") {
            Some(_) => Some(TurnId::from(checked_str_id(
                &self.config,
                turn,
                "id",
                native_type,
            )?)),
            None => None,
        };
        let status = parse_turn_status(optional_str(turn, "status"));
        let error = turn.get("error").map(|error| {
            let message = error.get("message").and_then(Value::as_str).unwrap_or("");
            ProviderError {
                message: redact_bounded(&self.config, message, self.config.limits.max_text_bytes),
                code: error
                    .get("codexErrorInfo")
                    .and_then(Value::as_str)
                    .map(sanitize_action_name),
                details: error
                    .get("additionalDetails")
                    .and_then(Value::as_str)
                    .map(|details| {
                        redact_bounded(&self.config, details, self.config.limits.max_text_bytes)
                    }),
            }
        });
        Ok(TurnInfo {
            id,
            status,
            error,
            duration_ms: turn.get("durationMs").and_then(Value::as_u64),
        })
    }

    fn parse_server_request(
        &mut self,
        method: &str,
        params: &Value,
        request_id: &str,
        stream: &StreamId,
        sequence: u64,
    ) -> Result<Vec<AdapterEvent>, AdapterError> {
        match method {
            "item/commandExecution/requestApproval" => {
                self.parse_command_approval(params, request_id, stream, sequence)
            }
            "item/fileChange/requestApproval" => {
                self.parse_file_approval(params, request_id, stream, sequence)
            }
            other => Ok(vec![self.unsupported_event(
                other,
                "server request not modelled by this contract version",
                UnsupportedScope::Action,
                stream,
                sequence,
            )?]),
        }
    }

    fn parse_command_approval(
        &mut self,
        params: &Value,
        request_id: &str,
        stream: &StreamId,
        sequence: u64,
    ) -> Result<Vec<AdapterEvent>, AdapterError> {
        let method = "item/commandExecution/requestApproval";
        let thread_id = checked_str_id(&self.config, params, "threadId", method)?;
        let turn_id = checked_str_id(&self.config, params, "turnId", method)?;
        let item_id = checked_str_id(&self.config, params, "itemId", method)?;
        let approval_id = optional_str(params, "approvalId")
            .map(str::to_owned)
            .unwrap_or_else(|| request_id.to_owned());
        self.config.limits.check_id("approval_id", &approval_id)?;
        self.remember_approval(request_id, &approval_id);

        let reason = optional_str(params, "reason")
            .map(|reason| redact_bounded(&self.config, reason, self.config.limits.max_text_bytes));
        let command = optional_str(params, "command").map(|command| {
            redact_bounded(&self.config, command, self.config.limits.max_input_bytes)
        });
        let cwd = optional_str(params, "cwd")
            .map(|cwd| redact_bounded(&self.config, cwd, self.config.limits.max_input_bytes));
        let network = params.get("networkApprovalContext").and_then(|network| {
            let host = network.get("host").and_then(Value::as_str)?;
            Some(NetworkApproval {
                host: sanitize_action_name(host),
                protocol: network
                    .get("protocol")
                    .and_then(Value::as_str)
                    .map(sanitize_action_name),
            })
        });
        let proposed_command =
            parse_execpolicy_amendment(&self.config, params.get("proposedExecpolicyAmendment"));
        let options = parse_approval_options(
            &self.config,
            params.get("availableDecisions"),
            proposed_command,
            &[
                ApprovalDecision::Accept,
                ApprovalDecision::AcceptForSession,
                ApprovalDecision::Decline,
                ApprovalDecision::Cancel,
            ],
        );
        let provenance = self.provenance(
            method,
            Some(approval_id.clone()),
            None,
            Some(thread_id),
            Some(turn_id),
            Some(item_id),
            stream,
            sequence,
        )?;
        Ok(vec![AdapterEvent::new(
            ProviderKind::CodexAppServer,
            provenance,
            EventPayload::ApprovalRequested {
                approval: ApprovalRequest {
                    id: ApprovalId::from(approval_id),
                    request_id: RequestId::from(request_id),
                    kind: ApprovalKind::CommandExecution,
                    item_id: Some(ItemId::from(item_id)),
                    subject: ApprovalSubject::Command {
                        reason,
                        command,
                        cwd,
                        network,
                    },
                    options,
                },
            },
        )])
    }

    fn parse_file_approval(
        &mut self,
        params: &Value,
        request_id: &str,
        stream: &StreamId,
        sequence: u64,
    ) -> Result<Vec<AdapterEvent>, AdapterError> {
        let method = "item/fileChange/requestApproval";
        let thread_id = checked_str_id(&self.config, params, "threadId", method)?;
        let turn_id = checked_str_id(&self.config, params, "turnId", method)?;
        let item_id = checked_str_id(&self.config, params, "itemId", method)?;
        let approval_id = request_id.to_owned();
        self.remember_approval(request_id, &approval_id);

        let reason = optional_str(params, "reason")
            .map(|reason| redact_bounded(&self.config, reason, self.config.limits.max_text_bytes));
        let grant_root = optional_str(params, "grantRoot")
            .map(|root| redact_bounded(&self.config, root, self.config.limits.max_input_bytes));
        let provenance = self.provenance(
            method,
            Some(approval_id.clone()),
            None,
            Some(thread_id),
            Some(turn_id),
            Some(item_id),
            stream,
            sequence,
        )?;
        Ok(vec![AdapterEvent::new(
            ProviderKind::CodexAppServer,
            provenance,
            EventPayload::ApprovalRequested {
                approval: ApprovalRequest {
                    id: ApprovalId::from(approval_id),
                    request_id: RequestId::from(request_id),
                    kind: ApprovalKind::FileChange,
                    item_id: Some(ItemId::from(item_id)),
                    subject: ApprovalSubject::FileChange { reason, grant_root },
                    options: vec![
                        ApprovalDecision::Accept,
                        ApprovalDecision::AcceptForSession,
                        ApprovalDecision::Decline,
                        ApprovalDecision::Cancel,
                    ],
                },
            },
        )])
    }
}

impl ProviderAdapter for CodexAppServerAdapter {
    fn provider(&self) -> ProviderKind {
        ProviderKind::CodexAppServer
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

fn parse_thread_status(status: &Value) -> SessionStatus {
    match status.get("type").and_then(Value::as_str) {
        Some("notLoaded") => SessionStatus::NotLoaded,
        Some("idle") => SessionStatus::Idle,
        Some("systemError") => SessionStatus::Error,
        Some("active") => SessionStatus::Active {
            flags: status
                .get("activeFlags")
                .and_then(Value::as_array)
                .map(|flags| {
                    flags
                        .iter()
                        .filter_map(Value::as_str)
                        .map(sanitize_action_name)
                        .collect()
                })
                .unwrap_or_default(),
        },
        _ => SessionStatus::Unknown,
    }
}

fn parse_turn_status(status: Option<&str>) -> TurnStatus {
    match status {
        Some("inProgress") => TurnStatus::InProgress,
        Some("completed") => TurnStatus::Completed,
        Some("interrupted") => TurnStatus::Interrupted,
        Some("failed") => TurnStatus::Failed,
        Some(other) => TurnStatus::Unknown(sanitize_action_name(other)),
        None => TurnStatus::Unknown("missing".to_owned()),
    }
}

fn parse_tool_status(status: Option<&str>) -> ToolStatus {
    match status {
        Some("pending") => ToolStatus::Pending,
        Some("inProgress") => ToolStatus::InProgress,
        Some("completed") => ToolStatus::Completed,
        Some("failed") => ToolStatus::Failed,
        Some("declined") => ToolStatus::Declined,
        Some("interrupted") => ToolStatus::Interrupted,
        Some(other) => ToolStatus::Unknown(sanitize_action_name(other)),
        None => ToolStatus::Unknown("missing".to_owned()),
    }
}

fn parse_plan_step_status(status: Option<&str>) -> PlanStepStatus {
    match status {
        Some("inProgress") => PlanStepStatus::InProgress,
        Some("completed") => PlanStepStatus::Completed,
        _ => PlanStepStatus::Pending,
    }
}

fn parse_file_changes(config: &AdapterConfig, changes: Option<&Value>) -> Vec<FileChange> {
    let Some(Value::Array(changes)) = changes else {
        return Vec::new();
    };
    changes
        .iter()
        .filter_map(|change| {
            let path = change.get("path").and_then(Value::as_str)?;
            let diff = change.get("diff").and_then(Value::as_str).unwrap_or("");
            Some(FileChange {
                path: redact_bounded(config, path, config.limits.max_text_bytes).text,
                kind: change
                    .get("kind")
                    .and_then(Value::as_str)
                    .map(sanitize_action_name),
                diff: redact_bounded(config, diff, config.limits.max_input_bytes),
            })
        })
        .collect()
}

fn parse_execpolicy_amendment(
    config: &AdapterConfig,
    value: Option<&Value>,
) -> Option<Vec<String>> {
    let value = value?;
    let commands = match value {
        Value::Array(commands) => commands,
        Value::Object(object) => object
            .get("execpolicyAmendment")
            .or_else(|| object.get("command"))?
            .as_array()?,
        _ => return None,
    };
    let command: Vec<String> = commands
        .iter()
        .filter_map(Value::as_str)
        .map(|token| redact_bounded(config, token, config.limits.max_input_bytes).text)
        .collect();
    if command.is_empty() {
        None
    } else {
        Some(command)
    }
}

fn parse_approval_options(
    config: &AdapterConfig,
    value: Option<&Value>,
    proposed_command: Option<Vec<String>>,
    defaults_without_amendment: &[ApprovalDecision],
) -> Vec<ApprovalDecision> {
    let defaults = || {
        let mut options = defaults_without_amendment.to_vec();
        if let Some(command) = &proposed_command {
            options.push(ApprovalDecision::AcceptWithExecpolicyAmendment {
                command: command.clone(),
            });
        }
        options
    };

    let Some(Value::Array(values)) = value else {
        return defaults();
    };
    let mut parsed = Vec::new();
    for value in values {
        let decision = match value {
            Value::String(option) => match option.as_str() {
                "accept" => ApprovalDecision::Accept,
                "acceptForSession" => ApprovalDecision::AcceptForSession,
                "acceptWithExecpolicyAmendment" => match &proposed_command {
                    Some(command) => ApprovalDecision::AcceptWithExecpolicyAmendment {
                        command: command.clone(),
                    },
                    // The documented decision carries an amendment; a bare
                    // string without a proposal cannot be answered truthfully.
                    None => ApprovalDecision::Other(
                        "acceptWithExecpolicyAmendmentWithoutProposal".to_owned(),
                    ),
                },
                "decline" => ApprovalDecision::Decline,
                "cancel" => ApprovalDecision::Cancel,
                other => ApprovalDecision::Other(sanitize_action_name(other)),
            },
            Value::Object(object) => {
                let amendment = object
                    .get("acceptWithExecpolicyAmendment")
                    .and_then(|value| parse_execpolicy_amendment(config, Some(value)));
                match amendment {
                    Some(command) => ApprovalDecision::AcceptWithExecpolicyAmendment { command },
                    None => match &proposed_command {
                        Some(command) => ApprovalDecision::AcceptWithExecpolicyAmendment {
                            command: command.clone(),
                        },
                        None => ApprovalDecision::Other(
                            "acceptWithExecpolicyAmendmentWithoutProposal".to_owned(),
                        ),
                    },
                }
            }
            _ => ApprovalDecision::Other("unknown_decision".to_owned()),
        };
        if !parsed.contains(&decision) {
            parsed.push(decision);
        }
    }
    if parsed.is_empty() {
        defaults()
    } else {
        parsed
    }
}
