//! Read-only live terminal state assembled from herdr-core's documented
//! events.subscribe snapshot + notifications. The hc event sequence is scoped
//! to one core incarnation; ukk's process-local revision is separate.
use serde_json::{json, Map, Value};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use ukk_hc_client::{HcClient, HcError};

#[derive(Clone, Debug)]
struct State {
    revision: u64,
    hc_seq: Option<u64>,
    connected: bool,
    terminals: Vec<Value>,
    error: Option<String>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            revision: 0,
            hc_seq: None,
            connected: false,
            terminals: Vec::new(),
            error: Some("Waiting for herdr-core".into()),
        }
    }
}

/// One process-local event broker shared by all local socket clients.
pub(crate) struct Broker {
    epoch: String,
    inner: Mutex<State>,
    updated: Condvar,
}

impl Broker {
    pub(crate) fn new() -> Arc<Self> {
        let started = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        Arc::new(Self {
            epoch: format!("{}-{started}", std::process::id()),
            inner: Mutex::new(State::default()),
            updated: Condvar::new(),
        })
    }

    pub(crate) fn start(self: &Arc<Self>) {
        let broker = Arc::clone(self);
        thread::spawn(move || loop {
            let client = HcClient::default();
            let result = client.watch(|message| {
                broker.apply(&message).map_err(HcError::Protocol)
            });
            broker.mark_offline(
                result
                    .err()
                    .map(|e| e.to_string())
                    .unwrap_or_else(|| "hc subscription ended".into()),
            );
            thread::sleep(Duration::from_secs(2));
        });
    }

    /// Applies a real hc notification. If this returns an error, caller must
    /// end the subscription and obtain a fresh snapshot on a new connection.
    fn apply(&self, message: &Value) -> Result<(), String> {
        let mut guard = self.inner.lock().map_err(|_| "broker lock poisoned")?;
        let changed = match message.get("kind").and_then(Value::as_str) {
            Some("snapshot") => {
                let data = message.get("data").ok_or("missing subscription snapshot")?;
                replace_snapshot(&mut guard, data)?;
                true
            }
            Some("notification") => {
                let notification = message.get("data").ok_or("missing hc notification")?;
                apply_notification(&mut guard, notification)?
            }
            _ => return Err("unexpected hc message kind".into()),
        };
        if changed {
            guard.revision = guard.revision.saturating_add(1);
            self.updated.notify_all();
        }
        Ok(())
    }

    fn mark_offline(&self, error: String) {
        if let Ok(mut guard) = self.inner.lock() {
            if guard.connected || guard.error.as_deref() != Some(error.as_str()) {
                guard.connected = false;
                guard.error = Some(error);
                guard.hc_seq = None;
                guard.revision = guard.revision.saturating_add(1);
                self.updated.notify_all();
            }
        }
    }

    pub(crate) fn snapshot(&self) -> Value {
        let guard = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        self.snapshot_inner(&guard)
    }

    pub(crate) fn wait(&self, after_epoch: Option<&str>, after_revision: u64, ms: u64) -> Value {
        let guard = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let bounded = Duration::from_millis(ms.clamp(1, 10_000));
        let guard = self
            .updated
            .wait_timeout_while(guard, bounded, |current| {
                after_epoch == Some(self.epoch.as_str()) && current.revision == after_revision
            })
            .unwrap_or_else(|p| p.into_inner())
            .0;
        self.snapshot_inner(&guard)
    }

    fn snapshot_inner(&self, state: &State) -> Value {
        json!({
            "host_epoch": self.epoch,
            "revision": state.revision,
            "hc_seq": state.hc_seq,
            "connected": state.connected,
            "message": state.error,
            "terminals": state.terminals,
            // events.subscribe does not provide terminal.list's machines or
            // hidden_finished fields. Never fabricate their values.
            "source": "events.subscribe"
        })
    }
}

fn replace_snapshot(state: &mut State, data: &Value) -> Result<(), String> {
    let seq = data
        .get("seq")
        .and_then(Value::as_u64)
        .ok_or("snapshot missing hc seq")?;
    let terminals = data
        .get("terminals")
        .and_then(Value::as_array)
        .ok_or("snapshot missing terminals")?;
    for item in terminals {
        if item.get("address").and_then(Value::as_str).is_none() {
            return Err("snapshot terminal missing address".into());
        }
    }
    state.terminals = terminals.clone();
    state.hc_seq = Some(seq);
    state.connected = true;
    state.error = None;
    Ok(())
}

fn apply_notification(state: &mut State, notification: &Value) -> Result<bool, String> {
    let method = notification.get("method").and_then(Value::as_str).ok_or("missing method")?;
    let params = notification.get("params").ok_or("missing params")?;
    let seq = params.get("seq").and_then(Value::as_u64).ok_or("missing seq")?;
    match method {
        "missed" => {
            // herdr-core provides a fresh authoritative snapshot on missed.
            replace_snapshot(state, params)?;
            Ok(true)
        }
        "heartbeat" => {
            // No content revision: heartbeats only confirm stream liveness.
            Ok(false)
        }
        "event" => {
            let last = state.hc_seq.ok_or("event before snapshot")?;
            if seq <= last {
                return Ok(false); // already applied / replayed
            }
            if seq != last.saturating_add(1) {
                return Err(format!("hc event gap: expected {}, received {seq}", last + 1));
            }
            let event_kind = params.get("type").and_then(Value::as_str).ok_or("missing event type")?;
            let address = params.get("terminal").and_then(Value::as_str).ok_or("missing terminal address")?;
            let data = params.get("data").unwrap_or(&Value::Null);
            let changed = apply_terminal_delta(&mut state.terminals, event_kind, address, data)?;
            state.hc_seq = Some(seq);
            Ok(changed)
        }
        _ => Err(format!("unknown hc notification method {method}")),
    }
}

fn apply_terminal_delta(terminals: &mut Vec<Value>, kind: &str, address: &str, data: &Value) -> Result<bool, String> {
    let existing = terminals
        .iter()
        .position(|item| item.get("address").and_then(Value::as_str) == Some(address));
    match kind {
        "created" | "restarted" => {
            let mut record = data.clone();
            let object = record.as_object_mut().ok_or("created/restarted data must be object")?;
            object.insert("address".into(), Value::String(address.to_owned()));
            if let Some(index) = existing {
                terminals[index] = record;
            } else {
                terminals.push(record);
            }
            Ok(true)
        }
        "removed" => {
            if let Some(index) = existing {
                terminals.remove(index);
                Ok(true)
            } else {
                Ok(false)
            }
        }
        "doc" => Ok(false), // doc state belongs to a different ukk model.
        "lost" | "finished" | "keep" | "status" | "turn" | "title" | "labels" | "size" | "foreground" => {
            let index = existing.ok_or_else(|| format!("hc {kind} event for unknown terminal {address}"))?;
            let record = terminals[index].as_object_mut().ok_or("terminal record is not object")?;
            match kind {
                "lost" => {
                    patch_keys(record, data, &["reason", "exit_code", "signal", "finished_ms"])?;
                    record.insert("pid".into(), Value::Null);
                    record.insert("foreground".into(), Value::Null);
                }
                "finished" => {
                    patch_keys(record, data, &["reason", "exit_code", "signal", "finished_ms"])?;
                    record.insert("state".into(), json!("finished"));
                    record.insert("foreground".into(), Value::Null);
                    record.insert("status".into(), Value::Null);
                }
                "status" => patch_keys(record, data, &["status", "records", "reported"])?,
                "turn" => patch_keys(record, data, &["turn"])?,
                "keep" => patch_keys(record, data, &["keep"])?,
                "title" => patch_keys(record, data, &["title"])?,
                "labels" => patch_keys(record, data, &["labels"])?,
                "size" => patch_keys(record, data, &["cols", "rows"])?,
                "foreground" => { record.insert("foreground".into(), data.clone()); },
                _ => unreachable!(),
            }
            Ok(true)
        }
        other => Err(format!("unknown hc event type {other} — must resubscribe")),
    }
}

fn patch_keys(target: &mut Map<String, Value>, source: &Value, keys: &[&str]) -> Result<(), String> {
    let object = source.as_object().ok_or("hc event data must be object")?;
    for key in keys {
        if let Some(value) = object.get(*key) {
            target.insert((*key).to_owned(), value.clone());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(seq: u64) -> Value {
        json!({"kind":"snapshot","data":{"seq":seq,"terminals":[
            {"address":"t3","name":"Codex","state":"live","cwd":"/repo",
             "argv":["codex"],"status":{"state":"working"},"parent":null}
        ]}})
    }

    fn event(seq: u64, typ: &str, terminal: &str, data: Value) -> Value {
        json!({"kind":"notification","data":{"jsonrpc":"2.0","method":"event",
          "params":{"seq":seq,"type":typ,"terminal":terminal,"data":data}}})
    }

    #[test]
    fn keeps_agent_state_from_source_events() {
        let broker = Broker::new();
        broker.apply(&snapshot(5)).unwrap();
        broker.apply(&event(6, "status", "t3", json!({"status":{"state":"blocked"},"records":{},"reported":true}))).unwrap();
        let state = broker.snapshot();
        assert_eq!(state["terminals"][0]["status"]["state"], "blocked");
        assert_eq!(state["revision"], 2);
        assert_eq!(state["hc_seq"], 6);
        assert_eq!(state["connected"], true);
    }

    #[test]
    fn creates_and_removes_child_agent_dynamically() {
        let broker = Broker::new();
        broker.apply(&snapshot(1)).unwrap();
        broker.apply(&event(2, "created", "t4", json!({"address":"t4","parent":"t3","state":"live",
            "cwd":"/repo","argv":["claude"]}))).unwrap();
        assert_eq!(broker.snapshot()["terminals"][1]["parent"], "t3");
        broker.apply(&event(3, "removed", "t4", json!({}))).unwrap();
        assert_eq!(broker.snapshot()["terminals"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn missed_resets_from_authoritative_snapshot() {
        let broker = Broker::new();
        broker.apply(&snapshot(10)).unwrap();
        broker.apply(&json!({"kind":"notification","data":{"method":"missed",
          "params":{"seq":20,"terminals":[{"address":"t9","state":"live","cwd":"/repo","argv":["codex"]}]}}})).unwrap();
        assert_eq!(broker.snapshot()["terminals"][0]["address"], "t9");
        assert_eq!(broker.snapshot()["hc_seq"], 20);
    }

    #[test]
    fn gap_requires_new_subscription() {
        let broker = Broker::new();
        broker.apply(&snapshot(10)).unwrap();
        let error = broker.apply(&event(12, "removed", "t3", json!({}))).unwrap_err();
        assert!(error.contains("gap"));
        assert_eq!(broker.snapshot()["terminals"].as_array().unwrap().len(), 1);
        assert_eq!(broker.snapshot()["hc_seq"], 10);
    }

    #[test]
    fn seq_can_reset_after_new_subscription() {
        let broker = Broker::new();
        broker.apply(&snapshot(300)).unwrap();
        broker.mark_offline("disconnected".into());
        broker.apply(&snapshot(2)).unwrap();
        assert_eq!(broker.snapshot()["hc_seq"], 2);
        assert_eq!(broker.snapshot()["connected"], true);
    }

    #[test]
    fn heartbeat_does_not_increment_revision() {
        let broker = Broker::new();
        broker.apply(&snapshot(5)).unwrap();
        broker.apply(&json!({"kind":"notification","data":{"method":"heartbeat","params":{"seq":5}}})).unwrap();
        assert_eq!(broker.snapshot()["revision"], 1);
    }

    #[test]
    fn wait_immediately_returns_if_epoch_or_revision_differs() {
        let broker = Broker::new();
        broker.apply(&snapshot(1)).unwrap();
        let snap = broker.wait(Some("different"), 1, 10_000);
        assert_eq!(snap["revision"], 1);
        let snap = broker.wait(Some(&broker.epoch), 0, 10_000);
        assert_eq!(snap["revision"], 1);
    }

    #[test]
    fn failed_event_does_not_falsely_publish_revision() {
        let broker = Broker::new();
        broker.apply(&snapshot(2)).unwrap();
        assert!(broker.apply(&event(3, "unsupported", "t3", json!({}))).is_err());
        assert_eq!(broker.snapshot()["revision"], 1);
    }
}
