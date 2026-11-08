//! Bridge from the read-only herdr-core broker to typed durable protocol
//! events.
//!
//! The runtime broker owns a process-local snapshot and `hc_seq`, neither of
//! which is a durable protocol cursor. This bridge only translates a
//! **structured** terminal status change into
//! [`ukk_protocol::Event::AgentRunStatusChanged`] for a terminal that is
//! already bound to a persisted `AgentRun`. The store then assigns a durable
//! cursor and open subscriptions receive the event live.
//!
//! Deliberate limits:
//!
//! * an unbound terminal never gains an Agent identity implicitly;
//! * `hc_seq`, `host_epoch` and the broker revision are never wire cursors;
//! * only `working`/`idle` map to `Running`/`Idle`. `blocked` and every other
//!   state fall back to `Unknown`; an approval is never inferred from terminal
//!   status. Structured approvals remain a separate flow.

use super::broker::Broker;
use super::protocol_service::{now_ms, ProtocolHost};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;
use ukk_protocol::AgentStatus;

/// How long the bridge waits for a broker revision before checking its own
/// shutdown-independent loop state again.
const POLL_MS: u64 = 500;

pub(crate) fn start(host: Arc<ProtocolHost>, broker: Arc<Broker>) {
    std::thread::spawn(move || watch(host, broker));
}

fn watch(host: Arc<ProtocolHost>, broker: Arc<Broker>) {
    let mut epoch = broker.epoch().to_owned();
    let mut revision = 0u64;
    let mut published: BTreeMap<String, AgentStatus> = BTreeMap::new();
    loop {
        let snapshot = broker.wait(Some(&epoch), revision, POLL_MS);
        let Some(current_epoch) = snapshot.get("host_epoch").and_then(Value::as_str) else {
            continue;
        };
        if current_epoch != epoch {
            epoch = current_epoch.to_owned();
            revision = 0;
            published.clear();
            continue;
        }
        revision = snapshot
            .get("revision")
            .and_then(Value::as_u64)
            .unwrap_or(revision);
        let Some(terminals) = snapshot.get("terminals").and_then(Value::as_array) else {
            continue;
        };
        for terminal in terminals {
            let Some(address) = terminal.get("address").and_then(Value::as_str) else {
                continue;
            };
            let status = structured_status(terminal);
            if published.get(address) == Some(&status) {
                continue;
            }
            match host.publish_broker_status(address, status, now_ms()) {
                Ok(true) => {
                    published.insert(address.to_owned(), status);
                }
                // Unbound terminal: keep it out of the published map so a
                // later binding is observed on the next change.
                Ok(false) => {}
                Err(error) => eprintln!("ukk broker status bridge: {error}"),
            }
        }
    }
}

/// Structured status mapping. Anything unclear is explicit `Unknown`.
fn structured_status(terminal: &Value) -> AgentStatus {
    match terminal
        .get("status")
        .and_then(|status| status.get("state"))
        .and_then(Value::as_str)
    {
        Some("working") => AgentStatus::Running,
        Some("idle") => AgentStatus::Idle,
        _ => AgentStatus::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn maps_only_unambiguous_structured_states() {
        assert_eq!(
            structured_status(&json!({"status": {"state": "working"}})),
            AgentStatus::Running
        );
        assert_eq!(
            structured_status(&json!({"status": {"state": "idle"}})),
            AgentStatus::Idle
        );
        assert_eq!(
            structured_status(&json!({"status": {"state": "blocked"}})),
            AgentStatus::Unknown,
            "approval must never be inferred from terminal status"
        );
        assert_eq!(structured_status(&json!({})), AgentStatus::Unknown);
    }
}
