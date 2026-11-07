//! Deterministic `hc connect` fixture for transport and framed-protocol tests.
//!
//! Cargo builds this binary once and exposes it through
//! `CARGO_BIN_EXE_mock_hc`, so tests never create an executable at runtime
//! (a concurrent `fork`/`exec` can observe such a file as `ETXTBSY` on Linux).
//!
//! Plain mode keeps the original one-request fixtures. A `hello` request with
//! `framed: true` switches to the documented frame format and a small scripted
//! control server. Scenario names are terminal addresses so a test selects a
//! behaviour without changing global environment variables:
//!
//! * `t-partial`  — attach emits a truncated frame header, then EOF.
//! * `t-oversized` — attach declares a payload above the 16 MiB limit.
//! * `t-finished` — attach fails with `finished`.
//! * `existing`   — `terminal.get` returns a matching live terminal.

use serde_json::{json, Value};
use std::io::{self, BufRead, Read, Write};

const MAX_FRAME_PAYLOAD: u32 = 16 * 1024 * 1024;

fn main() -> io::Result<()> {
    if std::env::args().nth(1).as_deref() != Some("connect") {
        std::process::exit(2);
    }
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let mut line = String::new();
    input.read_line(&mut line)?;
    let request: Value =
        serde_json::from_str(&line).expect("fixture received a JSON-RPC request line");
    assert_eq!(request["jsonrpc"], "2.0", "fixture expects JSON-RPC 2.0");
    if request["method"] == "hello" {
        return framed(&mut input);
    }
    assert_eq!(request["id"], 1, "fixture expects request id 1");
    plain(&request)
}

fn plain(request: &Value) -> io::Result<()> {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    match request["method"].as_str().unwrap_or_default() {
        "terminal.list" => writeln!(
            out,
            "{}",
            json!({
                "jsonrpc": "2.0", "id": 1,
                "result": {
                    "terminals": [
                        {"address": "t3", "name": "codex", "argv": ["codex"], "cwd": "/work",
                         "state": "live", "labels": {}, "created_ms": 1, "screen": "exact",
                         "cols": 80, "rows": 24},
                        {"address": "t4", "name": "claude", "argv": ["claude"], "cwd": "/work",
                         "state": "live", "labels": {}, "created_ms": 2, "screen": "exact",
                         "cols": 80, "rows": 24}
                    ],
                    "hidden_finished": 0,
                    "machines": []
                }
            })
        )?,
        "events.subscribe" => {
            writeln!(
                out,
                "{}",
                json!({
                    "jsonrpc": "2.0", "id": 1,
                    "result": {"seq": 10, "terminals": []}
                })
            )?;
            writeln!(
                out,
                "{}",
                json!({
                    "jsonrpc": "2.0", "method": "event",
                    "params": {"seq": 11, "type": "created", "terminal": "t3",
                               "data": {"id": "t3"}}
                })
            )?;
            writeln!(
                out,
                "{}",
                json!({
                    "jsonrpc": "2.0", "method": "missed",
                    "params": {"seq": 18, "terminals": []}
                })
            )?;
        }
        _ => writeln!(
            out,
            "{}",
            json!({
                "jsonrpc": "2.0", "id": 1,
                "error": {"code": -32601, "message": "not found"}
            })
        )?,
    }
    out.flush()
}

fn framed(input: &mut impl BufRead) -> io::Result<()> {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    writeln!(
        out,
        "{}",
        json!({
            "jsonrpc": "2.0", "id": 0,
            "result": {
                "generation": 1, "version": "0.1.18",
                "core": {"id": "mock-core", "name": "mock"},
                "framed": true,
                "methods": [
                    "hello","terminal.list","terminal.get","terminal.run","terminal.attach",
                    "terminal.send","terminal.keys","terminal.interrupt","terminal.kill",
                    "terminal.rm","terminal.keep","terminal.read","events.subscribe",
                    "server.status","server.stop","server.restart"
                ],
                "features": ["framed","attach","events","automatic_input","resize_notifications"]
            }
        })
    )?;
    out.flush()?;

    let mut send_turn = 4_u64;
    let mut next_attach_channel = 3_u32;
    loop {
        let Some((channel, kind, payload)) = read_frame(input)? else {
            return Ok(());
        };
        if kind != 0 || channel != 0 {
            if kind == 2 {
                // A viewer detached its stream: acknowledge with an empty
                // CLOSE, exactly as the core does.
                write_frame(&mut out, channel, 2, &[])?;
                out.flush()?;
            } else if kind == 0 {
                // Echo human input back so a test can prove DATA reached the
                // stream without enabling any process control.
                let mut echo = b"echo:".to_vec();
                echo.extend_from_slice(&payload);
                write_frame(&mut out, channel, 0, &echo)?;
                out.flush()?;
            }
            continue;
        }
        let request: Value = serde_json::from_slice(&payload).expect("framed JSON-RPC request");
        let id = request.get("id").and_then(Value::as_u64).unwrap_or(0);
        let method = request["method"].as_str().unwrap_or_default();
        let params = &request["params"];
        match method {
            "terminal.list" => respond(&mut out, id, list_result())?,
            "terminal.get" => match params["terminal"].as_str().unwrap_or_default() {
                "missing" => error(&mut out, id, "not_found")?,
                "existing" => respond(
                    &mut out,
                    id,
                    terminal("existing", "existing", &["codex".to_string()], "/mock"),
                )?,
                other => respond(
                    &mut out,
                    id,
                    terminal(other, other, &["sleep".into(), "60".into()], "/mock"),
                )?,
            },
            "terminal.run" => {
                if params["name"].as_str() == Some("taken") {
                    error(&mut out, id, "name_taken")?;
                } else {
                    let mut record = terminal(
                        "t7",
                        params["name"].as_str().unwrap_or("probe"),
                        &argv(params),
                        params["cwd"].as_str().unwrap_or("/mock"),
                    );
                    record["keep"] = params["keep"].clone();
                    respond(&mut out, id, record)?;
                }
            }
            "terminal.attach" => {
                let target = params["terminal"].as_str().unwrap_or_default();
                match target {
                    "t-partial" => {
                        // Three bytes of a nine-byte header, then EOF: the
                        // client must report truncation, not resynchronise.
                        out.write_all(&[3, 0, 0])?;
                        out.flush()?;
                        return Ok(());
                    }
                    "t-oversized" => {
                        let mut header = Vec::new();
                        header.extend_from_slice(&3_u32.to_le_bytes());
                        header.push(0);
                        header.extend_from_slice(&(MAX_FRAME_PAYLOAD + 1).to_le_bytes());
                        out.write_all(&header)?;
                        out.flush()?;
                        return Ok(());
                    }
                    "t-finished" => error(&mut out, id, "finished")?,
                    other => {
                        let channel = next_attach_channel;
                        next_attach_channel += 1;
                        respond(
                            &mut out,
                            id,
                            json!({
                                "channel": channel,
                                "terminal": terminal(
                                    other, other, &["sleep".into(), "60".into()], "/mock"
                                ),
                                "automatic_input": true,
                                "resize_notifications": true
                            }),
                        )?;
                        write_frame(&mut out, channel, 0, b"first paint")?;
                        write_frame(&mut out, channel, 3, &[100, 0, 40, 0])?;
                        write_frame(&mut out, channel, 0, b"live output")?;
                        out.flush()?;
                    }
                }
            }
            "terminal.send" => {
                send_turn += 1;
                respond(
                    &mut out,
                    id,
                    json!({"turn": send_turn, "status": "working", "started": true}),
                )?;
            }
            "terminal.keys" => respond(&mut out, id, json!({"turn": 4}))?,
            "terminal.interrupt" => {
                respond(&mut out, id, json!({"outcome": "stopped", "key": "esc"}))?
            }
            "terminal.kill" => {
                let target = params["terminal"].as_str().unwrap_or_default();
                let mut record = terminal(target, target, &["sleep".into(), "60".into()], "/mock");
                record["state"] = json!("finished");
                record["reason"] = json!("killed");
                record["exit_code"] = json!(null);
                respond(&mut out, id, record)?;
            }
            "terminal.rm" => respond(
                &mut out,
                id,
                json!({"removed": params["terminal"].as_str().unwrap_or_default()}),
            )?,
            "terminal.keep" => {
                let target = params["terminal"].as_str().unwrap_or_default();
                let mut record = terminal(target, target, &["sleep".into(), "60".into()], "/mock");
                record["keep"] = params["keep"].clone();
                respond(&mut out, id, record)?;
            }
            "terminal.read" => respond(
                &mut out,
                id,
                json!({"text": "screen", "complete": true, "how": "screen"}),
            )?,
            "events.subscribe" => {
                let low = params["terminal"].as_str() == Some("t-lower");
                let start = if low { 2 } else { 10 };
                respond(
                    &mut out,
                    id,
                    json!({"seq": start, "terminals": [], "channel": 40}),
                )?;
                if low {
                    write_frame(
                        &mut out,
                        40,
                        0,
                        &serde_json::to_vec(&json!({
                            "jsonrpc": "2.0", "method": "event",
                            "params": {"seq": 3, "type": "title", "terminal": "t-lower",
                                       "data": {"title": "after restart"}}
                        }))?,
                    )?;
                    write_frame(
                        &mut out,
                        40,
                        0,
                        &serde_json::to_vec(&json!({
                            "jsonrpc": "2.0", "method": "missed",
                            "params": {"seq": 20, "terminals": []}
                        }))?,
                    )?;
                    out.flush()?;
                    continue;
                }
                write_frame(
                    &mut out,
                    40,
                    0,
                    &serde_json::to_vec(&json!({
                        "jsonrpc": "2.0", "method": "event",
                        "params": {"seq": 11, "type": "title", "terminal": "t1",
                                   "data": {"title": "one"}}
                    }))?,
                )?;
                write_frame(
                    &mut out,
                    40,
                    0,
                    &serde_json::to_vec(&json!({
                        "jsonrpc": "2.0", "method": "event",
                        "params": {"seq": 13, "type": "title", "terminal": "t1",
                                   "data": {"title": "three"}}
                    }))?,
                )?;
                write_frame(
                    &mut out,
                    40,
                    0,
                    &serde_json::to_vec(&json!({
                        "jsonrpc": "2.0", "method": "missed",
                        "params": {"seq": 20, "terminals": []}
                    }))?,
                )?;
                out.flush()?;
            }
            "server.status" => respond(
                &mut out,
                id,
                json!({
                    "core": {"id": "mock-core", "name": "mock"},
                    "version": "0.1.18", "pid": 1, "uptime_ms": 2,
                    "holders": [], "live": 1, "finished": 0
                }),
            )?,
            "server.stop" | "server.restart" => respond(&mut out, id, json!({"stopped": true}))?,
            _ => error(&mut out, id, "invalid_params")?,
        }
        out.flush()?;
    }
}

fn argv(params: &Value) -> Vec<String> {
    params["argv"]
        .as_array()
        .map(|argv| {
            argv.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_else(|| vec!["sleep".into(), "60".into()])
}

fn terminal(address: &str, name: &str, argv: &[String], cwd: &str) -> Value {
    json!({
        "address": address,
        "name": name,
        "argv": argv,
        "cwd": cwd,
        "state": "live",
        "created_ms": 7,
        "screen": "exact",
        "cols": 80,
        "rows": 24,
        "labels": {},
        "parent": "t1"
    })
}

fn list_result() -> Value {
    json!({
        "terminals": [
            terminal("t3", "codex", &["codex".into()], "/work"),
            terminal("t4", "claude", &["claude".into()], "/work")
        ],
        "hidden_finished": 0,
        "machines": []
    })
}

fn respond(out: &mut impl Write, id: u64, result: Value) -> io::Result<()> {
    let payload = serde_json::to_vec(&json!({
        "jsonrpc": "2.0", "id": id, "result": result
    }))
    .expect("response serializes");
    write_frame(out, 0, 0, &payload)
}

fn error(out: &mut impl Write, id: u64, kind: &str) -> io::Result<()> {
    let payload = serde_json::to_vec(&json!({
        "jsonrpc": "2.0", "id": id,
        "error": {"code": -32000, "message": kind, "data": {"kind": kind}}
    }))
    .expect("error serializes");
    write_frame(out, 0, 0, &payload)
}

fn write_frame(out: &mut impl Write, channel: u32, kind: u8, payload: &[u8]) -> io::Result<()> {
    out.write_all(&channel.to_le_bytes())?;
    out.write_all(&[kind])?;
    out.write_all(&(payload.len() as u32).to_le_bytes())?;
    out.write_all(payload)
}

fn read_frame(input: &mut impl Read) -> io::Result<Option<(u32, u8, Vec<u8>)>> {
    let mut header = [0u8; 9];
    let mut read = 0;
    while read < header.len() {
        match input.read(&mut header[read..]) {
            Ok(0) if read == 0 => return Ok(None),
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "partial frame header",
                ))
            }
            Ok(count) => read += count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    let channel = u32::from_le_bytes(header[0..4].try_into().expect("4 bytes"));
    let kind = header[4];
    let length = u32::from_le_bytes(header[5..9].try_into().expect("4 bytes")) as usize;
    let mut payload = vec![0u8; length];
    input.read_exact(&mut payload)?;
    Ok(Some((channel, kind, payload)))
}
