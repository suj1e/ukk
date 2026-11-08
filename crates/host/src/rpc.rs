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
            "result":{"status":"ok","protocol":"ukk-local-spike-2","agent_runtime_read_only":false,"agent_runtime_control":"separate-capability-opt-in","plugin_control":"separate-capability-opt-in","workspace_edits":"local-capability-only","omp_rpc":omp_service::status()}
        }),
        method @ ("workspace.space.create"
        | "workspace.space.rename"
        | "workspace.session.create"
        | "workspace.agent.bind"
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
                match workspace_rpc::dispatch(broker, method, params) {
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
        method @ ("omp.snapshot" | "omp.wait" | "omp.start" | "omp.prompt" | "omp.abort"
        | "omp.stop") => {
            let params = req.get("params").unwrap_or(&Value::Null);
            match omp_service::dispatch(method, params) {
                Ok(result) => json!({"jsonrpc":"2.0","id":id,"result":result}),
                Err(error) => json!({"jsonrpc":"2.0","id":id,
                    "error":{"code":error.code,"message":error.message}}),
            }
        }
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
    fn omp_mutations_are_denied_without_agent_control() {
        let broker = Broker::new();
        let auth = LocalAuth::test();
        let response = handle_request(
            &json!({
                "jsonrpc":"2.0","id":70,"method":"omp.start",
                "params":{
                    "session_id":"s1",
                    "cwd": std::env::temp_dir().to_string_lossy(),
                    "request_id":"r1",
                    "intent":"user",
                    "control":"wrong-capability"
                }
            }),
            &broker,
            &auth,
        );
        assert_eq!(response["error"]["code"], -32030);
        // The workspace metadata capability is never accepted here.
        let response = handle_request(
            &json!({
                "jsonrpc":"2.0","id":71,"method":"omp.prompt",
                "params":{
                    "session_id":"s1","message":"hi","request_id":"r2",
                    "intent":"user","auth":auth.test_token()
                }
            }),
            &broker,
            &auth,
        );
        assert_eq!(response["error"]["code"], -32030);
    }

    #[test]
    fn omp_read_only_methods_do_not_require_a_capability() {
        let broker = Broker::new();
        let auth = LocalAuth::test();
        for (id, method) in [(80, "omp.snapshot"), (81, "omp.wait")] {
            let response = handle_request(
                &json!({
                    "jsonrpc":"2.0","id":id,"method":method,
                    "params":{"session_id":"missing"}
                }),
                &broker,
                &auth,
            );
            assert_eq!(
                response["error"]["code"], -32034,
                "{method} must reach the read-only omp surface without a token"
            );
        }
    }

    #[test]
    fn ping_reports_omp_capability_boundary() {
        let broker = Broker::new();
        let auth = LocalAuth::test();
        let response = handle_request(
            &json!({
                "jsonrpc":"2.0","id":90,"method":"host.ping","params":{}
            }),
            &broker,
            &auth,
        );
        assert_eq!(
            response["result"]["omp_rpc"]["capability"],
            "separate-agent-control"
        );
        assert_eq!(response["result"]["omp_rpc"]["view_close_stops_omp"], false);
        assert_eq!(
            response["result"]["omp_rpc"]["host_restart_reattaches"],
            false
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
    fn space_mutations_require_workspace_capability_and_never_expose_runtime_fields() {
        let broker = Broker::new();
        let auth = LocalAuth::test();
        for (method, params) in [
            (
                "workspace.space.create",
                json!({"request_id":"00000000-0000-4000-8000-000000000000",
                       "title":"ukk","project_root":"/work"}),
            ),
            (
                "workspace.space.rename",
                json!({"space_id":"space-1","title":"ukk"}),
            ),
        ] {
            let request = json!({
                "jsonrpc":"2.0","id":52,"method":method,"params":params.clone()
            });
            assert_eq!(
                handle_request(&request, &broker, &auth)["error"]["code"],
                -32030
            );
            let mut with_control = request.clone();
            with_control["params"]["control"] = json!("agent-control-token");
            with_control["params"]["intent"] = json!("user");
            assert_eq!(
                handle_request(&with_control, &broker, &auth)["error"]["code"],
                -32030
            );
            let mut with_runtime = request;
            with_runtime["params"]["terminal"] = json!("t1");
            with_runtime["params"]["core_id"] = json!("core");
            assert_eq!(
                handle_request(&with_runtime, &broker, &auth)["error"]["code"],
                -32030
            );
        }
        let agent = handle_request(
            &json!({
                "jsonrpc":"2.0","id":53,"method":"workspace.agent.create",
                "params":{"auth":auth.test_token()}
            }),
            &broker,
            &auth,
        );
        assert_eq!(agent["error"]["code"], -32601);
    }

    #[test]
    fn runtime_send_preflight_denial_is_audited() {
        let broker = Broker::new();
        let auth = LocalAuth::test();
        let request = json!({
            "jsonrpc":"2.0","id":63,"method":"runtime.send",
            "params":{
                "core_id":"core","terminal":"t1","text":"echo hi",
                "request_id":"audit-denial-1","intent":"automation"
            }
        });
        let (response, entries) =
            runtime_api::with_test_audit(|| handle_request(&request, &broker, &auth));
        assert!(response.get("result").is_none(), "{response}");
        assert!(
            response["error"]["code"].as_i64().unwrap() < 0,
            "{response}"
        );
        assert!(
            entries.iter().any(|entry| {
                entry.request_id == "audit-denial-1"
                    && entry.action == "runtime.send"
                    && entry.outcome.starts_with("denied:")
            }),
            "preflight denial was not audited: {entries:?}"
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
    fn agent_bind_requires_workspace_capability_and_never_exposes_agent_lifecycle() {
        let broker = Broker::new();
        let auth = LocalAuth::test();
        let params = json!({
            "request_id":"00000000-0000-4000-8000-000000000000",
            "session_id":"session-1",
            "core_id":"core-1",
            "terminal_address":"t7",
            "adapter":"codex"
        });
        let request = json!({
            "jsonrpc":"2.0","id":70,"method":"workspace.agent.bind","params":params.clone()
        });
        assert_eq!(
            handle_request(&request, &broker, &auth)["error"]["code"],
            -32030
        );
        let mut with_control = request.clone();
        with_control["params"]["control"] = json!("agent-control-token");
        with_control["params"]["intent"] = json!("user");
        assert_eq!(
            handle_request(&with_control, &broker, &auth)["error"]["code"],
            -32030
        );
        for method in [
            "workspace.agent.start",
            "workspace.agent.send",
            "workspace.agent.stop",
            "workspace.agent.kill",
        ] {
            let response = handle_request(
                &json!({
                    "jsonrpc":"2.0","id":71,"method":method,
                    "params":{"auth":auth.test_token()}
                }),
                &broker,
                &auth,
            );
            assert_eq!(response["error"]["code"], -32601, "{method}");
        }
    }

    #[test]
    fn authorized_agent_bind_persists_verified_metadata_through_handle_request() {
        static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let dir = std::env::temp_dir().join(format!(
            "ukk-bind-rpc-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let previous = std::env::var_os("UKK_DB_PATH");
        std::env::set_var("UKK_DB_PATH", dir.join("workspace.sqlite3"));
        struct Restore(Option<std::ffi::OsString>);
        impl Drop for Restore {
            fn drop(&mut self) {
                match self.0.take() {
                    Some(value) => std::env::set_var("UKK_DB_PATH", value),
                    None => std::env::remove_var("UKK_DB_PATH"),
                }
            }
        }
        let _restore = Restore(previous);

        let broker = Broker::new();
        broker
            .apply(&json!({"kind":"snapshot","data":{
                "seq":1,
                "core":{"id":"core-1","name":"hc"},
                "terminals":[
                    {"address":"t7","state":"live","cwd":"/repo","argv":["codex"]},
                    {"address":"t8","state":"finished","cwd":"/repo","argv":["codex"]}
                ]
            }}))
            .unwrap();
        let auth = LocalAuth::test();
        let space_id = "00000000-0000-4000-8000-000000000001";
        let space = handle_request(
            &json!({
                "jsonrpc":"2.0","id":80,"method":"workspace.space.create",
                "params":{"auth":auth.test_token(),"request_id":space_id,
                          "title":"ukk","project_root":"/work"}
            }),
            &broker,
            &auth,
        );
        assert_eq!(space["result"]["id"], space_id, "{space}");
        let session_id = "00000000-0000-4000-8000-000000000002";
        let session = handle_request(
            &json!({
                "jsonrpc":"2.0","id":81,"method":"workspace.session.create",
                "params":{"auth":auth.test_token(),"request_id":session_id,
                          "space_id":space_id,"title":"bound"}
            }),
            &broker,
            &auth,
        );
        assert_eq!(session["result"]["id"], session_id, "{session}");
        let request_id = "00000000-0000-4000-8000-000000000003";
        let bind = |id: u64| {
            handle_request(
                &json!({
                    "jsonrpc":"2.0","id":id,"method":"workspace.agent.bind",
                    "params":{"auth":auth.test_token(),"request_id":request_id,
                              "session_id":session_id,"core_id":"core-1",
                              "terminal_address":"t7","adapter":"codex"}
                }),
                &broker,
                &auth,
            )
        };
        let first = bind(82);
        assert_eq!(first["result"]["id"], request_id, "{first}");
        assert_eq!(first["result"]["terminal_address"], "t7");
        let repeated = bind(83);
        assert_eq!(repeated["result"], first["result"]);

        let snapshot = handle_request(
            &json!({
                "jsonrpc":"2.0","id":84,"method":"workspace.snapshot",
                "params":{}
            }),
            &broker,
            &auth,
        );
        assert_eq!(
            snapshot["result"]["agent_runs"].as_array().unwrap().len(),
            1
        );
        assert_eq!(snapshot["result"]["agent_runs"][0]["id"], request_id);

        // A finished terminal is not bindable through the same RPC path.
        let finished = "00000000-0000-4000-8000-000000000004";
        let error = handle_request(
            &json!({
                "jsonrpc":"2.0","id":85,"method":"workspace.agent.bind",
                "params":{"auth":auth.test_token(),"request_id":finished,
                          "session_id":session_id,"core_id":"core-1",
                          "terminal_address":"t8","adapter":"codex"}
            }),
            &broker,
            &auth,
        );
        assert_eq!(error["error"]["code"], -32062, "{error}");

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn rejects_invalid_requests() {
        let broker = Broker::new();
        let auth = LocalAuth::test();
        let response = handle_request(&json!({"method":"host.ping"}), &broker, &auth);
        assert_eq!(response["error"]["code"], -32600);
    }
}
