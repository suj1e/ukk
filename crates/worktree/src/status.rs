//! Porcelain v2 (`-z`) status parsing.
//!
//! Records are parsed from bytes; only display converts to lossy UTF-8.

use crate::RepoPath;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeState {
    Clean,
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    TypeChanged,
    Conflicted,
    Untracked,
    Unknown,
}

impl ChangeState {
    pub fn letter(self) -> &'static str {
        match self {
            Self::Clean => "·",
            Self::Added => "A",
            Self::Modified => "M",
            Self::Deleted => "D",
            Self::Renamed => "R",
            Self::Copied => "C",
            Self::TypeChanged => "T",
            Self::Conflicted => "U",
            Self::Untracked => "?",
            Self::Unknown => "·",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Clean => "unchanged",
            Self::Added => "added",
            Self::Modified => "modified",
            Self::Deleted => "deleted",
            Self::Renamed => "renamed",
            Self::Copied => "copied",
            Self::TypeChanged => "type changed",
            Self::Conflicted => "conflicted",
            Self::Untracked => "untracked",
            Self::Unknown => "unknown",
        }
    }

    fn from_code(code: u8) -> Self {
        match code {
            b'A' => Self::Added,
            b'M' => Self::Modified,
            b'R' => Self::Renamed,
            b'C' => Self::Copied,
            b'D' => Self::Deleted,
            b'T' => Self::TypeChanged,
            b'U' => Self::Conflicted,
            b'?' => Self::Untracked,
            b'.' | b' ' => Self::Clean,
            _ => Self::Unknown,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusEntry {
    /// Path relative to the repository top level, as Git reports it.
    pub path: RepoPath,
    /// Previous path for a rename or copy.
    pub original_path: Option<RepoPath>,
    pub index: ChangeState,
    pub worktree: ChangeState,
    pub conflicted: bool,
}

impl StatusEntry {
    pub fn is_untracked(&self) -> bool {
        self.worktree == ChangeState::Untracked
    }

    pub fn is_conflicted(&self) -> bool {
        self.conflicted
    }

    /// True when this entry only carries an index change, so the per-file diff
    /// should default to `--cached`.
    pub fn prefers_staged(&self) -> bool {
        !self.conflicted
            && self.worktree == ChangeState::Clean
            && self.index != ChangeState::Clean
            && self.index != ChangeState::Untracked
    }

    pub fn is_rename(&self) -> bool {
        self.original_path.is_some()
            || self.index == ChangeState::Renamed
            || self.worktree == ChangeState::Renamed
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Status {
    pub branch: Option<String>,
    pub head: Option<String>,
    /// Git reports `(detached)` instead of a branch name; keep both facts.
    pub detached: bool,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub entries: Vec<StatusEntry>,
    /// `true` when [`crate::Limits::max_status_entries`] cut the list, so the
    /// counts below describe the listed entries only.
    pub truncated: bool,
}

impl Status {
    pub fn staged(&self) -> usize {
        self.entries
            .iter()
            .filter(|entry| {
                !entry.conflicted
                    && !entry.is_untracked()
                    && entry.index != ChangeState::Clean
                    && entry.index != ChangeState::Unknown
            })
            .count()
    }

    pub fn unstaged(&self) -> usize {
        self.entries
            .iter()
            .filter(|entry| {
                !entry.conflicted
                    && !entry.is_untracked()
                    && entry.worktree != ChangeState::Clean
                    && entry.worktree != ChangeState::Unknown
            })
            .count()
    }

    pub fn untracked(&self) -> usize {
        self.entries
            .iter()
            .filter(|entry| entry.is_untracked())
            .count()
    }

    pub fn conflicted(&self) -> usize {
        self.entries.iter().filter(|entry| entry.conflicted).count()
    }

    /// Clean means "no changes" with confidence. A byte-truncated status with
    /// no parsed entries is unknown, not clean.
    pub fn is_clean(&self) -> bool {
        self.entries.is_empty() && !self.truncated
    }

    pub fn head_short(&self) -> Option<&str> {
        self.head
            .as_deref()
            .map(|head| head.get(..7).unwrap_or(head))
    }

    pub fn branch_label(&self) -> &str {
        if self.detached {
            "detached HEAD"
        } else {
            self.branch.as_deref().unwrap_or("no branch")
        }
    }
}

pub(crate) fn parse(raw: &[u8], max_entries: usize, output_truncated: bool) -> Status {
    let mut status = Status {
        truncated: output_truncated,
        ..Status::default()
    };
    // A truncated stream can end in the middle of a record. Discard that
    // partial record instead of reporting a mangled path.
    let mut records = raw.split(|byte| *byte == 0).collect::<Vec<_>>();
    if output_truncated && !raw.ends_with(b"\0") {
        records.pop();
    }
    let mut records = records.into_iter();
    while let Some(record) = records.next() {
        if record.is_empty() {
            continue;
        }
        if record.starts_with(b"# ") {
            // Real `-z` output NUL-terminates each header, but accept a
            // newline-separated block too so parsing is robust to tooling.
            for line in record.split(|byte| *byte == b'\n') {
                if let Some(header) = line.strip_prefix(b"# ") {
                    parse_branch_header(header, &mut status);
                }
            }
            continue;
        }
        match record.first() {
            Some(b'1') => {
                if status.entries.len() >= max_entries {
                    status.truncated = true;
                    break;
                }
                if let Some(entry) = parse_ordinary(record) {
                    status.entries.push(entry);
                }
            }
            Some(b'2') => {
                if status.entries.len() >= max_entries {
                    status.truncated = true;
                    break;
                }
                if let Some(mut entry) = parse_renamed(record) {
                    if let Some(original) = records.next().filter(|path| !path.is_empty()) {
                        entry.original_path = Some(RepoPath::from_bytes(original.to_vec()));
                    }
                    status.entries.push(entry);
                }
            }
            Some(b'u') => {
                if status.entries.len() >= max_entries {
                    status.truncated = true;
                    break;
                }
                if let Some(entry) = parse_unmerged(record) {
                    status.entries.push(entry);
                }
            }
            Some(b'?') => {
                if status.entries.len() >= max_entries {
                    status.truncated = true;
                    break;
                }
                if let Some(path) = record.splitn(2, |byte| *byte == b' ').nth(1) {
                    status.entries.push(StatusEntry {
                        path: RepoPath::from_bytes(path.to_vec()),
                        original_path: None,
                        index: ChangeState::Clean,
                        worktree: ChangeState::Untracked,
                        conflicted: false,
                    });
                }
            }
            // `!` ignored entries are never requested, but ignore them safely.
            _ => continue,
        }
    }
    status
}

fn parse_branch_header(header: &[u8], status: &mut Status) {
    let Some(space) = header.iter().position(|byte| *byte == b' ') else {
        return;
    };
    let (key, value) = (&header[..space], &header[space + 1..]);
    match key {
        b"branch.oid" => {
            if value != b"(initial)" {
                status.head = Some(String::from_utf8_lossy(value).into_owned());
            }
        }
        b"branch.head" => {
            if value == b"(detached)" {
                status.detached = true;
            } else {
                status.branch = Some(String::from_utf8_lossy(value).into_owned());
            }
        }
        b"branch.upstream" => {
            status.upstream = Some(String::from_utf8_lossy(value).into_owned());
        }
        b"branch.ab" => {
            let text = String::from_utf8_lossy(value);
            for part in text.split_whitespace() {
                if let Some(ahead) = part.strip_prefix('+') {
                    status.ahead = ahead.parse().unwrap_or(0);
                } else if let Some(behind) = part.strip_prefix('-') {
                    status.behind = behind.parse().unwrap_or(0);
                }
            }
        }
        _ => {}
    }
}

/// `1 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <path>`
fn parse_ordinary(record: &[u8]) -> Option<StatusEntry> {
    let fields: Vec<&[u8]> = record.splitn(9, |byte| *byte == b' ').collect();
    if fields.len() != 9 {
        return None;
    }
    let (index, worktree) = parse_xy(fields[1]);
    Some(StatusEntry {
        path: RepoPath::from_bytes(fields[8].to_vec()),
        original_path: None,
        index,
        worktree,
        conflicted: false,
    })
}

/// `2 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <Xscore> <path>` followed by the
/// original path in the next NUL-separated record.
fn parse_renamed(record: &[u8]) -> Option<StatusEntry> {
    let fields: Vec<&[u8]> = record.splitn(10, |byte| *byte == b' ').collect();
    if fields.len() != 10 {
        return None;
    }
    let (index, worktree) = parse_xy(fields[1]);
    Some(StatusEntry {
        path: RepoPath::from_bytes(fields[9].to_vec()),
        original_path: None,
        index,
        worktree,
        conflicted: false,
    })
}

/// `u <XY> <sub> <m1> <m2> <m3> <mW> <h1> <h2> <h3> <path>`
fn parse_unmerged(record: &[u8]) -> Option<StatusEntry> {
    let fields: Vec<&[u8]> = record.splitn(11, |byte| *byte == b' ').collect();
    if fields.len() != 11 {
        return None;
    }
    Some(StatusEntry {
        path: RepoPath::from_bytes(fields[10].to_vec()),
        original_path: None,
        index: ChangeState::Conflicted,
        worktree: ChangeState::Conflicted,
        conflicted: true,
    })
}

fn parse_xy(xy: &[u8]) -> (ChangeState, ChangeState) {
    match xy {
        [index, worktree, ..] => (
            ChangeState::from_code(*index),
            ChangeState::from_code(*worktree),
        ),
        _ => (ChangeState::Unknown, ChangeState::Unknown),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_branch_headers_and_ordinary_entries() {
        let mut raw = b"# branch.oid abc123def\0# branch.head main\0# branch.upstream origin/main\0# branch.ab +2 -3\0".to_vec();
        raw.extend_from_slice(b"1 .M N... 100644 100644 100644 aaa bbb src/lib.rs\0");
        let status = parse(&raw, 10, false);
        assert_eq!(status.branch.as_deref(), Some("main"));
        assert_eq!(status.head.as_deref(), Some("abc123def"));
        assert_eq!(status.upstream.as_deref(), Some("origin/main"));
        assert_eq!((status.ahead, status.behind), (2, 3));
        assert_eq!(status.entries.len(), 1);
        let entry = &status.entries[0];
        assert_eq!(entry.path.display(), "src/lib.rs");
        assert_eq!(entry.index, ChangeState::Clean);
        assert_eq!(entry.worktree, ChangeState::Modified);
        assert!(!entry.prefers_staged());
    }

    #[test]
    fn parses_rename_with_original_path_and_spaces() {
        let raw = b"2 R. N... 100644 100644 100644 aaa bbb R100 new name.rs\0old name.rs\0";
        let status = parse(raw, 10, false);
        let entry = &status.entries[0];
        assert_eq!(entry.path.display(), "new name.rs");
        assert_eq!(
            entry.original_path.as_ref().unwrap().display(),
            "old name.rs"
        );
        assert!(entry.is_rename());
        assert!(entry.prefers_staged());
    }

    #[test]
    fn parses_unicode_and_non_utf8_paths_without_panicking() {
        let mut raw = b"1 .M N... 100644 100644 100644 aaa bbb caff\xc3\xa8.rs\0".to_vec();
        raw.extend_from_slice(b"? caf\xe9.rs\0");
        let status = parse(&raw, 10, false);
        assert_eq!(status.entries[0].path.display(), "caffè.rs");
        assert!(!status.entries[1].path.is_utf8());
        assert_eq!(status.entries[1].path.display(), "caf�.rs");
        assert_eq!(status.untracked(), 1);
    }

    #[test]
    fn parses_conflicts_and_counts_them_separately() {
        let raw = b"u UU N... 100644 100644 100644 100644 aaa bbb ccc conflict.rs\0";
        let status = parse(raw, 10, false);
        assert_eq!(status.conflicted(), 1);
        assert_eq!(status.staged(), 0);
        assert_eq!(status.unstaged(), 0);
    }

    #[test]
    fn truncates_at_the_entry_limit() {
        let mut raw = Vec::new();
        for index in 0..5 {
            raw.extend_from_slice(format!("? file{index}.rs\0").as_bytes());
        }
        let status = parse(&raw, 2, false);
        assert_eq!(status.entries.len(), 2);
        assert!(status.truncated);
    }

    #[test]
    fn byte_truncation_is_reported_and_partial_records_are_dropped() {
        // The second record is cut mid-path, as a byte cap would do.
        let raw = b"? complete.txt\0? incomplete-file-name";
        let status = parse(raw, 10, true);
        assert!(status.truncated);
        assert_eq!(status.entries.len(), 1);
        assert_eq!(status.entries[0].path.display(), "complete.txt");
        assert!(!status.is_clean());
    }

    #[test]
    fn detached_and_initial_head_are_representable() {
        let raw = b"# branch.oid (initial)\0# branch.head (detached)\0";
        let status = parse(raw, 10, false);
        assert!(status.detached);
        assert!(status.head.is_none());
        assert!(status.branch.is_none());
        assert_eq!(status.branch_label(), "detached HEAD");
    }
}
