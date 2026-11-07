//! Contract tests against real, throwaway Git repositories.
//!
//! Every repository is created under the OS temp directory and removed on
//! drop. No destructive command runs against a user repository: the only
//! mutations are `git init`, `add`, `commit`, `branch`, `checkout` and `merge`
//! inside the temporary fixture.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use ukk_worktree::{ChangeState, DiffScope, Limits, RepoPath, Worktree, WorktreeError};

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempRepo {
    path: PathBuf,
}

impl TempRepo {
    fn new(tag: &str) -> Self {
        let unique = COUNTER.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!(
            "ukk-worktree-{tag}-{}-{unique}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create temp repository directory");
        let repo = Self { path };
        repo.git(&["init", "-q", "-b", "main"]);
        repo.git(&["config", "user.name", "ukk test"]);
        repo.git(&["config", "user.email", "test@ukk.invalid"]);
        repo.git(&["config", "commit.gpgsign", "false"]);
        repo
    }

    fn plain_dir(tag: &str) -> PathBuf {
        let unique = COUNTER.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!(
            "ukk-worktree-{tag}-{}-{unique}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create temp directory");
        path
    }

    fn git(&self, args: &[&str]) -> String {
        self.git_output(args, true)
    }

    fn git_output(&self, args: &[&str], must_succeed: bool) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(&self.path)
            .args(args)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .stdin(Stdio::null())
            .output()
            .expect("run git in temp repository");
        if must_succeed {
            assert!(
                output.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    fn write(&self, relative: &str, contents: impl AsRef<[u8]>) {
        let path = self.path.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent directory");
        }
        fs::write(path, contents).expect("write fixture file");
    }

    fn commit(&self, message: &str) {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", message]);
    }

    fn worktree(&self) -> Worktree {
        Worktree::open(&self.path).expect("open temp repository")
    }
}

impl Drop for TempRepo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[test]
fn clean_repository_reports_no_changes_and_recent_activity() {
    let repo = TempRepo::new("clean");
    repo.write("src/lib.rs", "fn main() {}\n");
    repo.commit("Initial commit");
    repo.write("src/lib.rs", "fn main() { println!(); }\n");
    repo.commit("Second commit");

    let worktree = repo.worktree();
    let status = worktree.status().unwrap();
    assert!(status.is_clean());
    assert_eq!(status.branch.as_deref(), Some("main"));
    assert!(status.head.is_some());
    assert_eq!(status.untracked(), 0);

    let activity = worktree.activity().unwrap();
    assert!(activity.has_commits);
    assert_eq!(activity.commits[0].subject, "Second commit");
    assert_eq!(activity.commits[1].subject, "Initial commit");
    for commit in &activity.commits {
        assert_eq!(commit.id.len(), 40, "commit id has no stray newline");
        assert!(commit.id.chars().all(|c| c.is_ascii_hexdigit()));
    }
    assert!(!activity.truncated);

    assert!(worktree.diff(DiffScope::Head).unwrap().is_empty());
    assert!(worktree.diff(DiffScope::Unstaged).unwrap().is_empty());
}

#[test]
fn reports_modified_staged_and_untracked_files_with_counts() {
    let repo = TempRepo::new("dirty");
    repo.write("tracked.txt", "one\n");
    repo.commit("base");
    repo.write("tracked.txt", "two\n");
    repo.write("untracked.txt", "new\n");
    repo.write("staged.txt", "staged\n");
    repo.git(&["add", "staged.txt"]);

    let status = repo.worktree().status().unwrap();
    assert_eq!(status.entries.len(), 3);
    assert_eq!(status.staged(), 1);
    assert_eq!(status.unstaged(), 1);
    assert_eq!(status.untracked(), 1);
    assert_eq!(status.conflicted(), 0);

    let untracked = status
        .entries
        .iter()
        .find(|entry| entry.is_untracked())
        .unwrap();
    assert_eq!(untracked.path.display(), "untracked.txt");
    assert_eq!(untracked.worktree, ChangeState::Untracked);

    let changed = repo.worktree().changed_files().unwrap();
    assert_eq!(changed.len(), 3);
}

#[test]
fn staged_rename_reports_the_original_path() {
    let repo = TempRepo::new("rename");
    repo.write("old-name.txt", "contents\n");
    repo.commit("base");
    fs::rename(
        repo.path.join("old-name.txt"),
        repo.path.join("new name.txt"),
    )
    .expect("rename fixture file");
    repo.git(&["add", "-A"]);

    let status = repo.worktree().status().unwrap();
    let entry = status
        .entries
        .iter()
        .find(|entry| entry.is_rename())
        .expect("rename entry");
    assert_eq!(entry.path.display(), "new name.txt");
    assert_eq!(
        entry.original_path.as_ref().unwrap().display(),
        "old-name.txt"
    );
    assert!(entry.prefers_staged());
}

#[test]
fn diff_covers_whole_tree_and_a_single_file() {
    let repo = TempRepo::new("diff");
    repo.write("one.txt", "one\n");
    repo.commit("base");
    repo.write("one.txt", "two\n");
    repo.write("extra.txt", "extra\n");

    let worktree = repo.worktree();
    let whole = worktree.diff(DiffScope::Head).unwrap();
    assert!(whole.text.contains("-one"));
    assert!(whole.text.contains("+two"));
    // Untracked files have no index entry, so they appear in Files but not in
    // a tracked diff. The view states this instead of inventing content.
    assert!(!whole.text.contains("extra.txt"));
    assert_eq!(whole.files, 1);
    assert!(!whole.truncated);

    let file = worktree
        .diff(DiffScope::UnstagedFile(RepoPath::from("one.txt")))
        .unwrap();
    assert!(file.text.contains("+two"));
    assert!(!file.text.contains("extra.txt"));

    repo.git(&["add", "one.txt"]);
    let staged = worktree.diff(DiffScope::Staged).unwrap();
    assert!(staged.text.contains("+two"));
    assert!(worktree.diff(DiffScope::Unstaged).unwrap().is_empty());
}

#[test]
fn binary_diff_is_reported_without_panicking() {
    let repo = TempRepo::new("binary");
    repo.write("asset.bin", [0_u8, 1, 2, 3, 0, 255]);
    repo.commit("base");
    repo.write("asset.bin", [0_u8, 9, 8, 7, 0, 254]);

    let diff = repo.worktree().diff(DiffScope::Unstaged).unwrap();
    assert_eq!(diff.files, 1);
    assert_eq!(diff.binary_files, 1);
    assert!(diff.is_binary_only());
    assert!(diff.text.contains("Binary files"));
}

#[test]
fn large_output_and_long_status_lists_are_bounded() {
    let repo = TempRepo::new("large");
    let mut contents = String::new();
    for index in 0..20_000 {
        contents.push_str(&format!("line {index:05} of a large bounded fixture\n"));
    }
    repo.write("large.txt", contents.clone());
    repo.commit("base");
    repo.write("large.txt", contents.replace("line", "LINE"));
    for index in 0..10 {
        repo.write(&format!("untracked-{index}.txt"), "new\n");
    }

    let limits = Limits::default()
        .with_max_output_bytes(4096)
        .with_timeout(Duration::from_secs(30));
    let bounded = Worktree::with_limits(&repo.path, limits).unwrap();
    let diff = bounded.diff(DiffScope::Unstaged).unwrap();
    assert!(diff.truncated, "large diff should be truncated");
    assert!(diff.bytes <= 4096);
    assert!(diff.text.len() <= 4096 + 16, "lossy text stays bounded");

    let limits = Limits {
        max_status_entries: 3,
        ..Limits::default()
    };
    let bounded = Worktree::with_limits(&repo.path, limits).unwrap();
    let status = bounded.status().unwrap();
    assert_eq!(status.entries.len(), 3);
    assert!(status.truncated);
}

#[test]
fn a_hung_git_process_is_killed_and_reaped_at_the_timeout() {
    let repo = TempRepo::new("timeout");
    let limits = Limits::default().with_timeout(Duration::from_millis(2000));
    let hang = PathBuf::from(env!("CARGO_BIN_EXE_ukk-worktree-hang-git"));
    let started = std::time::Instant::now();
    let error = Worktree::open_with(&repo.path, limits, hang).unwrap_err();
    assert!(matches!(error, WorktreeError::Timeout { .. }), "{error:?}");
    assert!(error.to_string().contains("time budget"));
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "timeout returned after {:?}",
        started.elapsed()
    );

    let pid_file = repo.path.join(".ukk-hang-git.pid");
    let pid_text = match fs::read_to_string(&pid_file) {
        Ok(text) => text,
        Err(error) => {
            let entries: Vec<_> = fs::read_dir(&repo.path)
                .map(|entries| {
                    entries
                        .filter_map(Result::ok)
                        .map(|entry| entry.file_name())
                        .collect()
                })
                .unwrap_or_default();
            panic!(
                "missing {} ({error}); directory contains {entries:?}; error was {error:?}",
                pid_file.display()
            );
        }
    };
    let pid: u32 = pid_text.trim().parse().expect("pid is numeric");
    #[cfg(unix)]
    {
        let alive = Command::new("kill")
            .arg("-0")
            .arg(pid.to_string())
            .stderr(Stdio::null())
            .status()
            .expect("run kill -0")
            .success();
        assert!(!alive, "the timed-out child {pid} is still alive");
    }
    #[cfg(not(unix))]
    {
        // Windows process liveness needs a platform API; the pid file plus the
        // bounded return still prove the child ran and the call did not block.
        assert_ne!(pid, 0);
    }
}

#[test]
fn missing_git_binary_is_typed() {
    let repo = TempRepo::new("missing-git");
    let missing = std::env::temp_dir().join(format!("ukk-worktree-no-git-{}", std::process::id()));
    let _ = fs::remove_file(&missing);
    let error = Worktree::open_with_git(&repo.path, &missing).unwrap_err();
    assert!(
        matches!(error, WorktreeError::GitMissing { .. }),
        "{error:?}"
    );
    assert!(error.to_string().contains("was not found"));
}

#[test]
fn plain_directory_is_not_a_repository() {
    let path = TempRepo::plain_dir("not-a-repo");
    let error = Worktree::open(&path).unwrap_err();
    assert!(
        matches!(error, WorktreeError::NotARepository { .. }),
        "{error:?}"
    );
    assert!(error.hint().contains("Git working tree"));
    let _ = fs::remove_dir_all(path);
}

#[test]
fn unborn_repository_has_empty_activity_and_diff() {
    let repo = TempRepo::new("unborn");
    repo.write("untracked.txt", "hello\n");

    let worktree = repo.worktree();
    let status = worktree.status().unwrap();
    assert!(status.head.is_none());
    assert_eq!(status.untracked(), 1);

    let activity = worktree.activity().unwrap();
    assert!(!activity.has_commits);
    assert!(activity.commits.is_empty());

    // HEAD does not exist yet; an empty diff is the truthful answer rather
    // than a Git failure.
    assert!(worktree.diff(DiffScope::Head).unwrap().is_empty());
}

#[test]
fn conflicts_are_reported_separately_from_staged_changes() {
    let repo = TempRepo::new("conflict");
    repo.write("base.txt", "base\n");
    repo.commit("base");

    repo.git(&["checkout", "-q", "-b", "other"]);
    repo.write("base.txt", "other\n");
    repo.commit("other change");
    repo.git(&["checkout", "-q", "main"]);
    repo.write("base.txt", "main\n");
    repo.commit("main change");
    let merge = Command::new("git")
        .arg("-C")
        .arg(&repo.path)
        .args(["merge", "other"])
        .output()
        .expect("run merge");
    assert!(!merge.status.success(), "merge should conflict");

    let status = repo.worktree().status().unwrap();
    assert_eq!(status.conflicted(), 1);
    assert_eq!(status.staged(), 0);
    let entry = status
        .entries
        .iter()
        .find(|entry| entry.is_conflicted())
        .unwrap();
    assert_eq!(entry.path.display(), "base.txt");
    let diff = repo.worktree().diff(DiffScope::Unstaged).unwrap();
    assert!(diff.text.contains("<<<<<<<"));
}

#[test]
fn project_root_scopes_status_and_diff_to_its_subdirectory() {
    let repo = TempRepo::new("scoped");
    repo.write("root.txt", "root\n");
    // A bracket directory also proves the project-root pathspec is literal.
    repo.write("sub [x]/inner.txt", "inner\n");
    repo.commit("base");
    repo.write("root.txt", "root changed\n");
    repo.write("sub [x]/inner.txt", "inner changed\n");

    let sub = repo.path.join("sub [x]");
    let worktree = Worktree::open(&sub).unwrap();
    assert_eq!(worktree.project_root(), sub.as_path());
    let status = worktree.status().unwrap();
    assert_eq!(status.entries.len(), 1);
    // Paths stay repository-relative so per-file diff requests are valid.
    assert_eq!(status.entries[0].path.display(), "sub [x]/inner.txt");

    let diff = worktree.diff(DiffScope::Head).unwrap();
    assert!(diff.text.contains("inner changed"));
    assert!(!diff.text.contains("root changed"));
}

#[cfg(target_os = "linux")]
#[test]
fn non_utf8_paths_round_trip_through_status_and_diff() {
    use std::os::unix::ffi::OsStringExt;
    let repo = TempRepo::new("non-utf8");
    repo.write("seed.txt", "seed\n");
    repo.commit("base");

    let name = b"caf\xe9.txt".to_vec();
    let path = repo.path.join(std::ffi::OsString::from_vec(name.clone()));
    fs::write(&path, "invalid utf-8 name\n").expect("write non-utf8 fixture");
    repo.commit("track non-utf8 name");
    fs::write(&path, "invalid utf-8 name changed\n").expect("modify non-utf8 fixture");

    let worktree = repo.worktree();
    let status = worktree.status().unwrap();
    let entry = status
        .entries
        .iter()
        .find(|entry| entry.path.as_bytes() == name.as_slice())
        .expect("non-utf8 entry");
    assert!(!entry.path.is_utf8());

    let diff = worktree
        .diff(DiffScope::UnstagedFile(entry.path.clone()))
        .unwrap();
    assert!(diff.text.contains("invalid utf-8 name changed"));
    // Display never panics even though the raw bytes are not UTF-8.
    assert!(entry.path.display().contains('\u{fffd}'));
}

#[cfg(unix)]
#[test]
fn unreadable_project_root_is_a_permission_error() {
    use std::os::unix::fs::PermissionsExt;

    let repo = TempRepo::new("permission");
    repo.write("file.txt", "one\n");
    repo.commit("base");

    // A root-owned CI process can read through mode 000; skip truthfully.
    let permissions = fs::metadata(&repo.path).unwrap().permissions();
    fs::set_permissions(&repo.path, fs::Permissions::from_mode(0o000))
        .expect("restrict fixture directory");
    let result = Worktree::open(&repo.path);
    fs::set_permissions(&repo.path, permissions).expect("restore fixture permissions");

    match result {
        Err(WorktreeError::PermissionDenied { .. }) => {}
        Err(error) => panic!("expected a permission error, got {error:?}"),
        Ok(_) => eprintln!("skipping: this user can read mode-000 directories"),
    }
}

#[test]
fn corrupt_head_is_an_error_not_an_empty_history_or_diff() {
    let repo = TempRepo::new("corrupt-head");
    repo.write("file.txt", "one\n");
    repo.commit("base");
    let worktree = repo.worktree();

    fs::write(
        repo.path
            .join(".git")
            .join("refs")
            .join("heads")
            .join("main"),
        b"garbage\n",
    )
    .expect("corrupt the branch ref");

    let activity = worktree.activity().unwrap_err();
    assert!(
        matches!(activity, WorktreeError::GitFailed { .. }),
        "{activity:?}"
    );
    let diff = worktree.diff(DiffScope::Head).unwrap_err();
    assert!(matches!(diff, WorktreeError::GitFailed { .. }), "{diff:?}");

    // A ref that points at a missing object is also corrupt, not unborn.
    fs::write(
        repo.path
            .join(".git")
            .join("refs")
            .join("heads")
            .join("main"),
        b"0123456789abcdef0123456789abcdef01234567\n",
    )
    .expect("point the branch ref at a missing object");
    let activity = worktree.activity().unwrap_err();
    assert!(
        matches!(activity, WorktreeError::GitFailed { .. }),
        "{activity:?}"
    );
    let diff = worktree.diff(DiffScope::Head).unwrap_err();
    assert!(matches!(diff, WorktreeError::GitFailed { .. }), "{diff:?}");
}

#[test]
fn a_byte_capped_status_is_reported_as_truncated() {
    let repo = TempRepo::new("status-cap");
    repo.write("seed.txt", "seed\n");
    repo.commit("base");
    for index in 0..40 {
        repo.write(
            &format!("untracked-with-a-fairly-long-name-{index:02}.txt"),
            "new\n",
        );
    }

    let limits = Limits {
        max_output_bytes: 300,
        ..Limits::default()
    };
    let status = Worktree::with_limits(&repo.path, limits)
        .unwrap()
        .status()
        .unwrap();
    assert!(
        status.truncated,
        "byte-capped status must report truncation"
    );
    assert!(!status.is_clean(), "truncated status must not claim clean");
    assert!(status.entries.len() < 40);
    assert!(status
        .entries
        .iter()
        .all(|entry| entry.path.display().ends_with(".txt")));
}

#[cfg(unix)]
#[test]
fn pathspec_metacharacters_select_only_the_literal_file() {
    let repo = TempRepo::new("pathspec");
    let files = [
        ("q?.txt", "question-mark"),
        ("q2.txt", "plain-two"),
        ("*star.txt", "star-file"),
        ("[bracket].txt", "bracket-file"),
        (":colon.txt", "colon-file"),
        ("dir [b]/inner.txt", "bracket-directory"),
    ];
    for (path, contents) in files {
        repo.write(path, format!("{contents}\n"));
    }
    repo.commit("base");
    for (path, contents) in files {
        repo.write(path, format!("{contents} changed\n"));
    }

    let worktree = repo.worktree();
    for (path, contents) in files {
        let diff = worktree
            .diff(DiffScope::UnstagedFile(RepoPath::from(path)))
            .unwrap();
        assert!(
            diff.text.contains(&format!("{contents} changed")),
            "diff for {path} missed its own change: {}",
            diff.text
        );
        assert_eq!(diff.files, 1, "diff for {path} matched extra files");
        for (other, other_contents) in files {
            if other != path {
                assert!(
                    !diff.text.contains(&format!("{other_contents} changed")),
                    "diff for {path} contained {other}"
                );
            }
        }
    }
}
