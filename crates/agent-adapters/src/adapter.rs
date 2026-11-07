//! Adapter configuration, trait and parse outcome.

use crate::capability::{CapabilityDescriptor, ProviderKind};
use crate::contract::{AdapterEvent, TerminalFallback};
use crate::error::AdapterError;
use crate::ids::{SessionId, StreamId};
use crate::limits::Limits;
use crate::redact::{NoopRedactor, Redactor};
use std::sync::Arc;

#[derive(Clone)]
pub struct AdapterConfig {
    pub limits: Limits,
    pub redactor: Arc<dyn Redactor>,
    /// When set, a session start matching this id is emitted as a resume.
    pub resume_session: Option<SessionId>,
}

impl Default for AdapterConfig {
    fn default() -> Self {
        Self {
            limits: Limits::default(),
            redactor: Arc::new(NoopRedactor),
            resume_session: None,
        }
    }
}

impl AdapterConfig {
    pub fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    pub fn with_redactor(mut self, redactor: impl Redactor + 'static) -> Self {
        self.redactor = Arc::new(redactor);
        self
    }

    pub fn with_redactor_arc(mut self, redactor: Arc<dyn Redactor>) -> Self {
        self.redactor = redactor;
        self
    }

    pub fn with_resume_session(mut self, session_id: impl Into<SessionId>) -> Self {
        self.resume_session = Some(session_id.into());
        self
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ParseOutcome {
    pub events: Vec<AdapterEvent>,
    /// Fatal parse failure. When set, `events` is empty and `fallback` is set.
    pub error: Option<AdapterError>,
    pub fallback: Option<TerminalFallback>,
}

impl ParseOutcome {
    pub fn ok(events: Vec<AdapterEvent>) -> Self {
        Self {
            events,
            error: None,
            fallback: None,
        }
    }

    pub fn empty() -> Self {
        Self::ok(Vec::new())
    }

    pub fn failed(error: AdapterError) -> Self {
        let fallback = error.fallback();
        Self {
            events: Vec::new(),
            error: Some(error),
            fallback: Some(fallback),
        }
    }
}

pub trait ProviderAdapter {
    fn provider(&self) -> ProviderKind;

    fn descriptor(&self) -> &CapabilityDescriptor;

    /// Parse one transport line. `sequence` is assigned by the host per stream
    /// and is used for replay/gap handling; it is not a provider sequence.
    fn parse_line(&mut self, stream: &StreamId, sequence: u64, line: &str) -> ParseOutcome;
}
