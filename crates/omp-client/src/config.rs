//! Spawn configuration for one omp RPC child.

use std::path::PathBuf;
use std::time::Duration;

/// Hard ceiling for an inbound frame even if the child advertises more. The
/// documented v1 limit is 1 MiB; a hostile or broken child must not be able to
/// make the Host allocate arbitrarily.
pub(crate) const ABSOLUTE_MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;
/// Smallest frame size accepted from `ready`. A smaller advertisement cannot
/// carry a normal protocol frame.
pub(crate) const MIN_FRAME_BYTES: usize = 4 * 1024;
/// The Host never sends protocol v2 chunked input, so outbound frames are
/// bounded by the same v1 line limit.
pub(crate) const DEFAULT_MAX_FRAME_BYTES: usize = 1024 * 1024;
/// Bound for the redacted stderr tail kept in memory.
pub(crate) const DEFAULT_MAX_STDERR_LINES: usize = 64;
/// Maximum bytes of one stderr line kept after redaction.
pub(crate) const MAX_STDERR_LINE_BYTES: usize = 4096;

/// Everything needed to start one omp RPC child.
#[derive(Clone, Debug)]
pub struct OmpConfig {
    /// Executable to spawn. The default is `omp` on `PATH`.
    pub binary: PathBuf,
    /// Arguments. Defaults to `--mode rpc --no-session`.
    pub args: Vec<String>,
    /// Working directory for the session. `None` inherits the Host's cwd; the
    /// Host always sets the Session's project root explicitly.
    pub cwd: Option<PathBuf>,
    /// How long `spawn` waits for the `ready` frame before killing the child.
    pub startup_timeout: Duration,
    /// How long a command acknowledgement may take. A timeout removes the
    /// correlation entry and returns a typed error; it does not kill a
    /// healthy child.
    pub request_timeout: Duration,
    /// Grace period between closing stdin and sending a kill signal during
    /// shutdown.
    pub shutdown_grace: Duration,
    /// Inbound frame cap requested by the caller. The child's advertised
    /// `maxFrameBytes` is authoritative up to [`ABSOLUTE_MAX_FRAME_BYTES`].
    pub max_frame_bytes: usize,
    /// Number of redacted stderr lines kept for diagnostics.
    pub max_stderr_lines: usize,
}

impl Default for OmpConfig {
    fn default() -> Self {
        Self {
            binary: PathBuf::from("omp"),
            args: vec![
                "--mode".to_owned(),
                "rpc".to_owned(),
                "--no-session".to_owned(),
            ],
            cwd: None,
            startup_timeout: Duration::from_secs(20),
            request_timeout: Duration::from_secs(15),
            shutdown_grace: Duration::from_secs(3),
            max_frame_bytes: DEFAULT_MAX_FRAME_BYTES,
            max_stderr_lines: DEFAULT_MAX_STDERR_LINES,
        }
    }
}

impl OmpConfig {
    pub fn with_binary(mut self, binary: impl Into<PathBuf>) -> Self {
        self.binary = binary.into();
        self
    }

    pub fn with_args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args = args.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    pub fn with_startup_timeout(mut self, timeout: Duration) -> Self {
        self.startup_timeout = timeout;
        self
    }

    pub fn with_request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
        self
    }

    pub fn with_max_frame_bytes(mut self, bytes: usize) -> Self {
        self.max_frame_bytes = bytes.max(MIN_FRAME_BYTES);
        self
    }
}
