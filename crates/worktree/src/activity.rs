//! Recent commit metadata for a project root.

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Commit {
    pub id: String,
    pub short: String,
    pub author: String,
    pub time: String,
    pub subject: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Activity {
    pub commits: Vec<Commit>,
    /// More commits exist than were returned.
    pub truncated: bool,
    /// At least one commit was found for this project root.
    pub has_commits: bool,
}

impl Activity {
    pub(crate) fn empty() -> Self {
        Self::default()
    }

    /// Parse `%H%x1f%h%x1f%an%x1f%aI%x1f%s%x00` records.
    ///
    /// `head_exists` distinguishes "the project root has no commits of its
    /// own" from "this repository has no commits at all".
    pub(crate) fn parse(
        raw: &[u8],
        max_commits: usize,
        output_truncated: bool,
        head_exists: bool,
    ) -> Self {
        let mut commits = Vec::new();
        let mut truncated = output_truncated;
        for record in raw.split(|byte| *byte == 0) {
            if record.is_empty() {
                continue;
            }
            if commits.len() >= max_commits {
                truncated = true;
                break;
            }
            let fields: Vec<&[u8]> = record.splitn(5, |byte| *byte == 0x1f).collect();
            if fields.len() != 5 {
                continue;
            }
            commits.push(Commit {
                id: String::from_utf8_lossy(fields[0]).into_owned(),
                short: String::from_utf8_lossy(fields[1]).into_owned(),
                author: String::from_utf8_lossy(fields[2]).into_owned(),
                time: String::from_utf8_lossy(fields[3]).into_owned(),
                subject: String::from_utf8_lossy(fields[4]).into_owned(),
            });
        }
        let has_commits = head_exists || !commits.is_empty();
        Self {
            commits,
            truncated,
            has_commits,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nul_separated_commits() {
        let raw = b"abc\x1fabc1234\x1fAda\x1f2026-11-08T10:00:00+01:00\x1ffeat: one\x00def\x1fdef5678\x1fLin\x1f2026-11-07T09:00:00+01:00\x1ffix: two\x00";
        let activity = Activity::parse(raw, 20, false, true);
        assert_eq!(activity.commits.len(), 2);
        assert_eq!(activity.commits[0].subject, "feat: one");
        assert_eq!(activity.commits[1].author, "Lin");
        // `%x00` with `format:` must not leave a newline on later ids.
        assert_eq!(activity.commits[0].id, "abc");
        assert_eq!(activity.commits[1].id, "def");
        assert!(activity.has_commits);
        assert!(!activity.truncated);
    }

    #[test]
    fn truncates_to_the_commit_limit() {
        let raw = b"a\x1fa\x1fa\x1fa\x1fone\x00b\x1fb\x1fb\x1fb\x1ftwo\x00";
        let activity = Activity::parse(raw, 1, false, true);
        assert_eq!(activity.commits.len(), 1);
        assert!(activity.truncated);
    }

    #[test]
    fn empty_output_is_an_empty_activity() {
        let activity = Activity::parse(b"", 20, false, false);
        assert!(!activity.has_commits);
        assert!(activity.commits.is_empty());
    }

    #[test]
    fn history_outside_the_project_root_is_distinguishable() {
        // HEAD exists, but the path-scoped log returned nothing.
        let activity = Activity::parse(b"", 20, false, true);
        assert!(activity.commits.is_empty());
        assert!(activity.has_commits);
    }
}
