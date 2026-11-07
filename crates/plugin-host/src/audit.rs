//! Append-only audit trail for plugin life-cycle and authorization decisions.
//!
//! Records deliberately carry ids and reasons, never command arguments, event
//! payloads, file contents or screen data. A sink may be a log file, SQLite, or
//! an in-memory collector in tests.
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::redact::redact;

/// Result of the audited operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AuditOutcome {
    Allowed,
    Denied,
    Failed,
}

impl AuditOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allowed => "allowed",
            Self::Denied => "denied",
            Self::Failed => "failed",
        }
    }
}

/// What happened. Extend only with new variants; existing text is stable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AuditKind {
    PluginInstalled,
    PluginUninstalled,
    DuplicatePlugin,
    DuplicateContribution,
    ManifestRejected,
    PluginEnabled,
    PluginDisabled,
    EnableFailed,
    CommandInvoked,
    CommandDenied,
    UnknownCommand,
    ForgedRequest,
    HelperCrashed,
    HelperTimeout,
    ProtocolViolation,
    EventDropped,
    GrantGranted,
    GrantRevoked,
}

impl AuditKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PluginInstalled => "plugin_installed",
            Self::PluginUninstalled => "plugin_uninstalled",
            Self::DuplicatePlugin => "duplicate_plugin",
            Self::DuplicateContribution => "duplicate_contribution",
            Self::ManifestRejected => "manifest_rejected",
            Self::PluginEnabled => "plugin_enabled",
            Self::PluginDisabled => "plugin_disabled",
            Self::EnableFailed => "enable_failed",
            Self::CommandInvoked => "command_invoked",
            Self::CommandDenied => "command_denied",
            Self::UnknownCommand => "unknown_command",
            Self::ForgedRequest => "forged_request",
            Self::HelperCrashed => "helper_crashed",
            Self::HelperTimeout => "helper_timeout",
            Self::ProtocolViolation => "protocol_violation",
            Self::EventDropped => "event_dropped",
            Self::GrantGranted => "grant_granted",
            Self::GrantRevoked => "grant_revoked",
        }
    }
}

/// One audit record. `detail` is a short, redaction-safe explanation: it is
/// passed through [`redact`] on construction, so a helper-authored string can
/// never inject newlines, ANSI escapes or unbounded text into a log sink.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditRecord {
    pub at_ms: i64,
    pub plugin_id: Option<String>,
    pub kind: AuditKind,
    pub outcome: AuditOutcome,
    pub detail: String,
}

impl AuditRecord {
    pub fn new(
        plugin_id: Option<&str>,
        kind: AuditKind,
        outcome: AuditOutcome,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            at_ms: now_ms(),
            plugin_id: plugin_id.map(str::to_owned),
            kind,
            outcome,
            detail: redact(&detail.into()),
        }
    }
}

/// Destination for audit records. Implementations must not block for long and
/// must never panic on a record.
pub trait AuditSink: Send + Sync {
    fn record(&self, record: &AuditRecord);
}

/// Discards everything. Only appropriate for tests or when the embedding host
/// installs a durable sink.
#[derive(Debug, Default)]
pub struct NullAuditSink;

impl AuditSink for NullAuditSink {
    fn record(&self, _record: &AuditRecord) {}
}

/// In-memory sink used by tests and by the headless demo.
#[derive(Debug, Default)]
pub struct MemoryAuditSink {
    records: Mutex<Vec<AuditRecord>>,
}

impl MemoryAuditSink {
    pub fn new() -> Self {
        Self::default()
    }

    /// Snapshot of all records in insertion order.
    pub fn records(&self) -> Vec<AuditRecord> {
        lock(&self.records).clone()
    }

    pub fn len(&self) -> usize {
        lock(&self.records).len()
    }

    pub fn is_empty(&self) -> bool {
        lock(&self.records).is_empty()
    }

    /// Records of one kind, in insertion order.
    pub fn kinds(&self) -> Vec<AuditKind> {
        lock(&self.records)
            .iter()
            .map(|record| record.kind)
            .collect()
    }
}

impl AuditSink for MemoryAuditSink {
    fn record(&self, record: &AuditRecord) {
        lock(&self.records).push(record.clone());
    }
}

/// Recover a poisoned lock instead of panicking: a dropped audit record must
/// not take down the supervisor.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Wall-clock milliseconds since the Unix epoch.
///
/// Grants use wall-clock expiry so a lease survives a suspend/resume cycle the
/// way a user expects. The trade-off is explicit: a backwards clock step can
/// extend an expired lease. Callers that need strict monotonicity must revoke
/// on resume; this is documented as a residual limitation, not a guarantee.
pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_sink_preserves_order_and_redacts_nothing_extra() {
        let sink = MemoryAuditSink::new();
        sink.record(&AuditRecord::new(
            Some("dev.ukk.sample"),
            AuditKind::PluginEnabled,
            AuditOutcome::Allowed,
            "enabled",
        ));
        sink.record(&AuditRecord::new(
            None,
            AuditKind::ManifestRejected,
            AuditOutcome::Denied,
            "unsupported_manifest_version",
        ));
        assert_eq!(
            sink.kinds(),
            vec![AuditKind::PluginEnabled, AuditKind::ManifestRejected]
        );
        assert_eq!(sink.records()[1].plugin_id, None);
    }

    #[test]
    fn poisoned_audit_mutex_is_recovered() {
        let sink = std::sync::Arc::new(MemoryAuditSink::new());
        let clone = std::sync::Arc::clone(&sink);
        let _ = std::thread::spawn(move || {
            let _guard = lock(&clone.records);
            panic!("poison on purpose");
        })
        .join();
        sink.record(&AuditRecord::new(
            None,
            AuditKind::EventDropped,
            AuditOutcome::Failed,
            "test",
        ));
        assert_eq!(sink.len(), 1);
    }
}
