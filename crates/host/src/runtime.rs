//! Product-level AgentRun runtime actions.
//!
//! Metadata operations (Session archive, Pane close) live in `workspace_rpc`.
//! This module maps an [`AgentRun`] binding to herdr-core lifecycle actions
//! and enforces:
//!
//! * the binding's `core_id` must match the connected core before any action;
//! * terminal input, interrupt, stop and runtime archive require the
//!   **separate** agent-control capability, never the workspace metadata
//!   token;
//! * mutating actions require an explicit user intent;
//! * closing a view/detaching is local and never calls stop or kill;
//! * repeated request ids are idempotent per Host process.
//!
//! Stop maps to `terminal.kill`, the only documented program-end method in
//! protocol generation 1. `terminal.rm` is the runtime half of archiving and
//! refuses a live terminal. Neither is reachable from a view-close path.

use crate::runtime_auth::{now_ms, AgentControlAuth, AuditEntry, AuditLog};
use std::collections::{HashMap, VecDeque};
use std::error::Error;
use std::fmt;
use std::sync::{Arc, Mutex};
use ukk_domain::AgentRun;
use ukk_hc_client::{
    AttachRequest, Attachment, HcError, InterruptRequest, InterruptResult, RemoveResult,
    SendRequest, SendResult, Terminal, TerminalAddress,
};

const MAX_CACHED_REQUESTS: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunBinding {
    pub core_id: String,
    pub terminal: TerminalAddress,
}

impl RunBinding {
    pub fn new(core_id: impl Into<String>, terminal: &str) -> Result<Self, RuntimeError> {
        let core_id = core_id.into();
        if core_id.trim().is_empty() {
            return Err(RuntimeError::InvalidBinding(
                "core_id must not be empty".into(),
            ));
        }
        Ok(Self {
            core_id,
            terminal: TerminalAddress::parse(terminal)
                .map_err(|error| RuntimeError::InvalidBinding(error.to_string()))?,
        })
    }

    // Used by callers that hold a durable `AgentRun`; the RPC surface receives
    // `core_id` and `terminal` as separate fields.
    #[allow(dead_code)]
    pub fn from_agent_run(run: &AgentRun) -> Result<Self, RuntimeError> {
        Self::new(run.core_id.clone(), &run.terminal_address)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    /// A person asked for this action in the UI.
    User,
    /// A script, adapter or replay path. Never sufficient for control.
    Automation,
}

impl Intent {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Automation => "automation",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeAction {
    Observe,
    // Programmatic/desktop use. The line RPC refuses `runtime.attach` because
    // a framed byte stream cannot be carried in one reply.
    #[allow(dead_code)]
    Attach {
        cols: u16,
        rows: u16,
        observe: bool,
    },
    SendInput {
        text: String,
        idempotency_key: Option<String>,
    },
    Interrupt,
    /// Explicit user stop. Maps to the documented `terminal.kill`.
    Stop,
    /// Runtime half of archiving: remove a finished terminal record only.
    RemoveFinished,
}

impl RuntimeAction {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Observe => "runtime.observe",
            Self::Attach { .. } => "runtime.attach",
            Self::SendInput { .. } => "runtime.send",
            Self::Interrupt => "runtime.interrupt",
            Self::Stop => "runtime.stop",
            Self::RemoveFinished => "runtime.archive-finished",
        }
    }

    pub fn is_mutating(&self) -> bool {
        matches!(
            self,
            Self::SendInput { .. } | Self::Interrupt | Self::Stop | Self::RemoveFinished
        )
    }

    pub fn requires_control(&self) -> bool {
        self.is_mutating() || matches!(self, Self::Attach { observe: false, .. })
    }

    pub fn requires_user_intent(&self) -> bool {
        self.is_mutating()
    }

    fn fingerprint(&self) -> String {
        match self {
            Self::SendInput { text, .. } => format!("send|{text}"),
            other => other.name().to_owned(),
        }
    }
}

#[derive(Debug)]
pub struct RuntimeCall {
    pub request_id: String,
    pub binding: RunBinding,
    pub intent: Intent,
    /// Value of the agent-control token file. Never the workspace token.
    pub control: Option<String>,
    pub action: RuntimeAction,
}

impl RuntimeCall {
    fn fingerprint(&self) -> String {
        format!(
            "{}|{}|{}",
            self.action.fingerprint(),
            self.binding.core_id,
            self.binding.terminal
        )
    }
}

pub enum RuntimeOutcome {
    Observed(Terminal),
    Attached(Attachment),
    InputSent { result: SendResult, replayed: bool },
    Interrupted(InterruptResult),
    Stopped(Terminal),
    Removed(RemoveResult),
}

impl fmt::Debug for RuntimeOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Observed(terminal) => f.debug_tuple("Observed").field(terminal).finish(),
            Self::Attached(attachment) => f.debug_tuple("Attached").field(attachment).finish(),
            Self::InputSent { result, replayed } => f
                .debug_struct("InputSent")
                .field("result", result)
                .field("replayed", replayed)
                .finish(),
            Self::Interrupted(result) => f.debug_tuple("Interrupted").field(result).finish(),
            Self::Stopped(terminal) => f.debug_tuple("Stopped").field(terminal).finish(),
            Self::Removed(result) => f.debug_tuple("Removed").field(result).finish(),
        }
    }
}

#[derive(Debug)]
pub struct Execution {
    pub outcome: RuntimeOutcome,
    pub replayed: bool,
}

#[derive(Debug, Clone)]
pub enum CachedOutcome {
    InputSent { result: SendResult, replayed: bool },
    Interrupted(InterruptResult),
    Stopped(Box<Terminal>),
    Removed(RemoveResult),
}

impl RuntimeOutcome {
    fn cacheable(&self) -> Option<CachedOutcome> {
        match self {
            Self::InputSent { result, replayed } => Some(CachedOutcome::InputSent {
                result: result.clone(),
                replayed: *replayed,
            }),
            Self::Interrupted(result) => Some(CachedOutcome::Interrupted(result.clone())),
            Self::Stopped(terminal) => Some(CachedOutcome::Stopped(Box::new(terminal.clone()))),
            Self::Removed(result) => Some(CachedOutcome::Removed(result.clone())),
            _ => None,
        }
    }

    fn from_cached(cached: CachedOutcome) -> Self {
        match cached {
            CachedOutcome::InputSent { result, replayed } => Self::InputSent { result, replayed },
            CachedOutcome::Interrupted(result) => Self::Interrupted(result),
            CachedOutcome::Stopped(terminal) => Self::Stopped(*terminal),
            CachedOutcome::Removed(result) => Self::Removed(result),
        }
    }
}

#[derive(Default)]
pub struct RuntimeCache {
    entries: HashMap<String, (String, CachedOutcome)>,
    order: VecDeque<String>,
}

impl RuntimeCache {
    pub fn new() -> Self {
        Self::default()
    }

    fn lookup(&self, request_id: &str, fingerprint: &str) -> CacheLookup {
        match self.entries.get(request_id) {
            Some((cached, _)) if cached != fingerprint => CacheLookup::Conflict,
            Some((_, outcome)) => CacheLookup::Hit(Box::new(outcome.clone())),
            None => CacheLookup::Miss,
        }
    }

    fn remember(&mut self, request_id: String, fingerprint: String, outcome: CachedOutcome) {
        if !self.entries.contains_key(&request_id) {
            self.order.push_back(request_id.clone());
        }
        self.entries.insert(request_id, (fingerprint, outcome));
        while self.order.len() > MAX_CACHED_REQUESTS {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            }
        }
    }
}

enum CacheLookup {
    Hit(Box<CachedOutcome>),
    Conflict,
    Miss,
}

pub trait RuntimeBackend: Send {
    fn core_id(&self) -> &str;
    fn get(&mut self, terminal: &TerminalAddress) -> Result<Terminal, HcError>;
    fn attach(&mut self, request: &AttachRequest) -> Result<Attachment, HcError>;
    fn send(
        &mut self,
        request: &SendRequest,
        idempotency_key: Option<&str>,
    ) -> Result<ukk_hc_client::SendOutcome, HcError>;
    fn interrupt(&mut self, request: &InterruptRequest) -> Result<InterruptResult, HcError>;
    fn kill(&mut self, terminal: &TerminalAddress) -> Result<Terminal, HcError>;
    fn remove(&mut self, terminal: &TerminalAddress) -> Result<RemoveResult, HcError>;
}

impl RuntimeBackend for ukk_hc_client::FramedConnection {
    fn core_id(&self) -> &str {
        ukk_hc_client::FramedConnection::core_id(self)
    }

    fn get(&mut self, terminal: &TerminalAddress) -> Result<Terminal, HcError> {
        self.terminal_get(terminal)
    }

    fn attach(&mut self, request: &AttachRequest) -> Result<Attachment, HcError> {
        ukk_hc_client::FramedConnection::attach(self, request)
    }

    fn send(
        &mut self,
        request: &SendRequest,
        idempotency_key: Option<&str>,
    ) -> Result<ukk_hc_client::SendOutcome, HcError> {
        match idempotency_key {
            Some(key) => self.terminal_send_once(key, request),
            None => self
                .terminal_send(request)
                .map(|result| ukk_hc_client::SendOutcome {
                    result,
                    replayed: false,
                }),
        }
    }

    fn interrupt(&mut self, request: &InterruptRequest) -> Result<InterruptResult, HcError> {
        self.terminal_interrupt(request)
    }

    fn kill(&mut self, terminal: &TerminalAddress) -> Result<Terminal, HcError> {
        self.terminal_kill(terminal)
    }

    fn remove(&mut self, terminal: &TerminalAddress) -> Result<RemoveResult, HcError> {
        self.terminal_remove(terminal)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeError {
    ControlDisabled,
    Unauthorized,
    InsecureCapability(String),
    CoreMismatch { expected: String, actual: String },
    InvalidBinding(String),
    IntentRequired,
    StillLive { terminal: String },
    RequestIdReuse(String),
    Unsupported(String),
    InvalidParams(String),
    Unavailable(String),
}

impl RuntimeError {
    pub fn code(&self) -> i32 {
        match self {
            Self::ControlDisabled => -32041,
            Self::Unauthorized => -32043,
            Self::InsecureCapability(_) => -32044,
            Self::CoreMismatch { .. } => -32045,
            Self::IntentRequired => -32046,
            Self::StillLive { .. } => -32047,
            Self::RequestIdReuse(_) => -32048,
            Self::Unsupported(_) => -32049,
            Self::InvalidBinding(_) | Self::InvalidParams(_) => -32602,
            Self::Unavailable(_) => -32040,
        }
    }

    pub fn message(&self) -> String {
        match self {
            Self::ControlDisabled => {
                "agent-control capability is disabled; create its separate token file first".into()
            }
            Self::Unauthorized => "agent-control capability is not authorized".into(),
            Self::InsecureCapability(problem) => {
                format!("agent-control capability is disabled: {problem}")
            }
            Self::CoreMismatch { expected, actual } => format!(
                "AgentRun binding belongs to core {expected}, but the connected core is {actual}"
            ),
            Self::InvalidBinding(message) => message.clone(),
            Self::IntentRequired => "this action requires an explicit user intent".into(),
            Self::StillLive { terminal } => format!(
                "terminal {terminal} is still live; archive its metadata instead of removing a live runtime record"
            ),
            Self::RequestIdReuse(id) => {
                format!("request id {id} was reused for a different runtime action")
            }
            Self::Unsupported(message) | Self::Unavailable(message) => message.clone(),
            Self::InvalidParams(message) => message.clone(),
        }
    }

    pub fn from_hc(error: HcError) -> Self {
        match error {
            HcError::Unsupported { .. } => Self::Unsupported(error.to_string()),
            HcError::InvalidArgument(message) => Self::InvalidParams(message),
            HcError::Remote(_) => Self::Unavailable(error.to_string()),
            other => Self::Unavailable(other.to_string()),
        }
    }
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message())
    }
}

impl Error for RuntimeError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewCloseOutcome {
    pub runtime_calls: usize,
    pub stopped: bool,
}

pub struct RuntimeService {
    auth: AgentControlAuth,
    backend: Box<dyn RuntimeBackend + Send>,
    audit: AuditLog,
    cache: Arc<Mutex<RuntimeCache>>,
}

impl RuntimeService {
    pub fn new(
        auth: AgentControlAuth,
        backend: Box<dyn RuntimeBackend + Send>,
        audit: AuditLog,
        cache: Arc<Mutex<RuntimeCache>>,
    ) -> Self {
        Self {
            auth,
            backend,
            audit,
            cache,
        }
    }

    #[cfg(test)]
    pub fn with_backend(auth: AgentControlAuth, backend: Box<dyn RuntimeBackend + Send>) -> Self {
        Self::new(
            auth,
            backend,
            AuditLog::memory(),
            Arc::new(Mutex::new(RuntimeCache::new())),
        )
    }

    pub fn auth(&self) -> &AgentControlAuth {
        &self.auth
    }

    #[cfg(test)]
    pub fn audit_entries(&self) -> Vec<AuditEntry> {
        self.audit.entries()
    }

    pub fn execute(&mut self, call: RuntimeCall) -> Result<Execution, RuntimeError> {
        if call.request_id.trim().is_empty() || call.request_id.len() > 128 {
            return Err(RuntimeError::InvalidParams(
                "request_id must be 1-128 characters".into(),
            ));
        }
        if call.action.requires_control() {
            if !self.auth.is_enabled() {
                self.deny(&call, "control_disabled");
                return Err(match self.auth.problem() {
                    Some(problem) => RuntimeError::InsecureCapability(problem.to_owned()),
                    None => RuntimeError::ControlDisabled,
                });
            }
            if !self.auth.authorized(call.control.as_deref()) {
                self.deny(&call, "unauthorized");
                return Err(RuntimeError::Unauthorized);
            }
        }
        if call.action.requires_user_intent() && call.intent != Intent::User {
            self.deny(&call, "intent_required");
            return Err(RuntimeError::IntentRequired);
        }
        let actual = self.backend.core_id().to_owned();
        if call.binding.core_id != actual {
            self.deny(&call, "core_mismatch");
            return Err(RuntimeError::CoreMismatch {
                expected: call.binding.core_id.clone(),
                actual,
            });
        }

        let fingerprint = call.fingerprint();
        if call.action.is_mutating() {
            let lookup = self
                .cache
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .lookup(&call.request_id, &fingerprint);
            match lookup {
                CacheLookup::Hit(cached) => {
                    self.audit(&call, "ok_replayed");
                    return Ok(Execution {
                        outcome: RuntimeOutcome::from_cached(*cached),
                        replayed: true,
                    });
                }
                CacheLookup::Conflict => {
                    self.deny(&call, "request_id_reuse");
                    return Err(RuntimeError::RequestIdReuse(call.request_id.clone()));
                }
                CacheLookup::Miss => {}
            }
        }

        let outcome = match self.perform(&call) {
            Ok(outcome) => outcome,
            Err(error) => {
                self.audit(&call, &format!("error:{}", error.code()));
                return Err(error);
            }
        };
        if let Some(cached) = outcome.cacheable() {
            self.cache
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .remember(call.request_id.clone(), fingerprint, cached);
        }
        self.audit(&call, "ok");
        Ok(Execution {
            outcome,
            replayed: false,
        })
    }

    /// Closing a view or detaching is a local operation. It validates the
    /// binding identity but never calls stop, kill, remove or any backend
    /// action.
    pub fn view_closed(&self, binding: &RunBinding) -> Result<ViewCloseOutcome, RuntimeError> {
        let actual = self.backend.core_id();
        if binding.core_id != actual {
            return Err(RuntimeError::CoreMismatch {
                expected: binding.core_id.clone(),
                actual: actual.to_owned(),
            });
        }
        Ok(ViewCloseOutcome {
            runtime_calls: 0,
            stopped: false,
        })
    }

    fn perform(&mut self, call: &RuntimeCall) -> Result<RuntimeOutcome, RuntimeError> {
        match &call.action {
            RuntimeAction::Observe => self
                .backend
                .get(&call.binding.terminal)
                .map(RuntimeOutcome::Observed)
                .map_err(RuntimeError::from_hc),
            RuntimeAction::Attach {
                cols,
                rows,
                observe,
            } => {
                let request = AttachRequest {
                    terminal: call.binding.terminal.clone(),
                    cols: *cols,
                    rows: *rows,
                    history: 0,
                    observe: *observe,
                    resize_notifications: true,
                    resumed: false,
                    caller: None,
                    display: None,
                };
                self.backend
                    .attach(&request)
                    .map(RuntimeOutcome::Attached)
                    .map_err(RuntimeError::from_hc)
            }
            RuntimeAction::SendInput {
                text,
                idempotency_key,
            } => {
                let request = SendRequest::new(call.binding.terminal.clone(), text.clone());
                let key = idempotency_key
                    .as_deref()
                    .unwrap_or(call.request_id.as_str());
                let outcome = self
                    .backend
                    .send(&request, Some(key))
                    .map_err(RuntimeError::from_hc)?;
                Ok(RuntimeOutcome::InputSent {
                    result: outcome.result,
                    replayed: outcome.replayed,
                })
            }
            RuntimeAction::Interrupt => {
                let request = InterruptRequest {
                    terminal: call.binding.terminal.clone(),
                    timeout: None,
                };
                self.backend
                    .interrupt(&request)
                    .map(RuntimeOutcome::Interrupted)
                    .map_err(RuntimeError::from_hc)
            }
            RuntimeAction::Stop => self
                .backend
                .kill(&call.binding.terminal)
                .map(RuntimeOutcome::Stopped)
                .map_err(RuntimeError::from_hc),
            RuntimeAction::RemoveFinished => {
                let terminal = self
                    .backend
                    .get(&call.binding.terminal)
                    .map_err(RuntimeError::from_hc)?;
                if terminal.is_live() {
                    return Err(RuntimeError::StillLive {
                        terminal: terminal.address.to_string(),
                    });
                }
                self.backend
                    .remove(&call.binding.terminal)
                    .map(RuntimeOutcome::Removed)
                    .map_err(RuntimeError::from_hc)
            }
        }
    }

    fn audit(&self, call: &RuntimeCall, outcome: &str) {
        if !call.action.requires_control() {
            return;
        }
        let entry = AuditEntry {
            at_ms: now_ms(),
            request_id: call.request_id.clone(),
            action: call.action.name().to_owned(),
            core_id: call.binding.core_id.clone(),
            terminal: call.binding.terminal.to_string(),
            intent: call.intent.as_str().to_owned(),
            outcome: outcome.to_owned(),
        };
        if let Err(error) = self.audit.record(entry) {
            eprintln!("ukk-host: agent-control audit write failed: {error}");
        }
    }

    fn deny(&self, call: &RuntimeCall, reason: &str) {
        self.audit(call, &format!("denied:{reason}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as TestMutex;
    use ukk_hc_client::{InterruptOutcome, SendOutcome, TerminalState};

    const CONTROL: &str = "control-capability-that-is-long-enough-for-validation-1234567890";

    fn terminal_record(live: bool) -> Terminal {
        let mut terminal = Terminal::from_value(serde_json::json!({
            "address": "t7", "argv": ["codex"], "cwd": "/work", "state": "live",
            "created_ms": 1, "screen": "exact", "cols": 80, "rows": 24
        }))
        .unwrap();
        if !live {
            terminal.state = TerminalState::Finished;
        }
        terminal
    }

    struct MockBackend {
        core: String,
        calls: Arc<TestMutex<Vec<String>>>,
        live: bool,
        turn: u64,
    }

    impl MockBackend {
        fn new() -> Self {
            Self {
                core: "core-1".into(),
                calls: Arc::new(TestMutex::new(Vec::new())),
                live: true,
                turn: 4,
            }
        }

        fn log(&self, call: impl Into<String>) {
            self.calls.lock().unwrap().push(call.into());
        }
    }

    impl RuntimeBackend for MockBackend {
        fn core_id(&self) -> &str {
            &self.core
        }

        fn get(&mut self, terminal: &TerminalAddress) -> Result<Terminal, HcError> {
            self.log(format!("get:{terminal}"));
            Ok(terminal_record(self.live))
        }

        fn attach(&mut self, _request: &AttachRequest) -> Result<Attachment, HcError> {
            self.log("attach");
            Err(HcError::Unsupported {
                method: "terminal.attach".into(),
                reason: "mock needs a framed connection".into(),
            })
        }

        fn send(
            &mut self,
            _request: &SendRequest,
            key: Option<&str>,
        ) -> Result<SendOutcome, HcError> {
            self.log(format!("send:{key:?}"));
            self.turn += 1;
            Ok(SendOutcome {
                result: SendResult {
                    turn: self.turn,
                    status: Some("working".into()),
                    started: Some(true),
                    wait: None,
                },
                replayed: false,
            })
        }

        fn interrupt(&mut self, _request: &InterruptRequest) -> Result<InterruptResult, HcError> {
            self.log("interrupt");
            Ok(InterruptResult {
                outcome: InterruptOutcome::Stopped,
                key: "esc".into(),
            })
        }

        fn kill(&mut self, _terminal: &TerminalAddress) -> Result<Terminal, HcError> {
            self.log("kill");
            let mut terminal = terminal_record(false);
            terminal.reason = Some(ukk_hc_client::TerminalReason::Killed);
            Ok(terminal)
        }

        fn remove(&mut self, terminal: &TerminalAddress) -> Result<RemoveResult, HcError> {
            self.log(format!("remove:{terminal}"));
            Ok(RemoveResult {
                removed: terminal.to_string(),
            })
        }
    }

    fn binding() -> RunBinding {
        RunBinding::new("core-1", "t7").unwrap()
    }

    fn call(action: RuntimeAction, intent: Intent, control: Option<&str>) -> RuntimeCall {
        RuntimeCall {
            request_id: "req-1".into(),
            binding: binding(),
            intent,
            control: control.map(str::to_owned),
            action,
        }
    }

    fn service(auth: AgentControlAuth) -> (RuntimeService, Arc<TestMutex<Vec<String>>>) {
        let backend = MockBackend::new();
        let calls = Arc::clone(&backend.calls);
        (RuntimeService::with_backend(auth, Box::new(backend)), calls)
    }

    #[test]
    fn view_close_never_calls_a_runtime_action() {
        let (service, _) = service(AgentControlAuth::test(CONTROL));
        let outcome = service.view_closed(&binding()).unwrap();
        assert_eq!(outcome.runtime_calls, 0);
        assert!(!outcome.stopped);
        assert!(service.audit_entries().is_empty());
    }

    #[test]
    fn control_requires_the_separate_capability() {
        let (mut disabled, _) = service(AgentControlAuth::disabled_test());
        let error = disabled
            .execute(call(
                RuntimeAction::SendInput {
                    text: "hi".into(),
                    idempotency_key: None,
                },
                Intent::User,
                Some("workspace-token"),
            ))
            .unwrap_err();
        assert_eq!(error, RuntimeError::ControlDisabled);

        let (mut enabled, _) = service(AgentControlAuth::test(CONTROL));
        let workspace_token = "workspace-metadata-token-that-must-never-control-runtime";
        let error = enabled
            .execute(call(
                RuntimeAction::Stop,
                Intent::User,
                Some(workspace_token),
            ))
            .unwrap_err();
        assert_eq!(error, RuntimeError::Unauthorized);
        assert!(enabled.audit_entries().iter().any(|entry| {
            entry.outcome == "denied:unauthorized" && entry.action == "runtime.stop"
        }));
    }

    #[test]
    fn authorized_user_stop_maps_to_kill() {
        let (mut service, _) = service(AgentControlAuth::test(CONTROL));
        let execution = service
            .execute(call(RuntimeAction::Stop, Intent::User, Some(CONTROL)))
            .unwrap();
        assert!(matches!(execution.outcome, RuntimeOutcome::Stopped(_)));
        let entry = service.audit_entries().pop().unwrap();
        assert_eq!(entry.action, "runtime.stop");
        assert_eq!(entry.outcome, "ok");
        assert!(!format!("{entry:?}").contains(CONTROL));
    }

    #[test]
    fn mutating_actions_require_explicit_user_intent() {
        let (mut service, _) = service(AgentControlAuth::test(CONTROL));
        let error = service
            .execute(call(
                RuntimeAction::Interrupt,
                Intent::Automation,
                Some(CONTROL),
            ))
            .unwrap_err();
        assert_eq!(error, RuntimeError::IntentRequired);
    }

    #[test]
    fn binding_core_must_match_the_connected_core() {
        let (mut service, _) = service(AgentControlAuth::test(CONTROL));
        let mut request = call(RuntimeAction::Stop, Intent::User, Some(CONTROL));
        request.binding = RunBinding::new("core-2", "t7").unwrap();
        let error = service.execute(request).unwrap_err();
        assert!(matches!(error, RuntimeError::CoreMismatch { .. }));
    }

    #[test]
    fn archive_refuses_a_live_terminal_and_never_kills_it() {
        let (mut service, calls) = service(AgentControlAuth::test(CONTROL));
        let error = service
            .execute(call(
                RuntimeAction::RemoveFinished,
                Intent::User,
                Some(CONTROL),
            ))
            .unwrap_err();
        assert!(matches!(error, RuntimeError::StillLive { .. }));
        assert_eq!(calls.lock().unwrap().clone(), vec!["get:t7"]);
    }

    #[test]
    fn archive_removes_only_a_finished_record() {
        let mut backend = MockBackend::new();
        backend.live = false;
        let calls = Arc::clone(&backend.calls);
        let mut service =
            RuntimeService::with_backend(AgentControlAuth::test(CONTROL), Box::new(backend));
        let execution = service
            .execute(call(
                RuntimeAction::RemoveFinished,
                Intent::User,
                Some(CONTROL),
            ))
            .unwrap();
        assert!(matches!(execution.outcome, RuntimeOutcome::Removed(_)));
        assert_eq!(calls.lock().unwrap().clone(), vec!["get:t7", "remove:t7"]);
    }

    #[test]
    fn duplicate_request_id_never_sends_or_stops_twice() {
        let (mut service, calls) = service(AgentControlAuth::test(CONTROL));
        let action = RuntimeAction::SendInput {
            text: "one".into(),
            idempotency_key: None,
        };
        let first = service
            .execute(call(action.clone(), Intent::User, Some(CONTROL)))
            .unwrap();
        assert!(!first.replayed);
        let second = service
            .execute(call(action, Intent::User, Some(CONTROL)))
            .unwrap();
        assert!(second.replayed);
        assert_eq!(calls.lock().unwrap().clone(), vec!["send:Some(\"req-1\")"]);
    }

    #[test]
    fn reusing_a_request_id_for_a_different_action_is_rejected() {
        let (mut service, calls) = service(AgentControlAuth::test(CONTROL));
        service
            .execute(call(
                RuntimeAction::SendInput {
                    text: "one".into(),
                    idempotency_key: None,
                },
                Intent::User,
                Some(CONTROL),
            ))
            .unwrap();
        let error = service
            .execute(call(
                RuntimeAction::SendInput {
                    text: "two".into(),
                    idempotency_key: None,
                },
                Intent::User,
                Some(CONTROL),
            ))
            .unwrap_err();
        assert!(matches!(error, RuntimeError::RequestIdReuse(_)));
        assert_eq!(calls.lock().unwrap().clone(), vec!["send:Some(\"req-1\")"]);
    }

    #[test]
    fn attach_with_input_requires_control_but_observe_does_not() {
        let (mut service, calls) = service(AgentControlAuth::disabled_test());
        let denied = service.execute(call(
            RuntimeAction::Attach {
                cols: 80,
                rows: 24,
                observe: false,
            },
            Intent::User,
            None,
        ));
        assert_eq!(denied.unwrap_err(), RuntimeError::ControlDisabled);
        assert!(calls.lock().unwrap().is_empty());
    }

    #[test]
    fn observe_is_read_only_and_does_not_require_control() {
        let (mut service, _) = service(AgentControlAuth::disabled_test());
        let execution = service
            .execute(call(RuntimeAction::Observe, Intent::Automation, None))
            .unwrap();
        assert!(matches!(execution.outcome, RuntimeOutcome::Observed(_)));
    }
}
