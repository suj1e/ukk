//! JSON-RPC dispatch and local capability authorization.
use super::*;
use crate::plugin_service::PluginService;

#[cfg(test)]
pub(super) fn handle_request(req: &Value, broker: &Broker, auth: &LocalAuth) -> Value {
    handle_request_with_plugins(req, broker, auth, &PluginService::test_empty())
}

pub(super) fn handle_request_with_plugins(
    req: &Value,
    broker: &Broker,
    auth: &LocalAuth,
    plugins: &PluginService,
) -> Value {
    let id = req.get("id").cloned().unwrap_or(Value::Null);
    if req.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
        || req.get("method").and_then(Value::as_str).is_none()
        || id.is_null()
    {
        return json!({
            "jsonrpc":"2.0","id":id,
            "error":{"code":-32600,"message":"invalid JSON-RPC request"}
        });
    }
    match req["method"].as_str().unwrap_or_default() {
        "host.ping" => json!({
            "jsonrpc":"2.0","id":id,
            "result":{"status":"ok","protocol":"ukk-local-spike-2","agent_runtime_read_only":false,"agent_runtime_control":"separate-capability-opt-in","plugin_control":"separate-capability-opt-in","workspace_edits":"local-capability-only"}
        }),
        method @ ("workspace.session.create"
        | "workspace.pane.split"
        | "workspace.pane.close"
        | "workspace.pane.resize"
        | "workspace.tab.add"
        | "workspace.tab.close"
        | "workspace.tab.reorder"
        | "workspace.tab.move") => {
            let params = req.get("params").unwrap_or(&Value::Null);
            if !auth.authorized(params.get("auth").and_then(Value::as_str)) {
                json!({"jsonrpc":"2.0","id":id,
                    "error":{"code":-32030,"message":"local workspace mutation is not authorized"}})
            } else {
                match workspace_rpc::dispatch(method, params) {
                    Ok(result) => json!({"jsonrpc":"2.0","id":id,"result":result}),
                    Err(error) => json!({"jsonrpc":"2.0","id":id,
                        "error":{"code":error.code,"message":error.message}}),
                }
            }
        }
        "workspace.state" => match workspace_watch::snapshot() {
            Ok(value) => json!({"jsonrpc":"2.0","id":id,"result":value}),
            Err(error) => json!({"jsonrpc":"2.0","id":id,
                "error":{"code":-32011,"message":error.to_string()}}),
        },
        "workspace.wait" => {
            let params = req.get("params");
            let after_epoch = params
                .and_then(|p| p.get("after_epoch"))
                .and_then(Value::as_str)
                .unwrap_or("");
            let after_revision = params
                .and_then(|p| p.get("after_revision"))
                .and_then(Value::as_i64)
                .unwrap_or(-1);
            let timeout_ms = params
                .and_then(|p| p.get("timeout_ms"))
                .and_then(Value::as_u64)
                .unwrap_or(10_000);
            match workspace_watch::wait(after_epoch, after_revision, timeout_ms) {
                Ok(value) => json!({"jsonrpc":"2.0","id":id,"result":value}),
                Err(error) => json!({"jsonrpc":"2.0","id":id,
                    "error":{"code":-32011,"message":error.to_string()}}),
            }
        }
        "workspace.snapshot" => match Store::open_default().and_then(|store| store.snapshot()) {
            Ok(snapshot) => json!({"jsonrpc":"2.0","id":id,"result":snapshot}),
            Err(error) => json!({"jsonrpc":"2.0","id":id,
                    "error":{"code":-32011,"message":error.to_string()}}),
        },
        "host.snapshot" => json!({"jsonrpc":"2.0","id":id,"result":broker.snapshot()}),
        "host.wait" => {
            let params = req.get("params");
            let after_epoch = params
                .and_then(|p| p.get("after_epoch"))
                .and_then(Value::as_str);
            let after_revision = params
                .and_then(|p| p.get("after_revision"))
                .and_then(Value::as_u64)
                .unwrap_or_default();
            let timeout_ms = params
                .and_then(|p| p.get("timeout_ms"))
                .and_then(Value::as_u64)
                .unwrap_or(10_000);
            json!({"jsonrpc":"2.0","id":id,
                   "result":broker.wait(after_epoch, after_revision, timeout_ms)})
        }
        method @ ("runtime.status"
        | "runtime.observe"
        | "runtime.attach"
        | "runtime.send"
        | "runtime.interrupt"
        | "runtime.stop"
        | "runtime.detach"
        | "runtime.archive-finished") => {
            let params = req.get("params").unwrap_or(&Value::Null);
            match runtime_api::dispatch(method, params) {
                Ok(result) => json!({"jsonrpc":"2.0","id":id,"result":result}),
                Err(error) => json!({"jsonrpc":"2.0","id":id,
                    "error":{"code":error.code(),"message":error.message()}}),
            }
        }
        "terminal.list" => match HcClient::default().call("terminal.list", json!({})) {
            Ok(result) => json!({"jsonrpc":"2.0","id":id,"result":result}),
            Err(error) => json!({
                "jsonrpc":"2.0","id":id,
                "error":{"code":-32001,"message":error.to_string()}
            }),
        },
        "plugin.snapshot" => json!({"jsonrpc":"2.0","id":id,"result":plugins.snapshot()}),
        method @ ("plugin.enable" | "plugin.disable" | "plugin.invoke") => {
            let params = req.get("params").unwrap_or(&Value::Null);
            match crate::plugin_service::dispatch(method, params, plugins) {
                Ok(result) => json!({"jsonrpc":"2.0","id":id,"result":result}),
                Err(error) => json!({"jsonrpc":"2.0","id":id,"error":error.value()}),
            }
        }
        _ => json!({
            "jsonrpc":"2.0","id":id,
            "error":{"code":-32601,"message":"method not found"}
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ping_describes_capability_boundary() {
        let broker = Broker::new();
        let auth = LocalAuth::test();
        let response = handle_request(
            &json!({
                "jsonrpc":"2.0","id":19,"method":"host.ping","params":{}
            }),
            &broker,
            &auth,
        );
        assert_eq!(response["id"], 19);
        assert_eq!(response["result"]["agent_runtime_read_only"], false);
        assert_eq!(
            response["result"]["agent_runtime_control"],
            "separate-capability-opt-in"
        );
        assert_eq!(
            response["result"]["workspace_edits"],
            "local-capability-only"
        );
    }

    #[test]
    fn rejects_unknown_or_mutating_methods() {
        let broker = Broker::new();
        let auth = LocalAuth::test();
        let response = handle_request(
            &json!({
                "jsonrpc":"2.0","id":20,"method":"terminal.kill","params":{"terminal":"t1"}
            }),
            &broker,
            &auth,
        );
        assert_eq!(response["error"]["code"], -32601);
    }

    #[test]
    fn snapshot_starts_disconnected() {
        let broker = Broker::new();
        let auth = LocalAuth::test();
        let response = handle_request(
            &json!({
                "jsonrpc":"2.0","id":30,"method":"host.snapshot","params":{}
            }),
            &broker,
            &auth,
        );
        assert_eq!(response["result"]["revision"], 0);
        assert_eq!(response["result"]["connected"], false);
        assert_eq!(response["result"]["terminals"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn wait_returns_immediately_for_new_host_epoch() {
        let broker = Broker::new();
        let auth = LocalAuth::test();
        let response = handle_request(
            &json!({
                "jsonrpc":"2.0","id":31,"method":"host.wait",
                "params":{"after_epoch":"previous-host","after_revision":0,"timeout_ms":10000}
            }),
            &broker,
            &auth,
        );
        assert_eq!(response["result"]["revision"], 0);
    }

    #[test]
    fn local_pane_mutations_require_capability() {
        let broker = Broker::new();
        let auth = LocalAuth::test();
        let request = json!({
            "jsonrpc":"2.0","id":51,"method":"workspace.pane.split",
            "params":{"session_id":"s","pane_id":"p","expected_revision":0,
                "axis":"horizontal","content":{"type":"diff"}}
        });
        let denied = handle_request(&request, &broker, &auth);
        assert_eq!(denied["error"]["code"], -32030);
        let mut with_bad_token = request;
        with_bad_token["params"]["auth"] = json!("wrong");
        assert_eq!(
            handle_request(&with_bad_token, &broker, &auth)["error"]["code"],
            -32030
        );
    }

    #[test]
    fn runtime_status_is_not_a_workspace_mutation() {
        let broker = Broker::new();
        let auth = LocalAuth::test();
        let response = handle_request(
            &json!({
                "jsonrpc":"2.0","id":60,"method":"runtime.status","params":{}
            }),
            &broker,
            &auth,
        );
        assert_eq!(response["result"]["capability"], "separate-agent-control");
        assert_eq!(response["result"]["view_close_stops_agent"], false);
    }

    #[test]
    fn runtime_mutations_are_not_reachable_with_only_workspace_auth() {
        let broker = Broker::new();
        let auth = LocalAuth::test();
        let response = handle_request(
            &json!({
                "jsonrpc":"2.0","id":61,"method":"runtime.stop",
                "params":{"core_id":"core","terminal":"t1","request_id":"r1",
                          "intent":"user","auth":auth.test_token()}
            }),
            &broker,
            &auth,
        );
        assert!(response.get("result").is_none(), "{response}");
        assert!(response["error"]["code"].as_i64().unwrap() < 0);
    }

    #[test]
    fn rejects_invalid_requests() {
        let broker = Broker::new();
        let auth = LocalAuth::test();
        let response = handle_request(&json!({"method":"host.ping"}), &broker, &auth);
        assert_eq!(response["error"]["code"], -32600);
    }
}
