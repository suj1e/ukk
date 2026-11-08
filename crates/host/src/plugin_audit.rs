//! Bounded in-memory audit sink for Host plugin decisions.
//!
//! This is deliberately **not durable**: a Host restart discards the retained
//! window. It exists so capability denials, registry rejections and supervisor
//! failures are recorded instead of silently dropped by `NullAuditSink`, while
//! a hostile plugin cannot grow memory without bound. Durable audit storage is
//! a follow-up.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use ukk_plugin_host::{AuditRecord, AuditSink};

const DEFAULT_CAPACITY: usize = 512;

pub(crate) struct BoundedAuditSink {
    records: Mutex<VecDeque<AuditRecord>>,
    capacity: usize,
    dropped: AtomicU64,
}

impl Default for BoundedAuditSink {
    fn default() -> Self {
        Self::new(DEFAULT_CAPACITY)
    }
}

impl BoundedAuditSink {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            records: Mutex::new(VecDeque::with_capacity(capacity.min(DEFAULT_CAPACITY))),
            capacity: capacity.max(1),
            dropped: AtomicU64::new(0),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, VecDeque<AuditRecord>> {
        self.records
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    #[cfg(test)]
    pub(crate) fn records(&self) -> Vec<AuditRecord> {
        self.lock().iter().cloned().collect()
    }

    #[cfg(test)]
    pub(crate) fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    pub(crate) fn summary(&self) -> (usize, u64) {
        (self.lock().len(), self.dropped.load(Ordering::Relaxed))
    }
}

impl AuditSink for BoundedAuditSink {
    fn record(&self, record: &AuditRecord) {
        let mut records = self.lock();
        if records.len() >= self.capacity {
            records.pop_front();
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
        records.push_back(record.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ukk_plugin_host::{AuditKind, AuditOutcome, AuditSink};

    fn record(index: usize) -> AuditRecord {
        AuditRecord::new(
            Some("dev.ukk.sample.session-action"),
            AuditKind::CommandDenied,
            AuditOutcome::Denied,
            format!("denial {index}"),
        )
    }

    #[test]
    fn capacity_is_bounded_and_oldest_records_are_counted() {
        let sink = BoundedAuditSink::new(2);
        sink.record(&record(1));
        sink.record(&record(2));
        sink.record(&record(3));
        let records = sink.records();
        assert_eq!(records.len(), 2);
        assert!(records[0].detail.contains("denial 2"));
        assert!(records[1].detail.contains("denial 3"));
        assert_eq!(sink.dropped(), 1);
        assert_eq!(sink.summary(), (2, 1));
    }

    #[test]
    fn zero_capacity_still_retains_one_record() {
        let sink = BoundedAuditSink::new(0);
        sink.record(&record(1));
        assert_eq!(sink.records().len(), 1);
    }
}
