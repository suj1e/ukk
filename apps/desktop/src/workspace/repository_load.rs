//! Background loading of read-only repository surfaces.
//!
//! Git runs on the GPUI background executor with compact limits. Every reply
//! is tagged with the generation that requested it, so switching Sessions
//! cannot display another Session's repository state.

use super::repository::{
    repository_target, RepositoryDiagnostic, RepositoryLoad, RepositorySelection, RepositoryTarget,
};
use super::*;
use ukk_worktree::{DiffScope, Limits, RepoPath, Worktree};

impl Workspace {
    /// Resolve the selected Session's project root and start a load only when
    /// the target actually changed. Long-poll updates therefore do not spawn a
    /// Git process on every revision bump.
    pub(super) fn sync_repository_surfaces(&mut self, cx: &mut Context<Self>) {
        match repository_target(&self.domain, self.selected_session.as_deref()) {
            RepositoryTarget::Unavailable => {
                if self.repository.session_id.is_some() {
                    self.repository.clear();
                    cx.notify();
                }
            }
            RepositoryTarget::MissingRoot {
                session_id,
                space_title,
            } => {
                let already_reported = self.repository.session_id.as_deref()
                    == Some(session_id.as_str())
                    && self.repository.status.failed().is_some();
                if already_reported {
                    return;
                }
                self.repository.begin(&session_id, "");
                self.repository
                    .fail_all(RepositoryDiagnostic::missing_root(&space_title));
                cx.notify();
            }
            RepositoryTarget::Ready {
                session_id,
                project_root,
            } => {
                let unchanged = self.repository.session_id.as_deref() == Some(session_id.as_str())
                    && self.repository.project_root.as_deref() == Some(project_root.as_str());
                if unchanged {
                    return;
                }
                let generation = self.repository.begin(&session_id, &project_root);
                self.spawn_repository_load(generation, project_root, cx);
                cx.notify();
            }
        }
    }

    /// Explicit user refresh: re-read status, whole-tree diff and activity.
    pub(super) fn reload_repository_surfaces(&mut self, cx: &mut Context<Self>) {
        let RepositoryTarget::Ready {
            session_id,
            project_root,
        } = repository_target(&self.domain, self.selected_session.as_deref())
        else {
            self.sync_repository_surfaces(cx);
            return;
        };
        let generation = self.repository.begin(&session_id, &project_root);
        self.spawn_repository_load(generation, project_root, cx);
        cx.notify();
    }

    fn spawn_repository_load(
        &mut self,
        generation: u64,
        project_root: String,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            let root = project_root.clone();
            let loaded = cx
                .background_executor()
                .spawn(async move {
                    let worktree = Worktree::with_limits(root, Limits::compact())?;
                    Ok((
                        worktree.status(),
                        worktree.diff(DiffScope::Head),
                        worktree.activity(),
                    ))
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                if !view.repository.is_current(generation) {
                    return; // a newer Session or refresh owns the surface now
                }
                let root = view.repository.project_root.clone().unwrap_or_default();
                match loaded {
                    Err(error) => {
                        view.repository.fail_all(RepositoryDiagnostic::from_error(
                            &root,
                            "the repository",
                            &error,
                        ));
                    }
                    Ok((status, diff, activity)) => {
                        view.repository.status = load_part(status, &root, "changed files");
                        // A per-file diff requested while this load was running
                        // owns the Diff surface; do not overwrite it.
                        if view.repository.selection.is_none() {
                            view.repository.diff = load_part(diff, &root, "the whole-tree diff");
                        }
                        view.repository.activity = load_part(activity, &root, "recent activity");
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Show a single file's unified diff. `staged` selects HEAD/index rather
    /// than index/worktree.
    pub(super) fn select_repository_file(
        &mut self,
        path: RepoPath,
        staged: bool,
        untracked: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(project_root) = self.repository.project_root.clone() else {
            return;
        };
        if project_root.is_empty() {
            return;
        }
        let generation = self.repository.generation;
        let selection = RepositorySelection {
            path: path.clone(),
            staged,
            untracked,
        };
        self.repository.selection = Some(selection.clone());
        if untracked {
            // Never ask Git for a baseline that cannot exist, and never
            // synthesize one in the UI.
            cx.notify();
            return;
        }
        self.repository.diff = RepositoryLoad::Loading;
        cx.notify();

        cx.spawn(async move |this, cx| {
            let root = project_root.clone();
            let loaded = cx
                .background_executor()
                .spawn(async move {
                    let worktree = Worktree::with_limits(root, Limits::compact())?;
                    let scope = if staged {
                        DiffScope::StagedFile(path)
                    } else {
                        DiffScope::UnstagedFile(path)
                    };
                    worktree.diff(scope)
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                if !view.repository.is_current(generation)
                    || view.repository.selection.as_ref() != Some(&selection)
                {
                    return; // the user picked another file or Session
                }
                let root = view.repository.project_root.clone().unwrap_or_default();
                view.repository.diff = load_part(loaded, &root, "this file diff");
                cx.notify();
            });
        })
        .detach();
    }

    /// Return the Diff surface to the whole working tree.
    pub(super) fn show_whole_tree_diff(&mut self, cx: &mut Context<Self>) {
        let Some(project_root) = self.repository.project_root.clone() else {
            return;
        };
        if project_root.is_empty() {
            return;
        }
        let generation = self.repository.generation;
        self.repository.selection = None;
        self.repository.diff = RepositoryLoad::Loading;
        cx.notify();

        cx.spawn(async move |this, cx| {
            let root = project_root.clone();
            let loaded = cx
                .background_executor()
                .spawn(async move {
                    let worktree = Worktree::with_limits(root, Limits::compact())?;
                    worktree.diff(DiffScope::Head)
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                if !view.repository.is_current(generation) || view.repository.selection.is_some() {
                    return;
                }
                let root = view.repository.project_root.clone().unwrap_or_default();
                view.repository.diff = load_part(loaded, &root, "the whole-tree diff");
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn copy_repository_diagnostics(&mut self, text: String, cx: &mut Context<Self>) {
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
        cx.notify();
    }
}

fn load_part<T>(
    result: Result<T, ukk_worktree::WorktreeError>,
    project_root: &str,
    subject: &str,
) -> RepositoryLoad<T> {
    match result {
        Ok(value) => RepositoryLoad::Ready(value),
        Err(error) => RepositoryLoad::Failed(RepositoryDiagnostic::from_error(
            project_root,
            subject,
            &error,
        )),
    }
}
