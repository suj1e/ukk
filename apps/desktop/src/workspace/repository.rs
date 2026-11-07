//! Read-only repository surface state: Files, Diff and Activity.
//!
//! This module owns presentation state only. Git inspection happens in
//! `repository_load` on a background executor through the `ukk-worktree`
//! crate; nothing here mutates a repository, owns an Agent or invents data.

use super::*;
use ukk_worktree::{Activity, Diff, RepoPath, Status, WorktreeError};

/// Maximum changed-file rows actually instantiated by the UI.
pub(super) const FILE_ROWS_MAX: usize = 200;
/// Maximum diff lines rendered at once.
pub(super) const DIFF_PREVIEW_MAX_LINES: usize = 400;
/// Maximum diff bytes rendered at once.
pub(super) const DIFF_PREVIEW_MAX_BYTES: usize = 64 * 1024;

/// Which read-only surface a Pane tab is showing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RepositorySurface {
    Files,
    Diff,
    Activity,
}

impl RepositorySurface {
    pub(super) fn for_content(content: &PaneContent) -> Option<Self> {
        match content {
            PaneContent::Files => Some(Self::Files),
            PaneContent::Diff => Some(Self::Diff),
            PaneContent::Activity => Some(Self::Activity),
            PaneContent::Agent { .. } | PaneContent::Terminal { .. } => None,
        }
    }

    pub(super) fn title(self) -> &'static str {
        match self {
            Self::Files => "Files",
            Self::Diff => "Diff",
            Self::Activity => "Activity",
        }
    }
}

/// The Session's project root resolved to a repository inspection target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum RepositoryTarget {
    /// No Session is selected, or its Space no longer exists.
    Unavailable,
    /// The Space exists but has no usable project root.
    MissingRoot {
        session_id: String,
        space_title: String,
    },
    /// A local directory that should be inspected read-only.
    Ready {
        session_id: String,
        project_root: String,
    },
}

pub(super) fn repository_target(
    domain: &DomainSnapshot,
    session_id: Option<&str>,
) -> RepositoryTarget {
    let Some(session_id) = session_id else {
        return RepositoryTarget::Unavailable;
    };
    let Some(session) = domain
        .sessions
        .iter()
        .find(|session| session.id == session_id)
    else {
        return RepositoryTarget::Unavailable;
    };
    let Some(space) = domain
        .spaces
        .iter()
        .find(|space| space.id == session.space_id)
    else {
        return RepositoryTarget::MissingRoot {
            session_id: session_id.to_owned(),
            space_title: "unknown Space".into(),
        };
    };
    if space.project_root.trim().is_empty() {
        RepositoryTarget::MissingRoot {
            session_id: session_id.to_owned(),
            space_title: space.title.clone(),
        }
    } else {
        RepositoryTarget::Ready {
            session_id: session_id.to_owned(),
            project_root: space.project_root.clone(),
        }
    }
}

/// A truthful failure with enough context to be copied into a bug report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct RepositoryDiagnostic {
    pub summary: String,
    pub detail: String,
    pub hint: String,
}

impl RepositoryDiagnostic {
    pub(super) fn from_error(project_root: &str, subject: &str, error: &WorktreeError) -> Self {
        Self {
            summary: format!("Could not read {subject}."),
            detail: format!("project root: {project_root}\n{error}"),
            hint: error.hint().to_owned(),
        }
    }

    pub(super) fn missing_root(space_title: &str) -> Self {
        Self {
            summary: "No project root is configured for this Space.".into(),
            detail: format!(
                "Space: {space_title}\n\nThe Files, Diff and Activity views inspect a local Git working tree. They never run a command that writes to the repository."
            ),
            hint: "Set this Space's project root to a directory inside a Git working tree.".into(),
        }
    }

    pub(super) fn copy_text(&self) -> String {
        format!(
            "ukk repository diagnostic\nsummary: {}\nhint: {}\ndetail:\n{}\n",
            self.summary, self.hint, self.detail
        )
    }
}

/// Loading state that distinguishes "waiting" from "there is genuinely no
/// data" and from a typed failure. An empty result is never shown as loading.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum RepositoryLoad<T> {
    Loading,
    Ready(T),
    Failed(RepositoryDiagnostic),
}

impl<T> RepositoryLoad<T> {
    pub(super) fn failed(&self) -> Option<&RepositoryDiagnostic> {
        match self {
            Self::Failed(diagnostic) => Some(diagnostic),
            Self::Loading | Self::Ready(_) => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct RepositorySelection {
    pub path: RepoPath,
    pub staged: bool,
    /// Untracked files have no Git baseline; the Diff surface must say so
    /// instead of requesting an empty diff.
    pub untracked: bool,
}

/// Cached, generation-tagged inspection for the selected Session.
#[derive(Clone, Debug)]
pub(super) struct RepositorySurfaces {
    pub(super) session_id: Option<String>,
    pub(super) project_root: Option<String>,
    pub(super) status: RepositoryLoad<Status>,
    pub(super) diff: RepositoryLoad<Diff>,
    pub(super) activity: RepositoryLoad<Activity>,
    pub(super) selection: Option<RepositorySelection>,
    /// Incremented on every request; late replies from an old generation are
    /// dropped instead of overwriting the current Session's surface.
    pub(super) generation: u64,
}

impl Default for RepositorySurfaces {
    fn default() -> Self {
        Self {
            session_id: None,
            project_root: None,
            status: RepositoryLoad::Loading,
            diff: RepositoryLoad::Loading,
            activity: RepositoryLoad::Loading,
            selection: None,
            generation: 0,
        }
    }
}

impl RepositorySurfaces {
    /// Start a fresh load for one Session and return its generation tag.
    pub(super) fn begin(&mut self, session_id: &str, project_root: &str) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.session_id = Some(session_id.to_owned());
        self.project_root = Some(project_root.to_owned());
        self.status = RepositoryLoad::Loading;
        self.diff = RepositoryLoad::Loading;
        self.activity = RepositoryLoad::Loading;
        self.selection = None;
        self.generation
    }

    pub(super) fn fail_all(&mut self, diagnostic: RepositoryDiagnostic) {
        self.status = RepositoryLoad::Failed(diagnostic.clone());
        self.diff = RepositoryLoad::Failed(diagnostic.clone());
        self.activity = RepositoryLoad::Failed(diagnostic);
    }

    pub(super) fn clear(&mut self) {
        let generation = self.generation.wrapping_add(1);
        *self = Self {
            generation,
            ..Self::default()
        };
    }

    pub(super) fn is_current(&self, generation: u64) -> bool {
        self.generation == generation
    }

    /// Copyable read-only summary of what the surface knows. Contains paths
    /// and counts only: no credentials, no remote URLs.
    pub(super) fn diagnostics(&self) -> String {
        let mut text = String::from("ukk repository view\n");
        text.push_str(&format!(
            "session: {}\n",
            self.session_id.as_deref().unwrap_or("(none)")
        ));
        text.push_str(&format!(
            "project root: {}\n",
            self.project_root.as_deref().unwrap_or("(none)")
        ));
        match &self.status {
            RepositoryLoad::Loading => text.push_str("status: loading\n"),
            RepositoryLoad::Failed(diagnostic) => {
                text.push_str(&format!("status: failed — {}\n", diagnostic.summary));
            }
            RepositoryLoad::Ready(status) => text.push_str(&format!(
                "status: branch {} · head {} · staged {} · unstaged {} · untracked {} · conflicts {}{}\n",
                status.branch_label(),
                status.head_short().unwrap_or("(unborn)"),
                status.staged(),
                status.unstaged(),
                status.untracked(),
                status.conflicted(),
                if status.truncated { " (truncated)" } else { "" }
            )),
        }
        match &self.diff {
            RepositoryLoad::Loading => text.push_str("diff: loading\n"),
            RepositoryLoad::Failed(diagnostic) => {
                text.push_str(&format!("diff: failed — {}\n", diagnostic.summary));
            }
            RepositoryLoad::Ready(diff) => text.push_str(&format!(
                "diff: files {} · binary {} · bytes {}{}\n",
                diff.files,
                diff.binary_files,
                diff.bytes,
                if diff.truncated { " (truncated)" } else { "" }
            )),
        }
        match &self.activity {
            RepositoryLoad::Loading => text.push_str("activity: loading\n"),
            RepositoryLoad::Failed(diagnostic) => {
                text.push_str(&format!("activity: failed — {}\n", diagnostic.summary));
            }
            RepositoryLoad::Ready(activity) => text.push_str(&format!(
                "activity: commits {}{}\n",
                activity.commits.len(),
                if activity.truncated {
                    " (truncated)"
                } else {
                    ""
                }
            )),
        }
        text
    }
}

/// Bounded preview so a huge diff cannot create a huge text element.
/// Returns the preview and whether anything was omitted.
pub(super) fn bounded_preview(text: &str, max_lines: usize, max_bytes: usize) -> (String, bool) {
    let mut preview = String::new();
    let mut truncated = false;
    for (index, line) in text.split_inclusive('\n').enumerate() {
        if index >= max_lines {
            truncated = true;
            break;
        }
        preview.push_str(line);
    }
    if preview.len() > max_bytes {
        let mut end = max_bytes;
        while !preview.is_char_boundary(end) {
            end -= 1;
        }
        preview.truncate(end);
        truncated = true;
    }
    (preview, truncated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ukk_domain::{Session, Space};

    fn domain(project_root: &str) -> DomainSnapshot {
        DomainSnapshot {
            spaces: vec![Space {
                id: "space-1".into(),
                title: "ukk".into(),
                project_root: project_root.into(),
                created_ms: 0,
            }],
            sessions: vec![Session {
                id: "session-1".into(),
                space_id: "space-1".into(),
                title: "Fix the parser".into(),
                archived: false,
                created_ms: 0,
                updated_ms: 0,
            }],
            ..DomainSnapshot::default()
        }
    }

    #[test]
    fn only_repository_surfaces_map_to_a_pane_content() {
        assert_eq!(
            RepositorySurface::for_content(&PaneContent::Files),
            Some(RepositorySurface::Files)
        );
        assert_eq!(
            RepositorySurface::for_content(&PaneContent::Diff),
            Some(RepositorySurface::Diff)
        );
        assert_eq!(
            RepositorySurface::for_content(&PaneContent::Activity),
            Some(RepositorySurface::Activity)
        );
        assert_eq!(
            RepositorySurface::for_content(&PaneContent::Agent {
                agent_run_id: "a".into()
            }),
            None
        );
        assert_eq!(
            RepositorySurface::for_content(&PaneContent::Terminal {
                agent_run_id: "a".into()
            }),
            None
        );
    }

    #[test]
    fn project_root_is_resolved_through_space_and_session() {
        assert_eq!(
            repository_target(&domain("/tmp/project"), Some("session-1")),
            RepositoryTarget::Ready {
                session_id: "session-1".into(),
                project_root: "/tmp/project".into(),
            }
        );
        assert_eq!(
            repository_target(&domain("   "), Some("session-1")),
            RepositoryTarget::MissingRoot {
                session_id: "session-1".into(),
                space_title: "ukk".into(),
            }
        );
        assert_eq!(
            repository_target(&domain("/tmp/project"), Some("missing")),
            RepositoryTarget::Unavailable
        );
        assert_eq!(
            repository_target(&domain("/tmp/project"), None),
            RepositoryTarget::Unavailable
        );
    }

    #[test]
    fn state_transitions_reset_data_and_ignore_stale_generations() {
        let mut surfaces = RepositorySurfaces::default();
        let first = surfaces.begin("session-1", "/tmp/project");
        assert!(surfaces.is_current(first));
        assert!(matches!(surfaces.status, RepositoryLoad::Loading));
        surfaces.status = RepositoryLoad::Ready(Status::default());
        surfaces.selection = Some(RepositorySelection {
            path: RepoPath::from("a.rs"),
            staged: false,
            untracked: false,
        });

        let second = surfaces.begin("session-2", "/tmp/other");
        assert!(second > first);
        assert!(!surfaces.is_current(first));
        assert!(matches!(surfaces.status, RepositoryLoad::Loading));
        assert!(matches!(surfaces.diff, RepositoryLoad::Loading));
        assert!(matches!(surfaces.activity, RepositoryLoad::Loading));
        assert!(surfaces.selection.is_none());
        assert_eq!(surfaces.session_id.as_deref(), Some("session-2"));

        surfaces.clear();
        assert!(!surfaces.is_current(second));
        assert!(surfaces.session_id.is_none());
    }

    #[test]
    fn failure_diagnostics_are_copyable_and_never_invent_data() {
        let error = WorktreeError::NotARepository {
            path: std::path::PathBuf::from("/tmp/not-a-repo"),
            detail: None,
        };
        let diagnostic =
            RepositoryDiagnostic::from_error("/tmp/not-a-repo", "changed files", &error);
        let copy = diagnostic.copy_text();
        assert!(copy.contains("summary: Could not read changed files."));
        assert!(copy.contains("project root: /tmp/not-a-repo"));
        assert!(copy.contains("is not a Git working tree"));
        assert!(copy.contains("Point the Space project root"));

        let missing = RepositoryDiagnostic::missing_root("ukk");
        assert!(missing.copy_text().contains("Space: ukk"));
        assert!(missing.hint.contains("project root"));

        let mut surfaces = RepositorySurfaces::default();
        surfaces.begin("session-1", "/tmp/not-a-repo");
        surfaces.fail_all(diagnostic);
        assert!(surfaces.status.failed().is_some());
        assert!(surfaces.diff.failed().is_some());
        assert!(surfaces.activity.failed().is_some());
        assert!(surfaces.diagnostics().contains("status: failed"));
    }

    #[test]
    fn ready_state_is_rendered_as_data_not_as_loading() {
        let load: RepositoryLoad<Status> = RepositoryLoad::Ready(Status::default());
        assert!(!matches!(load, RepositoryLoad::Loading));
        assert!(matches!(load, RepositoryLoad::Ready(_)));
        assert!(load.failed().is_none());
    }

    #[test]
    fn diff_preview_is_bounded_by_lines_and_bytes() {
        let text = (0..1000)
            .map(|index| format!("line {index}\n"))
            .collect::<String>();
        let (preview, truncated) = bounded_preview(&text, 10, 10_000);
        assert_eq!(preview.lines().count(), 10);
        assert!(truncated);

        let (preview, truncated) = bounded_preview(&text, 10_000, 24);
        assert!(preview.len() <= 24);
        assert_eq!(preview, "line 0\nline 1\nline 2\nlin");
        assert!(truncated);

        // A multibyte character is never cut in half.
        let unicode = "é".repeat(50);
        let (preview, truncated) = bounded_preview(&unicode, 10, 7);
        assert!(preview.len() <= 7);
        assert!(preview.chars().all(|character| character == 'é'));
        assert!(truncated);

        let (preview, truncated) = bounded_preview("short\n", 10, 10_000);
        assert_eq!(preview, "short\n");
        assert!(!truncated);
    }
}
