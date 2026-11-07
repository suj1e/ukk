//! Hard limits applied to every Git invocation.

use std::time::Duration;

/// Default wall-clock budget for a single Git command.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(8);
/// Default cap for each captured stream (stdout and stderr separately).
pub const DEFAULT_MAX_OUTPUT_BYTES: usize = 512 * 1024;
/// Default cap for parsed status entries.
pub const DEFAULT_MAX_STATUS_ENTRIES: usize = 2000;
/// Default number of recent commits returned by [`crate::Worktree::activity`].
pub const DEFAULT_MAX_COMMITS: usize = 20;

/// Bounds for one inspection. Exceeding a bound truncates data; it never
/// blocks the caller indefinitely.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Wall-clock budget for one Git process. A zero budget reports
    /// [`crate::WorktreeError::Timeout`] immediately after spawn.
    pub timeout: Duration,
    /// Maximum bytes kept from stdout *and* from stderr.
    pub max_output_bytes: usize,
    /// Maximum changed-file entries returned by status.
    pub max_status_entries: usize,
    /// Maximum commits returned by activity.
    pub max_commits: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            timeout: DEFAULT_TIMEOUT,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
            max_status_entries: DEFAULT_MAX_STATUS_ENTRIES,
            max_commits: DEFAULT_MAX_COMMITS,
        }
    }
}

impl Limits {
    /// A smaller, defensive profile suitable for rendering inside a pane.
    pub fn compact() -> Self {
        Self {
            timeout: Duration::from_secs(4),
            max_output_bytes: 256 * 1024,
            max_status_entries: 500,
            max_commits: 12,
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn with_max_output_bytes(mut self, max_output_bytes: usize) -> Self {
        self.max_output_bytes = max_output_bytes;
        self
    }
}
