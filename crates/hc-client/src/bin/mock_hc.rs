//! Deterministic `hc connect` fixture for the transport tests.
//!
//! Integration tests must not write and execute a script at runtime: on
//! Linux a concurrent `fork`/`exec` can observe the freshly written file as
//! `ETXTBSY`. Cargo builds this binary once and exposes it through
//! `CARGO_BIN_EXE_mock_hc`, so the transport test exercises a real child
//! process without any runtime executable creation.
//!
//! Invoked as `mock_hc connect`; reads one JSON-RPC request from stdin and
//! writes the same fixtures previously embedded in the Python mock.

use serde_json::{json, Value};
use std::io::{self, BufRead, Write};

fn main() -> io::Result<()> {
    if std::env::args().nth(1).as_deref() != Some("connect") {
        std::process::exit(2);
    }

    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;
    let request: Value =
        serde_json::from_str(&line).expect("fixture received a JSON-RPC request line");
    assert_eq!(request["jsonrpc"], "2.0", "fixture expects JSON-RPC 2.0");
    assert_eq!(request["id"], 1, "fixture expects request id 1");

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
