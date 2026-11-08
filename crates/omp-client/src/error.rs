//! Typed transport errors. Every message is safe to store or display: process
//! environment, credentials and raw stderr are never interpolated.

use std::fmt;
use std::io;
use std::time::Duration;

#[derive(Debug)]
pub enum OmpError {
    /// The child could not be started.
    Spawn {
        binary: String,
        message: String,
    },
    Io(io::Error),
    /// stdout or stdin failed after startup.
    Transport(String),
    /// A frame or handshake violated the documented protocol.
    Protocol(String),
    /// No `ready` frame arrived within the configured startup deadline.
    StartupTimeout {
        waited: Duration,
        stderr_tail: Vec<String>,
    },
    /// The command was written but no correlated response arrived in time.
    RequestTimeout {
        command: String,
        waited: Duration,
    },
    /// The child answered `success: false`.
    CommandRejected {
        command: String,
        message: String,
        code: Option<String>,
    },
    /// The child is not running (exited, or never started).
    NotRunning(String),
    /// Caller input cannot be represented within the transport limits.
    InvalidArgument(String),
}

impl OmpError {
    pub fn is_timeout(&self) -> bool {
        matches!(
            self,
            Self::StartupTimeout { .. } | Self::RequestTimeout { .. }
        )
    }

    /// Stable machine-readable kind for RPC error payloads.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Spawn { .. } => "spawn",
            Self::Io(_) => "io",
            Self::Transport(_) => "transport",
            Self::Protocol(_) => "protocol",
            Self::StartupTimeout { .. } => "startup_timeout",
            Self::RequestTimeout { .. } => "request_timeout",
            Self::CommandRejected { .. } => "command_rejected",
            Self::NotRunning(_) => "not_running",
            Self::InvalidArgument(_) => "invalid_argument",
        }
    }
}

impl fmt::Display for OmpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn { binary, message } => {
                write!(f, "could not start {binary}: {message}")
            }
            Self::Io(error) => write!(f, "omp I/O error: {error}"),
            Self::Transport(message) => write!(f, "omp transport error: {message}"),
            Self::Protocol(message) => write!(f, "omp protocol error: {message}"),
            Self::StartupTimeout {
                waited,
                stderr_tail,
            } => {
                write!(f, "omp did not become ready within {waited:?}")?;
                if !stderr_tail.is_empty() {
                    write!(f, " (stderr: {})", stderr_tail.join(" | "))?;
                }
                Ok(())
            }
            Self::RequestTimeout { command, waited } => {
                write!(f, "omp did not acknowledge `{command}` within {waited:?}")
            }
            Self::CommandRejected {
                command,
                message,
                code,
            } => {
                write!(f, "omp rejected `{command}`: {message}")?;
                if let Some(code) = code {
                    write!(f, " ({code})")?;
                }
                Ok(())
            }
            Self::NotRunning(message) => write!(f, "omp is not running: {message}"),
            Self::InvalidArgument(message) => write!(f, "invalid omp request: {message}"),
        }
    }
}

impl std::error::Error for OmpError {}

impl From<io::Error> for OmpError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}
