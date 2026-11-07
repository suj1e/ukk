//! Bounded unified diff output.

use crate::RepoPath;

/// Which comparison the user asked for. Never a write operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiffScope {
    /// HEAD versus the working tree (staged and unstaged combined).
    Head,
    /// HEAD versus the index.
    Staged,
    /// Index versus the working tree.
    Unstaged,
    /// One file, HEAD/index versus the working tree pair.
    StagedFile(RepoPath),
    /// One file, index versus the working tree.
    UnstagedFile(RepoPath),
}

impl DiffScope {
    pub fn is_file(&self) -> bool {
        matches!(self, Self::StagedFile(_) | Self::UnstagedFile(_))
    }

    pub fn label(&self) -> String {
        match self {
            Self::Head => "all changes since HEAD".into(),
            Self::Staged => "staged changes".into(),
            Self::Unstaged => "unstaged changes".into(),
            Self::StagedFile(path) => format!("staged · {}", path.display()),
            Self::UnstagedFile(path) => format!("unstaged · {}", path.display()),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Diff {
    /// Unified diff text. Lossy UTF-8; `bytes` is the raw captured size.
    pub text: String,
    pub bytes: usize,
    /// Output exceeded [`crate::Limits::max_output_bytes`].
    pub truncated: bool,
    /// Number of `diff --git` sections in the captured text.
    pub files: usize,
    /// Sections Git reported as binary rather than textual.
    pub binary_files: usize,
}

impl Diff {
    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty()
    }

    /// True when every captured section is binary, so the UI can explain an
    /// otherwise empty-looking diff.
    pub fn is_binary_only(&self) -> bool {
        self.binary_files > 0 && self.binary_files == self.files
    }

    pub(crate) fn from_output(raw: &[u8], truncated: bool, limit: usize) -> Self {
        let text = String::from_utf8_lossy(raw).into_owned();
        let files = text
            .lines()
            .filter(|line| line.starts_with("diff --git "))
            .count();
        let binary_files = text
            .lines()
            .filter(|line| {
                line.starts_with("Binary files ") || line.starts_with("GIT binary patch")
            })
            .count();
        Self {
            text,
            bytes: raw.len().min(limit),
            truncated,
            files,
            binary_files,
        }
    }
}
