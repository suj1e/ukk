//! Host-owned omp RPC sessions (`omp.*`).
//!
//! The Host is the process owner: one child per running Session, started only
//! by an explicit `omp.start`. Read-only `omp.snapshot` / `omp.wait` never
//! mutate state. A Desktop view or window closing has no Host-side effect:
//! nothing here is tied to a client connection. Only an explicit `omp.abort`
//! (which merely cancels the turn) or Host shutdown stops the child.
//!
//! This slice deliberately does not reattach after a Host restart. The omp
//! session id reported by omp could be used for future `--resume`, but that is
//! NOT implemented here.

use super::omp_timeline::Timeline;
use super::runtime_auth::{now_ms, AgentControlAuth, AuditEntry, AuditLog};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::env;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::thread;
use std::time::Duration;
use ukk_omp_client::{
    Event, EventKind, EventReceiver, OmpClient, OmpConfig, OmpError, OmpExit, OmpSnapshot,
    OmpState, OmpTimelineKind, OmpTurn, OmpTurnState, OMP_PROVENANCE, OMP_RPC_PROTOCOL_VERSION,
};

/// Hard cap on concurrently running omp children in one Host.
const MAX_SESSIONS: usize = 4;
/// Prompt message limit accepted from the Desktop.
const MAX_PROMPT_BYTES: usize = 64 * 1024;
/// Long-poll ceiling.
const MAX_WAIT_MS: u64 = 10_000;
/// Upper bound on a Session id echoed by the client.
const MAX_SESSION_ID_BYTES: usize = 256;

#[derive(Debug)]
pub(crate) struct OmpServiceError {
    pub(crate) code: i32,
    pub(crate) message: String,
}

impl OmpServiceError {
    fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    fn invalid(field: &str) -> Self {
        Self::new(-32602, format!("missing or invalid {field}"))
    }

    fn unauthorized(message: &str) -> Self {
        Self::new(-32030, message)
    }
}

pub(crate) struct OmpService {
    sessions: Mutex<HashMap<String, Arc<Session>>>,
    /// Test/embedding override. Production builds use `OmpConfig::default()`
    /// plus the Session cwd, optionally overridden by `UKK_OMP_BINARY` /
    /// `UKK_OMP_ARGS` for integration testing.
    config: Option<OmpConfig>,
    audit: AuditLog,
}

impl OmpService {
    pub(crate) fn from_env() -> Self {
        let config = {
            let mut config = OmpConfig::default();
            let mut customized = false;
            if let Some(binary) = env::var_os("UKK_OMP_BINARY") {
                config.binary = binary.into();
                customized = true;
            }
            if let Ok(args) = env::var("UKK_OMP_ARGS") {
                config.args = args.split_whitespace().map(str::to_owned).collect();
                customized = true;
            }
            customized.then_some(config)
        };
        Self {
            sessions: Mutex::new(HashMap::new()),
            config,
            audit: AuditLog::file(AgentControlAuth::audit_path()),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_config(config: OmpConfig) -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            config: Some(config),
            audit: AuditLog::memory(),
        }
    }

    #[cfg(test)]
    fn audit_entries(&self) -> Vec<AuditEntry> {
        self.audit.entries()
    }

    fn sessions(&self) -> std::sync::MutexGuard<'_, HashMap<String, Arc<Session>>> {
        self.sessions
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    /// Start (or return) the omp child for one Session. Spawning happens
    /// outside the session-map lock so `omp.snapshot` stays responsive.
    pub(crate) fn start(
        &self,
        params: &Value,
        auth: &AgentControlAuth,
    ) -> Result<Value, OmpServiceError> {
        let session_id = required_str(params, "session_id")?;
        if session_id.len() > MAX_SESSION_ID_BYTES {
            return Err(OmpServiceError::invalid("session_id"));
        }
        self.authorize(params, auth, "omp.start", session_id)?;
        let cwd = required_str(params, "cwd")?;
        if !Path::new(cwd).is_dir() {
            self.audit(params, "omp.start", session_id, "error:invalid_cwd");
            return Err(OmpServiceError::invalid("cwd"));
        }
        {
            let sessions = self.sessions();
            if let Some(existing) = sessions.get(session_id) {
                if existing.client.state().is_running() {
                    return existing.snapshot();
                }
            }
            if sessions.len() >= MAX_SESSIONS && !sessions.contains_key(session_id) {
                return Err(OmpServiceError::new(
                    -32040,
                    format!("omp session limit ({MAX_SESSIONS}) reached"),
                ));
            }
        }
        let mut config = self.config.clone().unwrap_or_default();
        config.cwd = Some(cwd.into());
        let client = OmpClient::spawn(config).map_err(|error| {
            self.audit(
                params,
                "omp.start",
                session_id,
                &format!("error:{}", error.kind()),
            );
            match error {
                OmpError::StartupTimeout { .. } => {
                    OmpServiceError::new(-32044, format!("omp did not become ready: {error}"))
                }
                other => OmpServiceError::new(-32042, format!("could not start omp: {other}")),
            }
        })?;
        let events = client.subscribe();
        let session = Arc::new(Session::new(session_id.to_owned(), cwd.to_owned(), client));
        {
            let mut sessions = self.sessions();
            if let Some(existing) = sessions.get(session_id) {
                if existing.client.state().is_running() {
                    // Another connection won the race; discard our child.
                    return existing.snapshot();
                }
            }
            sessions.insert(session_id.to_owned(), Arc::clone(&session));
        }
        spawn_consumer(Arc::downgrade(&session), events);
        self.audit(params, "omp.start", session_id, "ok");
        session.snapshot()
    }

    pub(crate) fn prompt(
        &self,
        params: &Value,
        auth: &AgentControlAuth,
    ) -> Result<Value, OmpServiceError> {
        let session_id = required_str(params, "session_id")?;
        let message = required_str(params, "message")?;
        if message.len() > MAX_PROMPT_BYTES {
            return Err(OmpServiceError::invalid("message"));
        }
        self.authorize(params, auth, "omp.prompt", session_id)?;
        let session = self.session(session_id)?;
        if !session.client.state().is_running() {
            return Err(OmpServiceError::new(-32043, "omp is not running"));
        }
        {
            let turn = session
                .turn
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            if matches!(turn.state, OmpTurnState::Running | OmpTurnState::Aborting) {
                return Err(OmpServiceError::new(
                    -32041,
                    "an omp turn is already running; abort it first",
                ));
            }
        }
        session.begin_user(message);
        session.bump();
        let result = session.client.prompt(message);
        match result {
            Ok(ack) => {
                if ack.agent_invoked == Some(false) {
                    session.complete_local();
                }
                self.audit(params, "omp.prompt", session_id, "ok");
                session.snapshot()
            }
            Err(error) => {
                session.fail(&format!("omp rejected the prompt: {error}"));
                self.audit(
                    params,
                    "omp.prompt",
                    session_id,
                    &format!("error:{}", error.kind()),
                );
                Err(match error {
                    OmpError::RequestTimeout { .. } | OmpError::StartupTimeout { .. } => {
                        OmpServiceError::new(-32044, error.to_string())
                    }
                    other => OmpServiceError::new(-32042, other.to_string()),
                })
            }
        }
    }

    pub(crate) fn abort(
        &self,
        params: &Value,
        auth: &AgentControlAuth,
    ) -> Result<Value, OmpServiceError> {
        let session_id = required_str(params, "session_id")?;
        self.authorize(params, auth, "omp.abort", session_id)?;
        let session = self.session(session_id)?;
        if !session.client.state().is_running() {
            return Err(OmpServiceError::new(-32043, "omp is not running"));
        }
        {
            let turn = session
                .turn
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            if !matches!(turn.state, OmpTurnState::Running | OmpTurnState::Aborting) {
                return Err(OmpServiceError::new(-32041, "no omp turn is running"));
            }
        }
        session.mark_aborting();
        match session.client.abort() {
            Ok(_) => {
                session
                    .timeline
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner())
                    .push(
                        OmpTimelineKind::Status,
                        "Abort requested; waiting for omp's terminal `agent_end`",
                        None,
                        None,
                        false,
                    );
                session.bump();
                self.audit(params, "omp.abort", session_id, "ok");
                session.snapshot()
            }
            Err(error) => {
                session.fail(&format!("omp abort failed: {error}"));
                self.audit(
                    params,
                    "omp.abort",
                    session_id,
                    &format!("error:{}", error.kind()),
                );
                Err(OmpServiceError::new(-32042, error.to_string()))
            }
        }
    }

    /// Read-only snapshot. Never requires a capability and never signals the
    /// child.
    pub(crate) fn snapshot(&self, params: &Value) -> Result<Value, OmpServiceError> {
        let session_id = required_str(params, "session_id")?;
        self.session(session_id)?.snapshot()
    }

    /// Long-poll on the Session generation. Never mutates and never stops the
    /// child when the caller disconnects.
    pub(crate) fn wait(&self, params: &Value) -> Result<Value, OmpServiceError> {
        let session_id = required_str(params, "session_id")?;
        let after = params
            .get("after_generation")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let timeout_ms = params
            .get("timeout_ms")
            .and_then(Value::as_u64)
            .unwrap_or(MAX_WAIT_MS)
            .clamp(1, MAX_WAIT_MS);
        let session = self.session(session_id)?;
        session.wait(after, Duration::from_millis(timeout_ms));
        session.snapshot()
    }

    fn session(&self, session_id: &str) -> Result<Arc<Session>, OmpServiceError> {
        self.sessions()
            .get(session_id)
            .cloned()
            .ok_or_else(|| OmpServiceError::new(-32034, "unknown omp session"))
    }

    fn authorize(
        &self,
        params: &Value,
        auth: &AgentControlAuth,
        action: &str,
        session_id: &str,
    ) -> Result<(), OmpServiceError> {
        if params.get("intent").and_then(Value::as_str) != Some("user") {
            self.audit(params, action, session_id, "denied:intent");
            return Err(OmpServiceError::new(
                -32602,
                "omp mutations require intent: \"user\"",
            ));
        }
        if params
            .get("request_id")
            .and_then(Value::as_str)
            .is_none_or(|request_id| request_id.trim().is_empty())
        {
            self.audit(params, action, session_id, "denied:request_id");
            return Err(OmpServiceError::invalid("request_id"));
        }
        if let Some(problem) = auth.problem() {
            self.audit(params, action, session_id, "denied:capability_problem");
            return Err(OmpServiceError::unauthorized(&format!(
                "agent control capability is unusable: {problem}"
            )));
        }
        if !auth.is_enabled() {
            self.audit(params, action, session_id, "denied:disabled");
            return Err(OmpServiceError::unauthorized(
                "agent control is disabled; provision the separate agent-control token",
            ));
        }
        if !auth.authorized(params.get("control").and_then(Value::as_str)) {
            self.audit(params, action, session_id, "denied:capability");
            return Err(OmpServiceError::unauthorized(
                "agent control capability is not authorized",
            ));
        }
        Ok(())
    }

    fn audit(&self, params: &Value, action: &str, session_id: &str, outcome: &str) {
        let _ = self.audit.record(AuditEntry {
            at_ms: now_ms(),
            request_id: params
                .get("request_id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
            action: action.to_owned(),
            core_id: "omp".into(),
            terminal: session_id.to_owned(),
            intent: params
                .get("intent")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
            outcome: outcome.to_owned(),
        });
    }

    /// Explicit Host shutdown only. Terminates every omp child. There is no
    /// per-view or per-connection teardown in this module.
    pub(crate) fn shutdown_all(&self) {
        let sessions: Vec<Arc<Session>> = {
            let mut map = self.sessions();
            map.drain().map(|(_, session)| session).collect()
        };
        drop(sessions);
    }
}

/// Drops every Host-owned omp child when the Host service loop exits. A child
/// also exits on stdin EOF if the Host process is killed, because the pipe is
/// closed by the OS; this is why no zombie is left behind.
pub(crate) struct ShutdownGuard;

impl Drop for ShutdownGuard {
    fn drop(&mut self) {
        service().shutdown_all();
    }
}

struct Session {
    session_id: String,
    cwd: String,
    client: OmpClient,
    timeline: Mutex<Timeline>,
    turn: Mutex<OmpTurn>,
    generation: Mutex<u64>,
    updated: Condvar,
    last_sequence: Mutex<u64>,
    unknown_frames: Mutex<u64>,
    error: Mutex<Option<String>>,
    exit_noted: AtomicBool,
}

impl Session {
    fn new(session_id: String, cwd: String, client: OmpClient) -> Self {
        let protocol_version = client
            .ready()
            .map(|ready| ready.protocol_version)
            .unwrap_or(OMP_RPC_PROTOCOL_VERSION);
        let session = Self {
            session_id,
            cwd,
            client,
            timeline: Mutex::new(Timeline::new()),
            turn: Mutex::new(OmpTurn::default()),
            generation: Mutex::new(1),
            updated: Condvar::new(),
            last_sequence: Mutex::new(0),
            unknown_frames: Mutex::new(0),
            error: Mutex::new(None),
            exit_noted: AtomicBool::new(false),
        };
        session
            .timeline
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .push(
                OmpTimelineKind::Status,
                format!(
                    "omp RPC session started · protocol v{protocol_version} · {OMP_PROVENANCE}"
                ),
                None,
                None,
                false,
            );
        session
    }

    fn bump(&self) {
        let mut generation = self
            .generation
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        *generation = generation.saturating_add(1);
        self.updated.notify_all();
    }

    fn wait(&self, after: u64, timeout: Duration) {
        let generation = self
            .generation
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let _ = self
            .updated
            .wait_timeout_while(generation, timeout, |generation| *generation <= after);
    }

    fn begin_user(&self, message: &str) {
        {
            let mut timeline = self
                .timeline
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            timeline.begin_turn();
            timeline.push(OmpTimelineKind::User, message.to_owned(), None, None, false);
        }
        let mut turn = self
            .turn
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        turn.state = OmpTurnState::Running;
        turn.agent_invoked = None;
        turn.started_ms = Some(now_ms());
        turn.finished_ms = None;
        *self
            .error
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = None;
    }

    fn complete_local(&self) {
        let mut turn = self
            .turn
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        turn.state = OmpTurnState::Complete;
        turn.agent_invoked = Some(false);
        turn.finished_ms = Some(now_ms());
    }

    fn mark_aborting(&self) {
        let mut turn = self
            .turn
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if turn.state == OmpTurnState::Running {
            turn.state = OmpTurnState::Aborting;
        }
    }

    fn fail(&self, message: &str) {
        self.timeline
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .push(OmpTimelineKind::Error, message, None, None, true);
        let mut turn = self
            .turn
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        turn.state = OmpTurnState::Error;
        turn.finished_ms = Some(now_ms());
        *self
            .error
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = Some(message.to_owned());
    }

    /// Fold one typed event into turn state, counters and the timeline.
    fn apply(&self, event: &Event) -> bool {
        *self
            .last_sequence
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = event.sequence;
        if matches!(event.kind, EventKind::Unknown { .. }) {
            let mut unknown = self
                .unknown_frames
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            *unknown = unknown.saturating_add(1);
            return false;
        }
        if let EventKind::ProtocolError { message } = &event.kind {
            *self
                .error
                .lock()
                .unwrap_or_else(|poison| poison.into_inner()) = Some(message.clone());
        }
        let timeline_changed = self
            .timeline
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .apply(event);
        let mut turn_changed = false;
        let mut turn = self
            .turn
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        match &event.kind {
            EventKind::AgentStart | EventKind::TurnStart => {
                if turn.state != OmpTurnState::Running {
                    turn.state = OmpTurnState::Running;
                    turn.started_ms = Some(now_ms());
                    turn.finished_ms = None;
                    turn_changed = true;
                }
            }
            EventKind::AgentEnd { is_terminal: true } => {
                turn.state = OmpTurnState::Complete;
                turn.finished_ms = Some(now_ms());
                turn_changed = true;
            }
            EventKind::AgentEnd { is_terminal: false } => {}
            EventKind::PromptResult {
                agent_invoked: false,
                ..
            } => {
                turn.state = OmpTurnState::Complete;
                turn.agent_invoked = Some(false);
                turn.finished_ms = Some(now_ms());
                turn_changed = true;
            }
            EventKind::PromptResult {
                agent_invoked: true,
                ..
            } => {
                turn.agent_invoked = Some(true);
                turn_changed = true;
            }
            _ => {}
        }
        timeline_changed || turn_changed
    }

    fn note_exit(&self) {
        if self.exit_noted.swap(true, Ordering::SeqCst) {
            return;
        }
        let (detail, code) = match self.client.state() {
            OmpState::Running => ("stdout closed".to_owned(), None),
            OmpState::Exited { code, detail } => (detail, code),
        };
        {
            let mut turn = self
                .turn
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            if matches!(turn.state, OmpTurnState::Running | OmpTurnState::Aborting) {
                turn.state = OmpTurnState::Error;
                turn.finished_ms = Some(now_ms());
            }
        }
        let code = code
            .map(|code| format!(" (exit code {code})"))
            .unwrap_or_default();
        self.timeline
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .push(
                OmpTimelineKind::Status,
                format!("omp process ended{code}: {detail}"),
                None,
                None,
                false,
            );
        self.bump();
    }

    fn snapshot(&self) -> Result<Value, OmpServiceError> {
        let generation = *self
            .generation
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let (state, exit) = match self.client.state() {
            OmpState::Running => ("running".to_owned(), None),
            OmpState::Exited { code, detail } => {
                ("exited".to_owned(), Some(OmpExit { code, detail }))
            }
        };
        let cursor = *self
            .last_sequence
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let unknown_frames = *self
            .unknown_frames
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let turn = self
            .turn
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone();
        let (timeline, truncated) = {
            let timeline = self
                .timeline
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            (timeline.entries().to_vec(), timeline.truncated())
        };
        let snapshot = OmpSnapshot {
            session_id: self.session_id.clone(),
            cwd: self.cwd.clone(),
            state,
            protocol_version: self
                .client
                .ready()
                .map(|ready| ready.protocol_version)
                .unwrap_or(OMP_RPC_PROTOCOL_VERSION),
            provenance: OMP_PROVENANCE.to_owned(),
            generation,
            cursor,
            turn,
            timeline,
            truncated,
            unknown_frames,
            stderr_tail: self.client.stderr_tail(),
            exit,
            error: self
                .error
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .clone(),
            restart_note:
                "Host restart does not reattach to a running omp process; start a new session"
                    .into(),
        };
        serde_json::to_value(snapshot)
            .map_err(|error| OmpServiceError::new(-32032, error.to_string()))
    }
}

fn spawn_consumer(session: Weak<Session>, events: EventReceiver) {
    let _ = thread::Builder::new()
        .name("ukk-omp-host-session".into())
        .spawn(move || loop {
            let Some(session) = session.upgrade() else {
                return;
            };
            match events.recv_timeout(Duration::from_millis(500)) {
                Ok(event) => {
                    if session.apply(&event) {
                        session.bump();
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    if !session.client.state().is_running() {
                        session.note_exit();
                        return;
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    session.note_exit();
                    return;
                }
            }
        });
}

fn required_str<'a>(params: &'a Value, key: &str) -> Result<&'a str, OmpServiceError> {
    params
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| OmpServiceError::invalid(key))
}

fn service() -> &'static OmpService {
    static SERVICE: std::sync::LazyLock<OmpService> =
        std::sync::LazyLock::new(OmpService::from_env);
    &SERVICE
}

/// Live dispatcher used by the local JSON-RPC surface.
pub(crate) fn dispatch(method: &str, params: &Value) -> Result<Value, OmpServiceError> {
    match method {
        "omp.snapshot" => service().snapshot(params),
        "omp.wait" => service().wait(params),
        "omp.start" => service().start(params, &AgentControlAuth::from_env()),
        "omp.prompt" => service().prompt(params, &AgentControlAuth::from_env()),
        "omp.abort" => service().abort(params, &AgentControlAuth::from_env()),
        _ => Err(OmpServiceError::new(
            -32601,
            format!("unknown omp method {method}"),
        )),
    }
}

/// Status surface for `host.ping` / diagnostics. Contains no secrets.
pub(crate) fn status() -> Value {
    let auth = AgentControlAuth::from_env();
    json!({
        "capability": "separate-agent-control",
        "enabled": auth.is_enabled(),
        "problem": auth.problem(),
        "provenance": OMP_PROVENANCE,
        "protocol_version": OMP_RPC_PROTOCOL_VERSION,
        "mutations": ["omp.start", "omp.prompt", "omp.abort"],
        "read_only": ["omp.snapshot", "omp.wait"],
        "view_close_stops_omp": false,
        "host_restart_reattaches": false
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ukk_omp_client::OmpConfig;

    fn mock_config(scenario: &str) -> OmpConfig {
        OmpConfig::default()
            .with_binary(mock_binary())
            .with_args(["--scenario", scenario])
            .with_startup_timeout(Duration::from_secs(5))
            .with_request_timeout(Duration::from_secs(5))
    }

    /// The mock child is a bin target of `ukk-omp-client`. `cargo test
    /// --workspace` builds it into the shared target directory; a filtered
    /// `cargo test -p ukk-host` run can point at it with `UKK_OMP_MOCK_BIN`.
    fn mock_binary() -> std::path::PathBuf {
        if let Some(path) = env::var_os("UKK_OMP_MOCK_BIN") {
            return path.into();
        }
        let exe = std::env::current_exe().expect("test executable path");
        let target = exe
            .parent()
            .and_then(|deps| deps.parent())
            .expect("target/debug directory");
        let name = if cfg!(windows) {
            "ukk-omp-mock-rpc.exe"
        } else {
            "ukk-omp-mock-rpc"
        };
        let path = target.join(name);
        assert!(
            path.is_file(),
            "missing {}; run `cargo build -p ukk-omp-client --bin ukk-omp-mock-rpc` \
             or set UKK_OMP_MOCK_BIN",
            path.display()
        );
        path
    }

    fn auth() -> AgentControlAuth {
        AgentControlAuth::test("omp-service-test-capability")
    }

    fn mutation(session_id: &str) -> Value {
        json!({
            "session_id": session_id,
            "request_id": "req-1",
            "intent": "user",
            "control": "omp-service-test-capability",
            "cwd": env::temp_dir().to_string_lossy(),
        })
    }

    #[test]
    fn mutations_require_the_separate_agent_control_capability() {
        let service = OmpService::with_config(mock_config("text"));
        let mut params = mutation("s1");
        params["control"] = json!("wrong");
        let error = service
            .start(&params, &auth())
            .expect_err("wrong token must be denied");
        assert_eq!(error.code, -32030);
        let denied = service.audit_entries();
        assert_eq!(denied.len(), 1);
        assert_eq!(denied[0].outcome, "denied:capability");
        assert!(service.snapshot(&json!({"session_id":"s1"})).is_err());
    }

    #[test]
    fn disabled_capability_denies_mutations() {
        let service = OmpService::with_config(mock_config("text"));
        let error = service
            .start(&mutation("s1"), &AgentControlAuth::disabled_test())
            .expect_err("disabled control must deny");
        assert_eq!(error.code, -32030);
    }

    #[test]
    fn read_only_snapshot_does_not_require_a_capability() {
        let service = OmpService::with_config(mock_config("text"));
        service.start(&mutation("s1"), &auth()).unwrap();
        let snapshot = service.snapshot(&json!({"session_id":"s1"})).unwrap();
        assert_eq!(snapshot["state"], "running");
        assert_eq!(snapshot["protocol_version"], 1);
        assert_eq!(snapshot["provenance"], OMP_PROVENANCE);
        assert!(snapshot["restart_note"]
            .as_str()
            .unwrap()
            .contains("does not reattach"));
    }

    #[test]
    fn prompt_streams_a_real_timeline_and_completes() {
        let service = OmpService::with_config(mock_config("text"));
        service.start(&mutation("s1"), &auth()).unwrap();
        let mut prompt = mutation("s1");
        prompt["message"] = json!("Reply with exactly: OMP-OK");
        let snapshot = service.prompt(&prompt, &auth()).unwrap();
        assert_eq!(snapshot["turn"]["state"], "running");
        assert!(snapshot["timeline"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["kind"] == "user"));
        let completed = wait_for(&service, "s1", |snapshot| {
            snapshot["turn"]["state"] == "complete"
        });
        let texts: Vec<&str> = completed["timeline"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|entry| entry["kind"] == "assistant")
            .map(|entry| entry["text"].as_str().unwrap_or_default())
            .collect();
        assert_eq!(texts.join(""), "OMP-MOCK");
        assert!(completed["cursor"].as_u64().unwrap() > 0);
        let audit = service.audit_entries();
        assert!(audit
            .iter()
            .any(|entry| entry.action == "omp.prompt" && entry.outcome == "ok"));
    }

    #[test]
    fn tool_events_become_bounded_tool_entries() {
        let service = OmpService::with_config(mock_config("tool"));
        service.start(&mutation("s1"), &auth()).unwrap();
        let mut prompt = mutation("s1");
        prompt["message"] = json!("run the tool");
        service.prompt(&prompt, &auth()).unwrap();
        let completed = wait_for(&service, "s1", |snapshot| {
            snapshot["turn"]["state"] == "complete"
        });
        let tool: Vec<&Value> = completed["timeline"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|entry| entry["kind"] == "tool")
            .collect();
        assert_eq!(tool.len(), 1, "{completed}");
        assert_eq!(tool[0]["tool_name"], "bash");
        assert_eq!(tool[0]["text"], "OMP-TOOL\n");
        assert_eq!(tool[0]["is_error"], false);
    }

    #[test]
    fn local_command_finishes_without_agent_end() {
        let service = OmpService::with_config(mock_config("local"));
        service.start(&mutation("s1"), &auth()).unwrap();
        let mut prompt = mutation("s1");
        prompt["message"] = json!("/status");
        service.prompt(&prompt, &auth()).unwrap();
        let completed = wait_for(&service, "s1", |snapshot| {
            snapshot["turn"]["state"] == "complete"
                && snapshot["timeline"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|entry| {
                        entry["kind"] == "status"
                            && entry["text"]
                                .as_str()
                                .is_some_and(|text| text.contains("Handled locally by omp"))
                    })
        });
        assert_eq!(completed["turn"]["agent_invoked"], false);
        assert!(completed["timeline"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| {
                entry["kind"] == "status"
                    && entry["text"]
                        .as_str()
                        .is_some_and(|text| text.contains("Handled locally by omp"))
            }));
    }

    #[test]
    fn abort_cancels_the_turn() {
        let service = OmpService::with_config(mock_config("commands"));
        service.start(&mutation("s1"), &auth()).unwrap();
        let mut prompt = mutation("s1");
        prompt["message"] = json!("run the tool");
        service.prompt(&prompt, &auth()).unwrap();
        let mut abort = mutation("s1");
        abort["request_id"] = json!("req-2");
        let snapshot = service.abort(&abort, &auth()).unwrap();
        assert!(snapshot["generation"].as_u64().unwrap() > 1);
        assert!(service.audit_entries().iter().any(|entry| {
            entry.action == "omp.abort" && entry.outcome == "ok" && entry.request_id == "req-2"
        }));
    }

    #[test]
    fn busy_prompt_is_rejected_instead_of_steering_silently() {
        let service = OmpService::with_config(mock_config("commands"));
        service.start(&mutation("s1"), &auth()).unwrap();
        let mut prompt = mutation("s1");
        prompt["message"] = json!("first");
        prompt["request_id"] = json!("req-1");
        service
            .prompt(&prompt, &auth())
            .expect("mock acknowledges the first prompt");
        let mut second = mutation("s1");
        second["message"] = json!("second");
        second["request_id"] = json!("req-2");
        let error = service.prompt(&second, &auth()).unwrap_err();
        assert_eq!(error.code, -32041);
    }

    #[test]
    fn bounded_timeline_reports_truncation() {
        let service = OmpService::with_config(mock_config("oversized"));
        service.start(&mutation("s1"), &auth()).unwrap();
        let mut prompt = mutation("s1");
        prompt["message"] = json!("oversized");
        service.prompt(&prompt, &auth()).unwrap();
        let completed = wait_for(&service, "s1", |snapshot| {
            snapshot["turn"]["state"] == "complete"
        });
        assert!(completed["timeline"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["text"]
                .as_str()
                .is_some_and(|text| text.contains("65536-byte v1 transport limit"))));
    }

    #[test]
    fn snapshot_and_wait_never_stop_the_child() {
        let service = OmpService::with_config(mock_config("text"));
        service.start(&mutation("s1"), &auth()).unwrap();
        for _ in 0..3 {
            let snapshot = service
                .wait(&json!({"session_id":"s1","after_generation":0,"timeout_ms":10}))
                .unwrap();
            assert_eq!(snapshot["state"], "running");
        }
        // Simulate a Desktop view closing: no Host call at all. A later
        // read-only snapshot still sees the same live child.
        thread::sleep(Duration::from_millis(50));
        let snapshot = service.snapshot(&json!({"session_id":"s1"})).unwrap();
        assert_eq!(snapshot["state"], "running");
        service.shutdown_all();
        let snapshot = service.snapshot(&json!({"session_id":"s1"})).unwrap_err();
        assert_eq!(snapshot.code, -32034);
    }

    #[test]
    fn unknown_session_is_a_not_found_error() {
        let service = OmpService::with_config(mock_config("text"));
        let error = service
            .snapshot(&json!({"session_id":"missing"}))
            .unwrap_err();
        assert_eq!(error.code, -32034);
    }

    #[test]
    fn a_second_start_does_not_spawn_a_second_child() {
        let service = OmpService::with_config(mock_config("text"));
        let first = service.start(&mutation("s1"), &auth()).unwrap();
        let second = service.start(&mutation("s1"), &auth()).unwrap();
        assert_eq!(first["generation"], second["generation"]);
        assert_eq!(service.sessions().len(), 1);
    }

    /// Real omp through the Host service (the code path behind `omp.*`).
    /// Ignored by default: needs a logged-in omp binary and model access.
    #[test]
    #[ignore = "requires a logged-in real omp binary and model access"]
    fn real_omp_conversation_reaches_the_host_timeline() {
        let cwd = env::temp_dir().join(format!("ukk-omp-host-smoke-{}", std::process::id()));
        std::fs::create_dir_all(&cwd).unwrap();
        let config = OmpConfig::default()
            .with_cwd(&cwd)
            .with_startup_timeout(Duration::from_secs(90))
            .with_request_timeout(Duration::from_secs(60));
        let service = OmpService::with_config(config);
        let mut start = mutation("s1");
        start["cwd"] = json!(cwd.to_string_lossy());
        let started = service.start(&start, &auth()).unwrap();
        assert_eq!(started["state"], "running");
        let mut prompt = mutation("s1");
        prompt["message"] = json!("Reply with exactly: OMP-OK");
        service.prompt(&prompt, &auth()).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(240);
        let completed = loop {
            let snapshot = service
                .wait(&json!({"session_id":"s1","after_generation":0,"timeout_ms":1000}))
                .unwrap();
            if snapshot["turn"]["state"] == "complete" {
                break snapshot;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "real omp turn did not complete: {snapshot}"
            );
        };
        eprintln!("real host snapshot: {completed}");
        let text: String = completed["timeline"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|entry| entry["kind"] == "assistant")
            .map(|entry| entry["text"].as_str().unwrap_or_default())
            .collect();
        assert!(text.contains("OMP-OK"), "assistant text: {text:?}");
        assert!(service
            .audit_entries()
            .iter()
            .any(|entry| entry.action == "omp.prompt" && entry.outcome == "ok"));
        service.shutdown_all();
        let _ = std::fs::remove_dir_all(&cwd);
    }

    fn wait_for(
        service: &OmpService,
        session_id: &str,
        predicate: impl Fn(&Value) -> bool,
    ) -> Value {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let snapshot = service
                .wait(&json!({"session_id": session_id, "after_generation": 0, "timeout_ms": 100}))
                .unwrap();
            if predicate(&snapshot) {
                return snapshot;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "condition not reached; last snapshot {snapshot}"
            );
        }
    }
}
