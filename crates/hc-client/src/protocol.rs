//! Plain-mode JSON-RPC line helpers.
//!
//! Plain mode is one JSON object per line in both directions. The typed
//! framed client only uses this for `hello`; the legacy one-shot call/watch
//! paths keep it for compatibility with existing Host read-only probes.

use crate::error::{HcError, Result};
use serde_json::{json, Value};
use std::io::{self, BufRead, Write};
use std::process::Child;

pub(crate) fn write_request(child: &mut Child, id: u64, method: &str, params: Value) -> Result<()> {
    let stdin = child
        .stdin
        .as_mut()
        .ok_or_else(|| HcError::Protocol("hc stdin unavailable".into()))?;
    serde_json::to_writer(
        &mut *stdin,
        &json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}),
    )?;
    stdin.write_all(b"\n")?;
    stdin.flush()?;
    Ok(())
}

pub(crate) fn parse_reply(line: &str, id: u64) -> Result<Value> {
    let value: Value = serde_json::from_str(line)?;
    if value.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(HcError::Protocol("response is not JSON-RPC 2.0".into()));
    }
    if value.get("id") != Some(&json!(id)) {
        return Err(HcError::Protocol(format!(
            "unexpected response id: {:?}",
            value.get("id")
        )));
    }
    if let Some(error) = value.get("error") {
        return Err(HcError::Remote(crate::error::RemoteError::from_value(
            error,
        )));
    }
    value
        .get("result")
        .cloned()
        .ok_or_else(|| HcError::Protocol("response missing result".into()))
}

#[allow(dead_code)]
pub(crate) fn read_line_bounded(
    reader: &mut impl BufRead,
    limit: usize,
) -> io::Result<Option<String>> {
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Ok(None);
    }
    if line.len() > limit {
        return Err(io::Error::other("JSON-RPC line too large"));
    }
    Ok(Some(line))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_terminal_list_result() {
        let line = r#"{"jsonrpc":"2.0","id":1,"result":{"terminals":[{"address":"t3","argv":["codex"],"cwd":"/work","state":"live","created_ms":1,"screen":"exact","cols":80,"rows":24,"labels":{}}],"hidden_finished":0,"machines":[]}}"#;
        let result = parse_reply(line, 1).expect("valid reply");
        assert_eq!(result["terminals"][0]["address"], "t3");
    }

    #[test]
    fn preserves_remote_errors_as_typed_kinds() {
        let line = r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32000,"message":"unavailable","data":{"kind":"unavailable","hint":"later"}}}"#;
        match parse_reply(line, 1) {
            Err(HcError::Remote(error)) => {
                assert_eq!(error.kind, crate::error::RemoteKind::Unavailable);
                assert_eq!(error.hint.as_deref(), Some("later"));
            }
            other => panic!("unexpected result: {other:?}"),
        }
    }

    #[test]
    fn rejects_wrong_request_id() {
        let line = r#"{"jsonrpc":"2.0","id":2,"result":{}}"#;
        assert!(matches!(parse_reply(line, 1), Err(HcError::Protocol(_))));
    }

    #[test]
    fn rejects_non_json_rpc() {
        assert!(matches!(
            parse_reply(r#"{"id":1,"result":{}}"#, 1),
            Err(HcError::Protocol(_))
        ));
    }

    #[test]
    fn rejects_missing_result() {
        assert!(matches!(
            parse_reply(r#"{"jsonrpc":"2.0","id":1}"#, 1),
            Err(HcError::Protocol(_))
        ));
    }
}
