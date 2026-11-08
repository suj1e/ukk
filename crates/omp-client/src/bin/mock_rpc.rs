//! Deterministic mock of `omp --mode rpc` used by integration tests.
//!
//! It implements only documented v1 frames. The scenario is chosen with
//! `--scenario NAME`; the default is `text`. This binary is a test fixture and
//! is never installed.

use serde_json::{json, Value};
use std::io::{self, BufRead, Write};
use std::sync::{Arc, LazyLock, Mutex};
use std::thread;
use std::time::Duration;

static OUT: LazyLock<Arc<Mutex<io::Stdout>>> = LazyLock::new(|| Arc::new(Mutex::new(io::stdout())));

fn emit(value: Value) {
    let mut handle = OUT.lock().unwrap();
    writeln!(handle, "{value}").expect("write stdout");
    handle.flush().expect("flush stdout");
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scenario {
    Text,
    Tool,
    Local,
    Reject,
    NoResponse,
    NoReady,
    Malformed,
    Oversized,
    ExitAfterAck,
    ExitBeforeAck,
    Stderr,
    OutOfOrder,
    DelayedAck,
    UiConfirm,
    Commands,
}

fn scenario_from_args() -> Scenario {
    let mut args = std::env::args().skip(1);
    let mut name = String::from("text");
    while let Some(arg) = args.next() {
        if arg == "--scenario" {
            if let Some(value) = args.next() {
                name = value;
            }
        } else if let Some(value) = arg.strip_prefix("--scenario=") {
            name = value.to_owned();
        }
    }
    match name.as_str() {
        "text" => Scenario::Text,
        "tool" => Scenario::Tool,
        "local" => Scenario::Local,
        "reject" => Scenario::Reject,
        "no-response" => Scenario::NoResponse,
        "no-ready" => Scenario::NoReady,
        "malformed" => Scenario::Malformed,
        "oversized" => Scenario::Oversized,
        "exit-after-ack" => Scenario::ExitAfterAck,
        "exit-before-ack" => Scenario::ExitBeforeAck,
        "stderr" => Scenario::Stderr,
        "out-of-order" => Scenario::OutOfOrder,
        "delayed-ack" => Scenario::DelayedAck,
        "ui-confirm" => Scenario::UiConfirm,
        "commands" => Scenario::Commands,
        other => panic!("unknown mock scenario {other}"),
    }
}

fn text_turn(id: &Value, with_tool: bool) {
    emit(json!({
        "type": "message_start",
        "message": {"role": "assistant", "content": [], "timestamp": 1}
    }));
    emit(json!({
        "type": "message_update",
        "message": {"role": "assistant", "content": []},
        "assistantMessageEvent": {"type": "start", "partial": {"role": "assistant", "content": []}}
    }));
    emit(json!({
        "type": "message_update",
        "message": {"role": "assistant", "content": []},
        "assistantMessageEvent": {
            "type": "text_start", "contentIndex": 0,
            "partial": {"role": "assistant", "content": []}
        }
    }));
    emit(json!({
        "type": "message_update",
        "message": {"role": "assistant", "content": []},
        "assistantMessageEvent": {
            "type": "text_delta", "contentIndex": 0, "delta": "OMP-",
            "partial": {"role": "assistant", "content": []}
        }
    }));
    if with_tool {
        emit(json!({
            "type": "tool_execution_start",
            "toolCallId": "tool-1",
            "toolName": "bash",
            "args": {"command": "echo OMP-TOOL"}
        }));
        emit(json!({
            "type": "tool_execution_update",
            "toolCallId": "tool-1",
            "toolName": "bash",
            "partialResult": {"content": [{"type": "text", "text": "OMP-TOOL\n"}]}
        }));
        emit(json!({
            "type": "tool_execution_end",
            "toolCallId": "tool-1",
            "toolName": "bash",
            "result": {"content": [{"type": "text", "text": "OMP-TOOL\n"}]},
            "isError": false
        }));
        emit(json!({
            "type": "message_start",
            "message": {
                "role": "toolResult", "toolCallId": "tool-1", "toolName": "bash",
                "content": [{"type": "text", "text": "OMP-TOOL\n"}],
                "isError": false, "timestamp": 2
            }
        }));
        emit(json!({
            "type": "message_end",
            "message": {
                "role": "toolResult", "toolCallId": "tool-1", "toolName": "bash",
                "content": [{"type": "text", "text": "OMP-TOOL\n"}],
                "isError": false, "timestamp": 2
            }
        }));
        emit(json!({
            "type": "message_update",
            "message": {"role": "assistant", "content": []},
            "assistantMessageEvent": {
                "type": "text_delta", "contentIndex": 1, "delta": "TOOL-OK",
                "partial": {"role": "assistant", "content": []}
            }
        }));
    }
    emit(json!({
        "type": "message_update",
        "message": {"role": "assistant", "content": []},
        "assistantMessageEvent": {
            "type": "text_delta", "contentIndex": 0, "delta": "MOCK",
            "partial": {"role": "assistant", "content": []}
        }
    }));
    let text = if with_tool { "OMP-TOOL-OK" } else { "OMP-MOCK" };
    emit(json!({
        "type": "message_update",
        "message": {"role": "assistant", "content": []},
        "assistantMessageEvent": {
            "type": "done", "reason": "stop",
            "message": {"role": "assistant", "content": [{"type": "text", "text": text}]}
        }
    }));
    emit(json!({
        "type": "message_end",
        "message": {
            "role": "assistant",
            "content": [{"type": "text", "text": text}],
            "stopReason": "stop", "timestamp": 3
        }
    }));
    emit(json!({
        "type": "turn_end",
        "message": {"role": "assistant", "content": [{"type": "text", "text": text}]},
        "toolResults": []
    }));
    emit(json!({
        "type": "agent_end",
        "messages": [],
        "isTerminal": true
    }));
    let _ = id;
}

fn text_turn_with_start(id: &Value) {
    emit(json!({
        "type": "response",
        "id": id,
        "command": "prompt",
        "success": true,
        "data": {"agentInvoked": true}
    }));
    emit(json!({"type": "agent_start"}));
    emit(json!({"type": "turn_start"}));
    text_turn(id, false);
}

fn main() {
    let scenario = scenario_from_args();
    let pid = std::process::id();
    if scenario == Scenario::NoReady {
        eprintln!("MOCK-PID {pid}");
        // Stay alive without ever disclosing readiness; the client must time
        // out and kill/reap this process.
        let stdin = io::stdin();
        for _ in stdin.lock().lines() {}
        return;
    }
    let max_frame = if scenario == Scenario::Oversized {
        65_536
    } else {
        1_048_576
    };
    emit(json!({
        "type": "ready",
        "protocolVersion": 1,
        "supportedProtocolVersions": [1, 2],
        "maxFrameBytes": max_frame,
        "maxReassembledFrameBytes": 67_108_864
    }));
    emit(json!({"type": "available_commands_update", "commands": []}));
    if scenario == Scenario::Stderr {
        eprintln!("mock stderr line one");
        eprintln!("mock stderr line two sk-{}", "a1b2c3d4".repeat(3));
    }
    let stdin = io::stdin();
    loop {
        let mut line = String::new();
        match stdin.lock().read_line(&mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let command = value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let id = value.get("id").cloned().unwrap_or(Value::Null);
        if command == "prompt" {
            // omp emits widget UI updates even headless; the host must not
            // deadlock and must not answer fire-and-forget methods.
            emit(json!({
                "type": "extension_ui_request",
                "id": "widget-1",
                "method": "setWidget",
                "widgetKey": "autoresearch",
                "widgetLines": ["mock"]
            }));
        }
        match (scenario, command.as_str()) {
            (Scenario::NoResponse, _) => {}
            (Scenario::ExitAfterAck, "prompt") => {
                emit(json!({
                    "type": "response", "id": id, "command": "prompt",
                    "success": true, "data": {"agentInvoked": true}
                }));
                std::process::exit(3);
            }
            (Scenario::ExitBeforeAck, "prompt") => {
                std::process::exit(3);
            }
            (Scenario::Reject, "prompt") => {
                emit(json!({
                    "type": "response", "id": id, "command": "prompt",
                    "success": false,
                    "error": "Model not found: provider/model",
                    "code": "model_not_found"
                }));
            }
            (Scenario::Local, "prompt") => {
                emit(json!({
                    "type": "response", "id": id, "command": "prompt",
                    "success": true, "data": {"agentInvoked": false}
                }));
                emit(json!({"type": "command_output", "text": "local handled"}));
                emit(json!({"type": "prompt_result", "id": id, "agentInvoked": false}));
            }
            (Scenario::Malformed, "prompt") => {
                emit(json!({
                    "type": "response", "id": id, "command": "prompt",
                    "success": true, "data": {"agentInvoked": true}
                }));
                {
                    let mut handle = OUT.lock().unwrap();
                    writeln!(handle, "{{\"type\":").unwrap();
                    handle.flush().unwrap();
                }
                emit(json!({"type": "agent_start"}));
                text_turn(&id, false);
            }
            (Scenario::Oversized, "prompt") => {
                emit(json!({
                    "type": "response", "id": id, "command": "prompt",
                    "success": true, "data": {"agentInvoked": true}
                }));
                emit(json!({
                    "type": "notice", "level": "info",
                    "message": "x".repeat(70_000)
                }));
                emit(json!({"type": "agent_start"}));
                text_turn(&id, false);
            }
            (Scenario::Stderr, "prompt") => {
                eprintln!("mock stderr during prompt");
                emit(json!({
                    "type": "response", "id": id, "command": "prompt",
                    "success": true, "data": {"agentInvoked": true}
                }));
                emit(json!({"type": "agent_start"}));
                text_turn(&id, false);
            }
            (Scenario::OutOfOrder, "prompt") => {
                let id = id.clone();
                thread::spawn(move || {
                    thread::sleep(Duration::from_millis(350));
                    emit(json!({
                        "type": "response", "id": id, "command": "prompt",
                        "success": true, "data": {"agentInvoked": true}
                    }));
                });
            }
            (Scenario::OutOfOrder, "abort") => {
                emit(json!({
                    "type": "response", "id": id, "command": "abort",
                    "success": true
                }));
            }
            (Scenario::DelayedAck, "prompt") => {
                emit(json!({
                    "type": "message_update",
                    "message": {"role": "assistant", "content": []},
                    "assistantMessageEvent": {
                        "type": "text_delta", "contentIndex": 0, "delta": "EARLY",
                        "partial": {"role": "assistant", "content": []}
                    }
                }));
                emit(json!({
                    "type": "response", "id": id, "command": "prompt",
                    "success": true, "data": {"agentInvoked": true}
                }));
                emit(json!({"type": "agent_start"}));
                text_turn(&id, false);
            }
            (Scenario::UiConfirm, "prompt") => {
                emit(json!({
                    "type": "extension_ui_request",
                    "id": "confirm-1",
                    "method": "confirm",
                    "title": "Continue?",
                    "message": "mock confirmation"
                }));
                // Block until the host answers the UI request. Without an
                // answer this handler would never finish, reproducing the
                // documented deadlock.
                let mut answer = String::new();
                io::stdin().lock().read_line(&mut answer).ok();
                let cancelled = serde_json::from_str::<Value>(&answer)
                    .ok()
                    .and_then(|value| value.get("cancelled").and_then(Value::as_bool))
                    .unwrap_or(false);
                emit(json!({
                    "type": "notice", "level": "info",
                    "message": if cancelled { "ui request cancelled" } else { "ui request answered" }
                }));
                emit(json!({
                    "type": "response", "id": id, "command": "prompt",
                    "success": true, "data": {"agentInvoked": true}
                }));
                emit(json!({"type": "agent_start"}));
                text_turn(&id, false);
            }
            (Scenario::Commands, "steer" | "follow_up" | "abort" | "prompt") => {
                emit(json!({
                    "type": "response", "id": id, "command": command,
                    "success": true,
                    "data": {
                        "received": command,
                        "message": value.get("message").cloned().unwrap_or(Value::Null)
                    }
                }));
            }
            (Scenario::Text | Scenario::Tool, "prompt") => {
                emit(json!({
                    "type": "response", "id": id, "command": "prompt",
                    "success": true, "data": {"agentInvoked": true}
                }));
                emit(json!({"type": "agent_start"}));
                emit(json!({"type": "turn_start"}));
                text_turn(&id, scenario == Scenario::Tool);
            }
            (_, "prompt") => {
                text_turn_with_start(&id);
            }
            _ => {
                emit(json!({
                    "type": "response", "id": id, "command": command,
                    "success": true
                }));
            }
        }
    }
}
