//! Bounded subprocess execution for the read-only Git commands.

use crate::error::WorktreeError;
use crate::limits::Limits;
use crate::Worktree;
use std::ffi::OsString;
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const POLL_INTERVAL: Duration = Duration::from_millis(5);
const READER_GRACE: Duration = Duration::from_secs(2);
const STDERR_IN_MESSAGE: usize = 2_000;

pub(crate) struct RunOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub exit_code: Option<i32>,
}

impl Worktree {
    pub(crate) fn run_git(
        &self,
        operation: &'static str,
        args: &[OsString],
    ) -> Result<RunOutput, WorktreeError> {
        let mut argv = Vec::with_capacity(args.len() + 1);
        argv.push(OsString::from(operation));
        argv.extend_from_slice(args);
        run(
            &self.git,
            &self.repository_root,
            &self.limits,
            operation,
            &argv,
        )
    }

    /// Map a non-zero Git exit into the most specific typed error we can
    /// justify from its stderr. Unknown failures stay `GitFailed`.
    pub(crate) fn classify_failure(
        &self,
        operation: &'static str,
        output: &RunOutput,
    ) -> WorktreeError {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let lower = stderr.to_ascii_lowercase();
        let mut detail = trim_stderr(&stderr);
        if output.stderr_truncated && !detail.ends_with('…') {
            detail.push('…');
        }
        if lower.contains("not a git repository") {
            return WorktreeError::NotARepository {
                path: self.project_root.clone(),
                detail: Some(detail),
            };
        }
        if lower.contains("dubious ownership") || lower.contains("not owned by current user") {
            return WorktreeError::RepositoryNotTrusted { detail };
        }
        if lower.contains("permission denied")
            || lower.contains("operation not permitted")
            || lower.contains("access is denied")
        {
            return WorktreeError::PermissionDenied { operation, detail };
        }
        if lower.contains("cannot change to")
            || lower.contains("no such file or directory")
            || lower.contains("not a directory")
            || lower.contains("cannot find the file specified")
            || lower.contains("cannot find the path specified")
        {
            return WorktreeError::NotARepository {
                path: self.project_root.clone(),
                detail: Some(detail),
            };
        }
        WorktreeError::GitFailed {
            operation,
            exit_code: output.exit_code,
            stderr: detail,
        }
    }
}

fn trim_stderr(stderr: &str) -> String {
    let trimmed = stderr.trim();
    if trimmed.chars().count() <= STDERR_IN_MESSAGE {
        trimmed.to_owned()
    } else {
        let mut text: String = trimmed.chars().take(STDERR_IN_MESSAGE).collect();
        text.push('…');
        text
    }
}

fn run(
    program: &std::path::Path,
    cwd: &std::path::Path,
    limits: &Limits,
    operation: &'static str,
    args: &[OsString],
) -> Result<RunOutput, WorktreeError> {
    let mut command = Command::new(program);
    command.arg("-C").arg(cwd);
    command.args(["--no-pager", "--no-optional-locks"]);
    // Disable repository-configured behaviour that would spawn helpers or
    // change quoting of the machine-readable output.
    for setting in [
        "core.fsmonitor=false",
        "core.quotepath=false",
        "core.pager=cat",
        "diff.external=",
        "color.ui=false",
    ] {
        command.arg("-c").arg(setting);
    }
    command.args(args);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for variable in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_NAMESPACE",
        "GIT_PREFIX",
        "GIT_EXTERNAL_DIFF",
        "GIT_DIFF_OPTS",
        "GIT_CONFIG_COUNT",
    ] {
        command.env_remove(variable);
    }
    command
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_PAGER", "cat")
        .env("PAGER", "cat")
        // Error classification is based on Git's English prose; keep the
        // child's locale stable across user environments.
        .env("LC_ALL", "C")
        .env("LANG", "C");

    let mut child = command.spawn().map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => WorktreeError::GitMissing {
            program: program.to_path_buf(),
        },
        std::io::ErrorKind::PermissionDenied => WorktreeError::PermissionDenied {
            operation,
            detail: error.to_string(),
        },
        _ => WorktreeError::Io {
            operation,
            detail: error.to_string(),
        },
    })?;

    let cap = limits.max_output_bytes;
    let (out_tx, out_rx) = mpsc::channel();
    let (err_tx, err_rx) = mpsc::channel();
    if let Some(stdout) = child.stdout.take() {
        std::thread::spawn(move || {
            let _ = out_tx.send(read_bounded(stdout, cap));
        });
    }
    if let Some(stderr) = child.stderr.take() {
        std::thread::spawn(move || {
            let _ = err_tx.send(read_bounded(stderr, cap));
        });
    }

    // A zero budget means "no time allowed"; stop the child before waiting.
    let status = if limits.timeout.is_zero() {
        let _ = child.kill();
        let _ = child.wait();
        return Err(WorktreeError::Timeout {
            operation,
            limit: limits.timeout,
        });
    } else {
        let deadline = Instant::now().checked_add(limits.timeout);
        loop {
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                let _ = child.kill();
                let _ = child.wait();
                return Err(WorktreeError::Timeout {
                    operation,
                    limit: limits.timeout,
                });
            }
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => std::thread::sleep(POLL_INTERVAL),
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(WorktreeError::Io {
                        operation,
                        detail: error.to_string(),
                    });
                }
            }
        }
    };

    let (stdout, stdout_truncated) = out_rx
        .recv_timeout(READER_GRACE)
        .unwrap_or_else(|_| (Vec::new(), true));
    let (stderr, stderr_truncated) = err_rx
        .recv_timeout(READER_GRACE)
        .unwrap_or_else(|_| (Vec::new(), true));

    Ok(RunOutput {
        stdout,
        stderr,
        stdout_truncated,
        stderr_truncated,
        exit_code: status.code(),
    })
}

/// Read at most `cap` bytes, then keep draining so the child never blocks on
/// a full pipe. The byte after the cap is what marks the output truncated.
fn read_bounded(mut stream: impl Read, cap: usize) -> (Vec<u8>, bool) {
    let mut kept = Vec::new();
    let mut truncated = false;
    let mut buffer = [0_u8; 8192];
    loop {
        match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                let room = cap.saturating_sub(kept.len());
                if room == 0 {
                    truncated = true;
                    continue;
                }
                if read <= room {
                    kept.extend_from_slice(&buffer[..read]);
                } else {
                    kept.extend_from_slice(&buffer[..room]);
                    truncated = true;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    (kept, truncated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn probe() -> Worktree {
        Worktree {
            project_root: PathBuf::from("/tmp/project"),
            repository_root: PathBuf::from("/tmp/project"),
            prefix: Vec::new(),
            git: PathBuf::from("git"),
            limits: Limits::default(),
        }
    }

    fn failure(stderr: &[u8]) -> RunOutput {
        RunOutput {
            stdout: Vec::new(),
            stderr: stderr.to_vec(),
            stdout_truncated: false,
            stderr_truncated: false,
            exit_code: Some(128),
        }
    }

    #[test]
    fn known_git_failures_map_to_typed_errors() {
        let worktree = probe();
        let untrusted = failure(b"fatal: detected dubious ownership in repository at '/x'\n");
        assert!(matches!(
            worktree.classify_failure("status", &untrusted),
            WorktreeError::RepositoryNotTrusted { .. }
        ));
        let denied = failure(b"fatal: cannot open '.git/index': Permission denied\n");
        assert!(matches!(
            worktree.classify_failure("status", &denied),
            WorktreeError::PermissionDenied { .. }
        ));
        let not_repo =
            failure(b"fatal: not a git repository (or any of the parent directories): .git\n");
        assert!(matches!(
            worktree.classify_failure("status", &not_repo),
            WorktreeError::NotARepository { .. }
        ));
        let other = failure(b"fatal: unexpected failure\n");
        match worktree.classify_failure("status", &other) {
            WorktreeError::GitFailed { stderr, .. } => {
                assert!(stderr.contains("unexpected failure"));
            }
            error => panic!("expected GitFailed, got {error:?}"),
        }
    }

    #[test]
    fn a_missing_project_directory_is_not_a_repository() {
        let worktree = probe();
        for stderr in [
            b"fatal: cannot change to '/missing': No such file or directory\n".as_slice(),
            b"fatal: cannot change to 'C:\\missing': The system cannot find the file specified.\n"
                .as_slice(),
            b"fatal: cannot chdir: Not a directory\n".as_slice(),
        ] {
            assert!(matches!(
                worktree.classify_failure("rev-parse", &failure(stderr)),
                WorktreeError::NotARepository { .. }
            ));
        }
    }

    #[test]
    fn classification_ignores_case_of_the_os_message() {
        let worktree = probe();
        let denied = failure(b"Access is denied.\n");
        assert!(matches!(
            worktree.classify_failure("status", &denied),
            WorktreeError::PermissionDenied { .. }
        ));
    }

    #[test]
    fn bounded_reader_keeps_the_cap_and_marks_truncation() {
        let data = b"0123456789".to_vec();
        let (kept, truncated) = read_bounded(std::io::Cursor::new(data.clone()), 4);
        assert_eq!(kept, b"0123");
        assert!(truncated);
        let (kept, truncated) = read_bounded(std::io::Cursor::new(data), 16);
        assert_eq!(kept, b"0123456789");
        assert!(!truncated);
    }
}
