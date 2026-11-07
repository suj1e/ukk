//! Sample plugin helper: a minimal, harmless reference implementation of the
//! ukk helper protocol.
//!
//! It reads newline-delimited JSON on stdin and answers on stdout. It touches
//! no files, no network, no screen and no environment beyond its own stdio, so
//! enabling it proves the broker path without needing any privileged
//! capability.
//!
//! Wire it into the sample manifest at
//! `plugins/sample-session-action/plugin.toml`. The installer resolves the
//! entrypoint name to this executable's absolute path; the manifest alone can
//! never choose a program to run.
use std::io::{self, BufRead, Write};
use std::process::ExitCode;

use serde_json::{json, Value};
use ukk_plugin_api::wire::{
    HelloContributions, HelloResult, RequestFrame, ResponseFrame, METHOD_HEALTH, METHOD_HELLO,
    METHOD_INVOKE, METHOD_SHUTDOWN,
};

const PLUGIN_ID: &str = "dev.ukk.sample.session-action";
const COMMAND_PING: &str = "sample.session.ping";

fn main() -> ExitCode {
    let stdin = io::stdin();
    let mut reader = stdin.lock();
    let stdout = io::stdout();
    let mut output = stdout.lock();
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => return ExitCode::SUCCESS,
            Ok(_) => {}
            Err(error) => {
                eprintln!("sample-session-helper: stdin read failed: {error}");
                return ExitCode::FAILURE;
            }
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let value: Value = match serde_json::from_str(trimmed) {
            Ok(value) => value,
            Err(error) => {
                let _ = writeln!(
                    output,
                    "{}",
                    to_line(&ResponseFrame::err(
                        0,
                        "invalid_request",
                        &error.to_string()
                    ))
                );
                let _ = output.flush();
                return ExitCode::FAILURE;
            }
        };
        // Host event notifications carry no `id` and must never be answered;
        // answering one is an unsolicited response, which the host treats as a
        // protocol violation.
        if value.get("event").is_some() && value.get("id").is_none() {
            continue;
        }
        let request: RequestFrame = match serde_json::from_value(value) {
            Ok(request) => request,
            Err(error) => {
                let _ = writeln!(
                    output,
                    "{}",
                    to_line(&ResponseFrame::err(
                        0,
                        "invalid_request",
                        &error.to_string()
                    ))
                );
                let _ = output.flush();
                return ExitCode::FAILURE;
            }
        };
        let shutdown = request.method == METHOD_SHUTDOWN;
        let response = handle(&request);
        if writeln!(output, "{}", to_line(&response)).is_err() {
            return ExitCode::SUCCESS;
        }
        if output.flush().is_err() {
            return ExitCode::SUCCESS;
        }
        if shutdown {
            return ExitCode::SUCCESS;
        }
    }
}

fn to_line(response: &ResponseFrame) -> String {
    serde_json::to_string(response).unwrap_or_else(|_| {
        r#"{"id":0,"ok":false,"error":{"code":"encode_failed","message":"cannot encode response"}}"#
            .to_owned()
    })
}

fn handle(request: &RequestFrame) -> ResponseFrame {
    match request.method.as_str() {
        METHOD_HELLO => ResponseFrame::ok(
            request.id,
            serde_json::to_value(HelloResult {
                protocol: ukk_plugin_api::HELPER_PROTOCOL_VERSION,
                plugin_id: PLUGIN_ID.to_owned(),
                contributions: HelloContributions {
                    commands: vec![COMMAND_PING.to_owned()],
                    views: vec![
                        "sample.session.action".to_owned(),
                        "sample.session.tab".to_owned(),
                        "sample.session.overlay".to_owned(),
                    ],
                },
            })
            .unwrap_or(Value::Null),
        ),
        METHOD_HEALTH => ResponseFrame::ok(request.id, json!({ "status": "ready" })),
        METHOD_INVOKE => {
            let command = request
                .params
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if command != COMMAND_PING {
                return ResponseFrame::err(
                    request.id,
                    "unknown_command",
                    "this helper only implements sample.session.ping",
                );
            }
            let session_id = request
                .params
                .get("session_id")
                .cloned()
                .unwrap_or(Value::Null);
            ResponseFrame::ok(
                request.id,
                json!({
                    "message": "pong",
                    "command": COMMAND_PING,
                    "session_id": session_id,
                    "capabilities_used": [],
                }),
            )
        }
        METHOD_SHUTDOWN => ResponseFrame::ok(request.id, json!({ "stopped": true })),
        other => ResponseFrame::err(
            request.id,
            "unknown_method",
            &format!("unsupported method `{other}`"),
        ),
    }
}
