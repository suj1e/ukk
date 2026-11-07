//! JSON-RPC surface for `runtime.*`.
//!
//! Only non-streaming actions are exposed over the line protocol. Framed
//! attach stays in the typed client/desktop layer because a line RPC reply
//! cannot carry a backpressured byte stream.
//!
//! Every mutating call needs `request_id`, `intent: "user"` and a `control`
//! value from the **separate** agent-control token file. The workspace
//! `auth` token is ignored here by construction.

use super::runtime::{
    Intent, RunBinding, RuntimeAction, RuntimeCall, RuntimeError, RuntimeOutcome, RuntimeService,
};
use super::runtime_auth::{AgentControlAuth, AuditLog};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use ukk_hc_client::HcClient;

pub const METHODS: &[&str] = &[
    "runtime.status",
    "runtime.observe",
    "runtime.attach",
    "runtime.send",
    "runtime.interrupt",
    "runtime.stop",
    "runtime.detach",
    "runtime.archive-finished",
];

fn global_cache() -> Arc<Mutex<super::runtime::RuntimeCache>> {
    static CACHE: std::sync::LazyLock<Arc<Mutex<super::runtime::RuntimeCache>>> =
        std::sync::LazyLock::new(|| Arc::new(Mutex::new(super::runtime::RuntimeCache::new())));
    Arc::clone(&CACHE)
}

pub fn dispatch(method: &str, params: &Value) -> Result<Value, RuntimeError> {
    if method == "runtime.status" {
        return Ok(status_from(&AgentControlAuth::from_env()));
    }
    if !METHODS.contains(&method) {
        return Err(RuntimeError::Unsupported(format!(
            "unknown runtime method {method}"
        )));
    }
    if method == "runtime.attach" {
        return Err(RuntimeError::Unsupported(
            "framed attach streams are not exposed over the line RPC; use the framed client".into(),
        ));
    }
    if method == "runtime.detach" {
        // View close is local and must succeed even when herdr-core is down.
        // Nothing is spawned, connected or called; only the payload shape is
        // validated.
        let _ = binding(params)?;
        return Ok(json!({"runtime_calls": 0, "stopped": false}));
    }

    // Parse, validate and authorize before any `hc connect` spawn. An
    // unauthenticated caller must not allocate a child process or worker.
    let call = parse_call(method, params)?;
    let auth = AgentControlAuth::from_env();
    RuntimeService::preflight(&auth, &call)?;
    let connection = HcClient::default()
        .framed()
        .map_err(RuntimeError::from_hc)?;
    let mut service = RuntimeService::new(
        auth,
        Box::new(connection),
        AuditLog::file(AgentControlAuth::audit_path()),
        global_cache(),
    );
    let execution = service.execute(call)?;
    execution_json(execution)
}

fn status_from(auth: &AgentControlAuth) -> Value {
    json!({
        "enabled": auth.is_enabled(),
        "capability": "separate-agent-control",
        "problem": auth.problem(),
        "mutations": ["runtime.send", "runtime.interrupt", "runtime.stop", "runtime.archive-finished"],
        "view_close_stops_agent": false
    })
}

/// Executes one parsed request against an injected service. Unit tests use a
/// mock backend; the live dispatcher above builds a framed one.
#[cfg(test)]
pub fn dispatch_with(
    service: &mut RuntimeService,
    method: &str,
    params: &Value,
) -> Result<Value, RuntimeError> {
    match method {
        "runtime.status" => Ok(status_from(service.auth())),
        "runtime.detach" => {
            let binding = binding(params)?;
            let outcome = service.view_closed(&binding)?;
            Ok(json!({
                "runtime_calls": outcome.runtime_calls,
                "stopped": outcome.stopped
            }))
        }
        "runtime.attach" => Err(RuntimeError::Unsupported(
            "framed attach streams are not exposed over the line RPC".into(),
        )),
        _ => {
            let call = parse_call(method, params)?;
            execution_json(service.execute(call)?)
        }
    }
}

fn parse_call(method: &str, params: &Value) -> Result<RuntimeCall, RuntimeError> {
    let action = match method {
        "runtime.observe" => RuntimeAction::Observe,
        "runtime.send" => RuntimeAction::SendInput {
            text: required_str(params, "text")?.to_owned(),
        },
        "runtime.interrupt" => RuntimeAction::Interrupt,
        "runtime.stop" => RuntimeAction::Stop,
        "runtime.archive-finished" => RuntimeAction::RemoveFinished,
        other => {
            return Err(RuntimeError::Unsupported(format!(
                "unknown runtime method {other}"
            )))
        }
    };
    let require_request_id = action.is_mutating();
    runtime_call(params, action, require_request_id)
}

fn runtime_call(
    params: &Value,
    action: RuntimeAction,
    require_request_id: bool,
) -> Result<RuntimeCall, RuntimeError> {
    let request_id = match optional_str(params, "request_id") {
        Some(id) => id,
        None if require_request_id => {
            return Err(RuntimeError::InvalidParams(
                "request_id is required for a runtime mutation".into(),
            ))
        }
        None => "read-only".to_owned(),
    };
    Ok(RuntimeCall {
        request_id,
        binding: binding(params)?,
        intent: intent(params)?,
        // Deliberately only `control`. `auth` is the workspace token and is
        // never accepted for agent control.
        control: optional_str(params, "control"),
        action,
    })
}

fn binding(params: &Value) -> Result<RunBinding, RuntimeError> {
    let core_id = required_str(params, "core_id")?;
    let terminal = required_str(params, "terminal")?;
    RunBinding::new(core_id, terminal)
}

fn intent(params: &Value) -> Result<Intent, RuntimeError> {
    match params.get("intent").and_then(Value::as_str) {
        Some("user") => Ok(Intent::User),
        Some("automation") | None => Ok(Intent::Automation),
        Some(other) => Err(RuntimeError::InvalidParams(format!(
            "intent must be \"user\" or \"automation\", got {other:?}"
        ))),
    }
}

fn required_str<'a>(params: &'a Value, key: &str) -> Result<&'a str, RuntimeError> {
    params
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| RuntimeError::InvalidParams(format!("missing or invalid {key}")))
}

fn optional_str(params: &Value, key: &str) -> Option<String> {
    params
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
}

fn execution_json(execution: super::runtime::Execution) -> Result<Value, RuntimeError> {
    let replayed = execution.replayed;
    let (value, include_replayed) = match execution.outcome {
        RuntimeOutcome::Observed(terminal) => (serde_json::to_value(terminal), false),
        RuntimeOutcome::InputSent { result, .. } => (serde_json::to_value(result), true),
        RuntimeOutcome::Interrupted(result) => (serde_json::to_value(result), true),
        RuntimeOutcome::Stopped(terminal) => (serde_json::to_value(terminal), true),
        RuntimeOutcome::Removed(result) => (serde_json::to_value(result), true),
        RuntimeOutcome::Attached(_) => {
            return Err(RuntimeError::Unsupported(
                "attach results cannot be encoded on the line RPC".into(),
            ))
        }
    };
    let mut value = value.map_err(|error| RuntimeError::Unavailable(error.to_string()))?;
    if include_replayed {
        if let Some(object) = value.as_object_mut() {
            object.insert("replayed".into(), Value::Bool(replayed));
        }
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use ukk_hc_client::{
        AttachRequest, Attachment, HcError, InterruptRequest, InterruptResult, RemoveResult,
        SendOutcome, SendRequest, Terminal, TerminalAddress,
    };

    struct PanicBackend {
        called: Arc<AtomicBool>,
    }

    impl PanicBackend {
        fn new() -> Self {
            Self {
                called: Arc::new(AtomicBool::new(false)),
            }
        }

        fn touch(&self) {
            self.called.store(true, Ordering::SeqCst);
            panic!("backend must not be called");
        }
    }

    impl super::super::runtime::RuntimeBackend for PanicBackend {
        fn core_id(&self) -> &str {
            "core-1"
        }
        fn get(&mut self, _terminal: &TerminalAddress) -> Result<Terminal, HcError> {
            self.touch();
            unreachable!()
        }
        fn attach(&mut self, _request: &AttachRequest) -> Result<Attachment, HcError> {
            self.touch();
            unreachable!()
        }
        fn send(
            &mut self,
            _request: &SendRequest,
            _key: Option<&str>,
        ) -> Result<SendOutcome, HcError> {
            self.touch();
            unreachable!()
        }
        fn interrupt(&mut self, _request: &InterruptRequest) -> Result<InterruptResult, HcError> {
            self.touch();
            unreachable!()
        }
        fn kill(&mut self, _terminal: &TerminalAddress) -> Result<Terminal, HcError> {
            self.touch();
            unreachable!()
        }
        fn remove(&mut self, _terminal: &TerminalAddress) -> Result<RemoveResult, HcError> {
            self.touch();
            unreachable!()
        }
    }

    fn disabled_service() -> RuntimeService {
        RuntimeService::with_backend(
            AgentControlAuth::disabled_test(),
            Box::new(PanicBackend::new()),
        )
    }

    #[test]
    fn status_never_connects_to_herdr_core() {
        let value = status_from(&AgentControlAuth::disabled_test());
        assert_eq!(value["enabled"], false);
        assert_eq!(value["capability"], "separate-agent-control");
        assert_eq!(value["view_close_stops_agent"], false);
    }

    #[test]
    fn detach_reports_no_runtime_call_and_never_reaches_the_backend() {
        let backend = PanicBackend::new();
        let called = Arc::clone(&backend.called);
        let mut service =
            RuntimeService::with_backend(AgentControlAuth::disabled_test(), Box::new(backend));
        let value = dispatch_with(
            &mut service,
            "runtime.detach",
            &json!({"core_id":"core-1","terminal":"t7"}),
        )
        .unwrap();
        assert_eq!(value["runtime_calls"], 0);
        assert_eq!(value["stopped"], false);
        assert!(!called.load(Ordering::SeqCst));

        // A view close for the wrong core is a typed mismatch but still makes
        // no runtime call.
        let error = dispatch_with(
            &mut service,
            "runtime.detach",
            &json!({"core_id":"core-2","terminal":"t7"}),
        )
        .unwrap_err();
        assert!(matches!(error, RuntimeError::CoreMismatch { .. }));
        assert!(!called.load(Ordering::SeqCst));
    }

    #[test]
    fn workspace_auth_field_does_not_authorize_control() {
        let backend = PanicBackend::new();
        let called = Arc::clone(&backend.called);
        let mut service = RuntimeService::with_backend(
            AgentControlAuth::test("separate-control-token"),
            Box::new(backend),
        );
        let error = dispatch_with(
            &mut service,
            "runtime.stop",
            &json!({
                "core_id":"core-1","terminal":"t7","request_id":"r1",
                "intent":"user","auth":"workspace-token"
            }),
        )
        .unwrap_err();
        assert_eq!(error, RuntimeError::Unauthorized);
        assert!(!called.load(Ordering::SeqCst));
    }

    #[test]
    fn mutating_rpc_requires_request_id_and_user_intent() {
        let mut service = disabled_service();
        let error = dispatch_with(
            &mut service,
            "runtime.send",
            &json!({"core_id":"core-1","terminal":"t7","text":"hi","intent":"user"}),
        )
        .unwrap_err();
        assert!(matches!(error, RuntimeError::InvalidParams(_)));

        let error = dispatch_with(
            &mut service,
            "runtime.send",
            &json!({"core_id":"core-1","terminal":"t7","text":"hi","request_id":"r1"}),
        )
        .unwrap_err();
        assert_eq!(error, RuntimeError::ControlDisabled);
    }

    #[test]
    fn invalid_intent_word_is_typed() {
        let mut service = disabled_service();
        let error = dispatch_with(
            &mut service,
            "runtime.observe",
            &json!({"core_id":"core-1","terminal":"t7","intent":"robot"}),
        )
        .unwrap_err();
        assert!(matches!(error, RuntimeError::InvalidParams(_)));
    }

    #[test]
    fn attach_over_line_rpc_is_explicitly_unsupported() {
        let mut service = disabled_service();
        let error = dispatch_with(
            &mut service,
            "runtime.attach",
            &json!({"core_id":"core-1","terminal":"t7"}),
        )
        .unwrap_err();
        assert!(matches!(error, RuntimeError::Unsupported(_)));
    }

    #[test]
    fn send_requires_text_and_a_nonempty_request_id() {
        let mut service = disabled_service();
        let error = dispatch_with(
            &mut service,
            "runtime.send",
            &json!({"core_id":"core-1","terminal":"t7","request_id":"r1","intent":"user"}),
        )
        .unwrap_err();
        assert!(matches!(error, RuntimeError::InvalidParams(_)));

        let error = dispatch_with(
            &mut service,
            "runtime.send",
            &json!({"core_id":"core-1","terminal":"t7","request_id":"  ","text":"hi","intent":"user"}),
        )
        .unwrap_err();
        assert!(matches!(error, RuntimeError::InvalidParams(_)));
    }
}
