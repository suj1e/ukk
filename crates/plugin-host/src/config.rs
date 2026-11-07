//! Supervisor tunables. Defaults are deliberately small; tests shrink them.
use std::time::Duration;

use ukk_plugin_api::limits;

use crate::helper::HelperLimits;

#[derive(Debug, Clone)]
pub struct HostConfig {
    pub request_timeout: Duration,
    pub shutdown_grace: Duration,
    pub max_frame_bytes: usize,
    pub event_queue_capacity: usize,
    pub max_plugins: usize,
}

impl Default for HostConfig {
    fn default() -> Self {
        Self {
            request_timeout: Duration::from_secs(2),
            shutdown_grace: Duration::from_millis(750),
            max_frame_bytes: limits::MAX_FRAME_BYTES,
            event_queue_capacity: limits::MAX_EVENT_QUEUE,
            max_plugins: 64,
        }
    }
}

impl HostConfig {
    pub(crate) fn helper_limits(&self) -> HelperLimits {
        HelperLimits {
            request_timeout: self.request_timeout,
            shutdown_grace: self.shutdown_grace,
            max_frame_bytes: self.max_frame_bytes,
            event_queue_capacity: self.event_queue_capacity,
        }
    }
}
