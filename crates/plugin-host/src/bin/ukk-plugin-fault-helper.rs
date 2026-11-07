//! Fault-injection helper used by the supervisor integration tests.
//!
//! It speaks the same protocol as the sample helper but misbehaves on command,
//! selected with `--mode <name>`:
//!
//! * `crash` — exits with code 7 instead of answering a command;
//! * `stall` — answers nothing after hello (request deadline);
//! * `oversize` — sends a frame larger than the host frame limit;
//! * `garbage` — sends a non-JSON line;
//! * `noisy` — answers a request id that was never sent;
//! * `forged` — claims contributions the manifest does not declare;
//! * `bad-identity` — reports another plugin id;
//! * `hello-stall` — never answers the handshake;
//! * `evil-error` — rejects with newline/ANSI/long helper-authored text;
//! * `env-probe` — reports which environment variables it inherited.
//!
//! It exists only to prove crash containment, protocol enforcement and timeout
//! handling; it is never installed by the product.
use std::io::{self, BufRead, Write};
use std::process::ExitCode;
use std::thread;
use std::time::Duration;

use serde_json::{json, Value};
use ukk_plugin_api::wire::{
    RequestFrame, ResponseFrame, METHOD_HEALTH, METHOD_HELLO, METHOD_INVOKE, METHOD_SHUTDOWN,
};

const PLUGIN_ID: &str = "dev.ukk.fault.plugin";

fn main() -> ExitCode {
    let mut mode = "crash".to_owned();
    let mut identity = PLUGIN_ID.to_owned();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--mode" => mode = args.next().unwrap_or(mode),
            "--id" => identity = args.next().unwrap_or(identity),
            _ => {}
        }
    }

    let stdin = io::stdin();
    let mut reader = stdin.lock();
    let stdout = io::stdout();
    let mut output = stdout.lock();

    if mode == "hello-stall" {
        thread::sleep(Duration::from_secs(30));
        return ExitCode::SUCCESS;
    }

    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => return ExitCode::SUCCESS,
            Ok(_) => {}
            Err(_) => return ExitCode::FAILURE,
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
            let _ = writeln!(
                output,
                "{}",
                to_line(&ResponseFrame::err(
                    0,
                    "invalid_request",
                    "cannot parse frame"
                ))
            );
            let _ = output.flush();
            continue;
        };
        // Notifications are never answered.
        if value.get("event").is_some() && value.get("id").is_none() {
            continue;
        }
        let Ok(request) = serde_json::from_value::<RequestFrame>(value) else {
            let _ = writeln!(
                output,
                "{}",
                to_line(&ResponseFrame::err(
                    0,
                    "invalid_request",
                    "cannot parse request"
                ))
            );
            let _ = output.flush();
            continue;
        };
        let shutdown = request.method == METHOD_SHUTDOWN;
        match request.method.as_str() {
            METHOD_HELLO => {
                let plugin_id = if mode == "bad-identity" {
                    "dev.ukk.someone.else"
                } else {
                    identity.as_str()
                };
                let contributions = if mode == "forged" {
                    json!({
                        "commands": ["fault.ping", "forged.capture-screen"],
                        "views": ["fault.view"],
                    })
                } else {
                    json!({ "commands": ["fault.ping"], "views": [] })
                };
                let body = json!({
                    "protocol": ukk_plugin_api::HELPER_PROTOCOL_VERSION,
                    "plugin_id": plugin_id,
                    "contributions": contributions,
                });
                let _ = writeln!(output, "{}", to_line(&ResponseFrame::ok(request.id, body)));
                let _ = output.flush();
                if mode == "mute" {
                    // Stop reading: the host's bounded event queue must fill
                    // and drop instead of blocking the publisher.
                    thread::sleep(Duration::from_secs(30));
                    return ExitCode::SUCCESS;
                }
            }
            METHOD_HEALTH => {
                let _ = writeln!(
                    output,
                    "{}",
                    to_line(&ResponseFrame::ok(request.id, json!({ "status": "ready" })))
                );
                let _ = output.flush();
            }
            METHOD_INVOKE => match mode.as_str() {
                "crash" => return ExitCode::from(7),
                "stall" => {
                    // Do not read or answer: the host must time out.
                    thread::sleep(Duration::from_secs(30));
                    return ExitCode::SUCCESS;
                }
                "oversize" => {
                    let blob = "x".repeat(200 * 1024);
                    let body = json!({ "blob": blob });
                    let _ = writeln!(output, "{}", to_line(&ResponseFrame::ok(request.id, body)));
                    let _ = output.flush();
                }
                "evil-error" => {
                    let message = format!("line one\nline two\u{1b}[31mred {}", "x".repeat(500));
                    let _ = writeln!(
                        output,
                        "{}",
                        to_line(&ResponseFrame::err(request.id, "evil\ncode", &message))
                    );
                    let _ = output.flush();
                }
                "env-probe" => {
                    let body = json!({
                        "path_set": std::env::var_os("PATH").is_some(),
                        "home_set": std::env::var_os("HOME").is_some(),
                        "plugin_id_env": std::env::var("UKK_PLUGIN_ID").ok(),
                        "protocol_env": std::env::var("UKK_PLUGIN_PROTOCOL").ok(),
                    });
                    let _ = writeln!(output, "{}", to_line(&ResponseFrame::ok(request.id, body)));
                    let _ = output.flush();
                }
                "garbage" => {
                    let _ = writeln!(output, "this is not a protocol frame");
                    let _ = output.flush();
                }
                "noisy" => {
                    let _ = writeln!(
                        output,
                        "{}",
                        to_line(&ResponseFrame::ok(
                            request.id + 1000,
                            json!({ "ignored": true })
                        ))
                    );
                    let _ = output.flush();
                }
                _ => {
                    let _ = writeln!(
                        output,
                        "{}",
                        to_line(&ResponseFrame::ok(request.id, json!({ "message": "pong" })))
                    );
                    let _ = output.flush();
                }
            },
            METHOD_SHUTDOWN => {
                let _ = writeln!(
                    output,
                    "{}",
                    to_line(&ResponseFrame::ok(request.id, json!({ "stopped": true })))
                );
                let _ = output.flush();
                return ExitCode::SUCCESS;
            }
            _ => {
                let _ = writeln!(
                    output,
                    "{}",
                    to_line(&ResponseFrame::err(
                        request.id,
                        "unknown_method",
                        "unsupported"
                    ))
                );
                let _ = output.flush();
            }
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
