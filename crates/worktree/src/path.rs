//! Repository-relative paths stored as bytes so non-UTF-8 names survive.

use crate::bytes_to_os_string;
use crate::WorktreeError;
use std::borrow::Cow;
use std::ffi::OsString;
use std::fmt;

/// A path relative to the repository top level, exactly as Git reports it.
///
/// Display is lossy; the raw bytes are retained for comparisons and for
/// passing the path back to Git on platforms that support byte paths.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RepoPath {
    raw: Vec<u8>,
}

impl RepoPath {
    pub fn from_bytes(bytes: impl Into<Vec<u8>>) -> Self {
        Self { raw: bytes.into() }
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.raw
    }

    pub fn is_utf8(&self) -> bool {
        std::str::from_utf8(&self.raw).is_ok()
    }

    /// Lossy UTF-8 for display. Never use this to address the file again.
    pub fn display(&self) -> Cow<'_, str> {
        String::from_utf8_lossy(&self.raw)
    }

    /// Convert to a command-line argument, preserving bytes on Unix.
    pub fn git_arg(&self) -> Result<OsString, WorktreeError> {
        #[cfg(unix)]
        {
            Ok(bytes_to_os_string(&self.raw))
        }
        #[cfg(not(unix))]
        {
            match std::str::from_utf8(&self.raw) {
                Ok(path) => Ok(OsString::from(path)),
                Err(_) => Err(WorktreeError::NonUtf8Path {
                    path: self.display().into_owned(),
                }),
            }
        }
    }
}

impl From<&str> for RepoPath {
    fn from(value: &str) -> Self {
        Self::from_bytes(value.as_bytes().to_vec())
    }
}

impl From<String> for RepoPath {
    fn from(value: String) -> Self {
        Self::from_bytes(value.into_bytes())
    }
}

impl From<Vec<u8>> for RepoPath {
    fn from(value: Vec<u8>) -> Self {
        Self::from_bytes(value)
    }
}

impl AsRef<[u8]> for RepoPath {
    fn as_ref(&self) -> &[u8] {
        &self.raw
    }
}

impl fmt::Display for RepoPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.display())
    }
}

impl fmt::Debug for RepoPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "RepoPath({:?})", self.display())
    }
}
