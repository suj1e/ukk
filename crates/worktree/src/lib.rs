//! Bounded, read-only Git inspection for a ukk project root.
//!
//! This crate is deliberately small and self-contained: it shells out to the
//! user's `git` executable with read-only commands, never mutates a repository,
//! never fetches from a remote and never blocks an interactive UI. Every
//! operation has a hard timeout and bounded output, and non-UTF-8 paths are
//! handled as bytes rather than silently mangled.
//!
//! It is not a general Git library. It answers three product questions:
//!
//! * which files changed (`Status` / `changed_files`),
//! * what the unified diff is (`Diff` / `diff`),
//! * what happened recently (`Activity`).
//!
//! Safety properties enforced here:
//!
//! * only `rev-parse`, `status`, `diff` and `log` are ever executed;
//! * no `-C`/`--git-dir`/environment escape supplied by the caller reaches Git;
//! * repository-configured `core.fsmonitor` hooks, external diff programs and
//!   textconv filters are disabled (`--no-ext-diff`, `--no-textconv`);
//! * stdin is closed, terminal prompts are disabled and no pager is started;
//! * output is capped by [`Limits`] and the child is killed on timeout.
//!
//! Residual risk: Git still honours repository-configured *clean filters* for
//! files whose `.gitattributes` declares a `filter=` driver. A hostile
//! repository the user explicitly opens can therefore execute that filter
//! during `status`/`diff`. Do not treat this crate as a sandbox for untrusted
//! repositories.

mod activity;
mod diff;
mod error;
mod limits;
mod path;
mod runner;
mod status;

pub use activity::{Activity, Commit};
pub use diff::{Diff, DiffScope};
pub use error::WorktreeError;
pub use limits::Limits;
pub use path::RepoPath;
pub use status::{ChangeState, Status, StatusEntry};

/// A read-only view of one project root inside a Git working tree.
///
/// The requested project root is kept for display, but Git commands run from
/// the repository top level and are scoped with a pathspec so a Space that
/// points at a subdirectory never reports unrelated sibling changes.
#[derive(Clone, Debug)]
pub struct Worktree {
    project_root: std::path::PathBuf,
    repository_root: std::path::PathBuf,
    /// Repository-relative directory prefix of the project root, without a
    /// trailing slash. Empty means the whole working tree.
    prefix: Vec<u8>,
    git: std::path::PathBuf,
    limits: Limits,
}

impl Worktree {
    /// Open a project root with default limits and the `git` on `PATH`.
    pub fn open(root: impl Into<std::path::PathBuf>) -> Result<Self, WorktreeError> {
        Self::open_with(root, Limits::default(), std::path::PathBuf::from("git"))
    }

    /// Open a project root with explicit limits.
    pub fn with_limits(
        root: impl Into<std::path::PathBuf>,
        limits: Limits,
    ) -> Result<Self, WorktreeError> {
        Self::open_with(root, limits, std::path::PathBuf::from("git"))
    }

    /// Open a project root using a specific `git` executable.
    ///
    /// This exists so tests can exercise the missing-binary path; product code
    /// should use [`Worktree::open`].
    pub fn open_with_git(
        root: impl Into<std::path::PathBuf>,
        git: impl Into<std::path::PathBuf>,
    ) -> Result<Self, WorktreeError> {
        Self::open_with(root, Limits::default(), git.into())
    }

    /// Fully explicit constructor. The Git binary is a test/diagnostic seam.
    pub fn open_with(
        root: impl Into<std::path::PathBuf>,
        limits: Limits,
        git: std::path::PathBuf,
    ) -> Result<Self, WorktreeError> {
        let project_root = root.into();
        let probe = Self {
            project_root: project_root.clone(),
            repository_root: project_root.clone(),
            prefix: Vec::new(),
            git,
            limits,
        };
        let output = probe.run_git(
            "rev-parse",
            &[
                "--is-inside-work-tree".into(),
                "--show-toplevel".into(),
                "--show-prefix".into(),
            ],
        )?;
        if output.exit_code != Some(0) {
            return Err(probe.classify_failure("rev-parse", &output));
        }
        let lines = split_lines(&output.stdout);
        let inside = lines.first().copied().unwrap_or_default();
        if inside != b"true" {
            return Err(WorktreeError::NotARepository {
                path: project_root,
                detail: Some("path is not inside a Git working tree".into()),
            });
        }
        let Some(toplevel) = lines.get(1).filter(|line| !line.is_empty()) else {
            return Err(WorktreeError::NotARepository {
                path: project_root,
                detail: Some("Git did not report a working tree root".into()),
            });
        };
        let repository_root = std::path::PathBuf::from(bytes_to_os_string(toplevel));
        let mut prefix = lines.get(2).copied().unwrap_or_default().to_vec();
        while prefix.last() == Some(&b'/') {
            prefix.pop();
        }
        Ok(Self {
            project_root,
            repository_root,
            prefix,
            git: probe.git,
            limits,
        })
    }

    /// The path the caller asked to inspect.
    pub fn project_root(&self) -> &std::path::Path {
        &self.project_root
    }

    /// The working-tree root Git reports for that path.
    pub fn repository_root(&self) -> &std::path::Path {
        &self.repository_root
    }

    pub fn limits(&self) -> &Limits {
        &self.limits
    }

    /// Branch, HEAD and changed files. Never writes to the repository.
    pub fn status(&self) -> Result<Status, WorktreeError> {
        let mut args: Vec<std::ffi::OsString> = vec![
            "--porcelain=v2".into(),
            "--branch".into(),
            "-z".into(),
            "--untracked-files=all".into(),
            "--ignore-submodules=dirty".into(),
        ];
        args.extend(self.pathspec_args());
        let output = self.run_git("status", &args)?;
        if output.exit_code != Some(0) {
            return Err(self.classify_failure("status", &output));
        }
        Ok(status::parse(
            &output.stdout,
            self.limits.max_status_entries,
        ))
    }

    /// Changed files only; identical parsing to [`Worktree::status`].
    pub fn changed_files(&self) -> Result<Vec<StatusEntry>, WorktreeError> {
        Ok(self.status()?.entries)
    }

    /// Unified diff for the requested scope, bounded by
    /// [`Limits::max_output_bytes`].
    pub fn diff(&self, scope: DiffScope) -> Result<Diff, WorktreeError> {
        let mut args: Vec<std::ffi::OsString> = vec![
            "--no-ext-diff".into(),
            "--no-textconv".into(),
            "--no-color".into(),
            "--unified=3".into(),
        ];
        match &scope {
            DiffScope::Head => args.push("HEAD".into()),
            DiffScope::Staged => args.push("--cached".into()),
            DiffScope::Unstaged => {}
            DiffScope::StagedFile(path) => {
                args.push("--cached".into());
                args.push("--".into());
                args.push(path.git_arg()?);
                return self.finish_diff("diff", args);
            }
            DiffScope::UnstagedFile(path) => {
                args.push("--".into());
                args.push(path.git_arg()?);
                return self.finish_diff("diff", args);
            }
        }
        args.extend(self.pathspec_args());
        let result = self.finish_diff("diff", args);
        match result {
            Err(WorktreeError::GitFailed { stderr, .. })
                if matches!(scope, DiffScope::Head) && is_unborn_head(&stderr) =>
            {
                // An unborn HEAD has nothing tracked to compare against, so an
                // empty working-tree diff is the truthful answer.
                Ok(Diff::default())
            }
            other => other,
        }
    }

    fn finish_diff(
        &self,
        operation: &'static str,
        args: Vec<std::ffi::OsString>,
    ) -> Result<Diff, WorktreeError> {
        let output = self.run_git(operation, &args)?;
        if output.exit_code != Some(0) {
            return Err(self.classify_failure(operation, &output));
        }
        Ok(Diff::from_output(
            &output.stdout,
            output.stdout_truncated,
            self.limits.max_output_bytes,
        ))
    }

    /// Recent commits scoped to this project root.
    pub fn activity(&self) -> Result<Activity, WorktreeError> {
        // `log` fails on an unborn HEAD; detect that explicitly so an empty
        // repository is an empty activity list rather than an error.
        let probe = self.run_git(
            "rev-parse",
            &["--verify".into(), "--quiet".into(), "HEAD^{commit}".into()],
        )?;
        if probe.exit_code != Some(0) {
            // An unborn HEAD exits non-zero without any stderr. Any other
            // failure (corruption, invalid HEAD, permission) is reported
            // instead of being misread as an empty history.
            let stderr = String::from_utf8_lossy(&probe.stderr);
            if probe.stderr_truncated || !stderr.trim().is_empty() {
                let lower = stderr.to_ascii_lowercase();
                let unborn = lower.contains("needed a single revision")
                    || lower.contains("needs a single revision")
                    || lower.contains("unknown revision")
                    || lower.contains("bad revision")
                    || lower.contains("ambiguous argument")
                    || lower.contains("does not have any commits yet");
                if !unborn {
                    return Err(self.classify_failure("rev-parse", &probe));
                }
            }
            return Ok(Activity::empty());
        }
        let requested = self.limits.max_commits.saturating_add(1);
        let mut args: Vec<std::ffi::OsString> = vec![
            format!("-n{requested}").into(),
            "--no-decorate".into(),
            "--no-color".into(),
            "--format=%H%x1f%h%x1f%an%x1f%aI%x1f%s%x00".into(),
        ];
        args.extend(self.pathspec_args());
        let output = self.run_git("log", &args)?;
        if output.exit_code != Some(0) {
            return Err(self.classify_failure("log", &output));
        }
        Ok(Activity::parse(
            &output.stdout,
            self.limits.max_commits,
            output.stdout_truncated,
        ))
    }

    fn pathspec_args(&self) -> Vec<std::ffi::OsString> {
        vec!["--".into(), self.pathspec()]
    }

    fn pathspec(&self) -> std::ffi::OsString {
        if self.prefix.is_empty() {
            ".".into()
        } else {
            bytes_to_os_string(&self.prefix)
        }
    }
}

fn split_lines(raw: &[u8]) -> Vec<&[u8]> {
    raw.split(|byte| *byte == b'\n')
        .map(|line| line.strip_suffix(b"\r").unwrap_or(line))
        .collect()
}

fn is_unborn_head(stderr: &str) -> bool {
    let stderr = stderr.to_ascii_lowercase();
    stderr.contains("bad revision 'head'")
        || stderr.contains("ambiguous argument 'head'")
        || stderr.contains("does not have any commits yet")
}

/// Convert raw repository path bytes into an `OsString` without assuming
/// UTF-8. Windows cannot represent arbitrary bytes, but Git for Windows emits
/// UTF-8 file names, so lossy conversion is only a last resort there.
pub(crate) fn bytes_to_os_string(raw: &[u8]) -> std::ffi::OsString {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        std::ffi::OsString::from_vec(raw.to_vec())
    }
    #[cfg(not(unix))]
    {
        std::ffi::OsString::from(String::from_utf8_lossy(raw).into_owned())
    }
}
