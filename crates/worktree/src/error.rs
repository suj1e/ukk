//! Typed failures with user-facing guidance.
//!
//! No variant carries credentials: only paths and bounded Git stderr.

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorktreeError {
    /// The path is missing, is not a directory, or is not inside a Git
    /// working tree.
    NotARepository {
        path: PathBuf,
        detail: Option<String>,
    },
    /// The OS or Git denied access to the repository.
    PermissionDenied {
        operation: &'static str,
        detail: String,
    },
    /// Git refused the repository because of ownership/safe.directory rules.
    RepositoryNotTrusted { detail: String },
    /// The command exceeded its wall-clock budget and was killed.
    Timeout {
        operation: &'static str,
        limit: Duration,
    },
    /// No `git` executable could be spawned.
    GitMissing { program: PathBuf },
    /// The path cannot be passed to Git as bytes on this platform.
    NonUtf8Path { path: String },
    /// Git ran and exited non-zero for another reason.
    GitFailed {
        operation: &'static str,
        exit_code: Option<i32>,
        stderr: String,
    },
    /// Spawning or waiting for Git failed at the OS level.
    Io {
        operation: &'static str,
        detail: String,
    },
}

impl WorktreeError {
    /// A short, action-oriented hint safe to show next to the raw diagnostic.
    pub fn hint(&self) -> &'static str {
        match self {
            Self::NotARepository { .. } => {
                "Point the Space project root at a directory inside a Git working tree."
            }
            Self::PermissionDenied { .. } => {
                "Check read permissions for the repository and its .git directory."
            }
            Self::RepositoryNotTrusted { .. } => {
                "Git refuses repositories owned by another user; inspect a repository owned by your account."
            }
            Self::Timeout { .. } => {
                "The repository may be very large or on a slow volume. Retry, or open a narrower project root."
            }
            Self::GitMissing { .. } => "Install Git and make sure `git` is available on PATH.",
            Self::NonUtf8Path { .. } => {
                "This path cannot be represented on this platform, so its diff cannot be requested."
            }
            Self::GitFailed { .. } => "See the Git output in the diagnostic for details.",
            Self::Io { .. } => "Check that Git can be executed in this environment.",
        }
    }
}

impl fmt::Display for WorktreeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotARepository { path, detail } => {
                write!(formatter, "`{}` is not a Git working tree", path.display())?;
                if let Some(detail) = detail {
                    write!(formatter, ": {detail}")?;
                }
                Ok(())
            }
            Self::PermissionDenied { operation, detail } => {
                write!(formatter, "Git {operation} was denied permission: {detail}")
            }
            Self::RepositoryNotTrusted { detail } => {
                write!(
                    formatter,
                    "Git refused to inspect this repository: {detail}"
                )
            }
            Self::Timeout { operation, limit } => write!(
                formatter,
                "Git {operation} exceeded its {:?} time budget and was stopped",
                limit
            ),
            Self::GitMissing { program } => write!(
                formatter,
                "The `git` executable was not found at `{}`",
                program.display()
            ),
            Self::NonUtf8Path { path } => write!(
                formatter,
                "The repository path cannot be passed to Git on this platform: {path}"
            ),
            Self::GitFailed {
                operation,
                exit_code,
                stderr,
            } => {
                write!(formatter, "Git {operation} failed")?;
                if let Some(code) = exit_code {
                    write!(formatter, " with exit code {code}")?;
                }
                if !stderr.is_empty() {
                    write!(formatter, ": {stderr}")?;
                }
                Ok(())
            }
            Self::Io { operation, detail } => {
                write!(formatter, "Could not run Git {operation}: {detail}")
            }
        }
    }
}

impl std::error::Error for WorktreeError {}
