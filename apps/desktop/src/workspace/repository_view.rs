//! Rendering for the read-only Files, Diff and Activity surfaces.
//!
//! Every branch distinguishes loading, genuinely empty, unsupported and
//! failed. Untracked files are listed but never given a fabricated diff, and
//! nothing here starts, stops or writes anything.

use super::repository::{
    bounded_preview, RepositoryDiagnostic, RepositoryLoad, RepositorySurface,
    DIFF_PREVIEW_MAX_BYTES, DIFF_PREVIEW_MAX_LINES, FILE_ROWS_MAX,
};
use super::*;
use ukk_worktree::{Activity, Diff, Status, StatusEntry};

impl Workspace {
    /// Shared surface chrome plus the selected repository body.
    pub(super) fn repository_surface_view(
        &self,
        surface: RepositorySurface,
        pane_id: &str,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let body = match surface {
            RepositorySurface::Files => self.files_surface(pane_id, cx),
            RepositorySurface::Diff => self.diff_surface(pane_id, cx),
            RepositorySurface::Activity => self.activity_surface(pane_id, cx),
        };
        div()
            .id(format!("repository-{pane_id}-{}", surface.title()))
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .gap_2()
            .child(self.repository_header(surface, pane_id, cx))
            .child(body)
            .into_any_element()
    }

    fn repository_header(
        &self,
        surface: RepositorySurface,
        pane_id: &str,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let root = self
            .repository
            .project_root
            .clone()
            .filter(|root| !root.is_empty())
            .unwrap_or_else(|| "project root not configured".into());
        let branch = match &self.repository.status {
            RepositoryLoad::Loading => "reading branch…".to_owned(),
            RepositoryLoad::Failed(_) => "branch unavailable".to_owned(),
            RepositoryLoad::Ready(status) => format!(
                "{} · {}",
                status.branch_label(),
                status.head_short().unwrap_or("unborn")
            ),
        };
        let diagnostics = self.repository.diagnostics();
        div()
            .flex()
            .flex_wrap()
            .items_start()
            .justify_between()
            .gap_2()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .min_w_0()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgb(MUTED))
                            .child("READ-ONLY GIT · LOCAL ONLY"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(INK))
                            .child(format!("{root} · {branch}")),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_1()
                    .child(self.surface_button(
                        format!("{pane_id}-repo-refresh"),
                        "Refresh",
                        format!("Re-read repository state for the {} view", surface.title()),
                        move |this, cx| this.reload_repository_surfaces(cx),
                        cx,
                    ))
                    .child(self.surface_button(
                        format!("{pane_id}-repo-copy"),
                        "Copy state",
                        "Copy repository diagnostics".into(),
                        move |this, cx| this.copy_repository_diagnostics(diagnostics.clone(), cx),
                        cx,
                    )),
            )
            .into_any_element()
    }

    pub(super) fn surface_button<F>(
        &self,
        id: String,
        label: &'static str,
        aria_label: String,
        handler: F,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement
    where
        F: Fn(&mut Self, &mut Context<Self>) + 'static,
    {
        div()
            .id(id)
            .role(Role::Button)
            .aria_label(aria_label)
            .focusable()
            .focus_visible(|style| style.border_color(rgb(ACCENT)))
            .px_2()
            .py_1()
            .rounded_md()
            .border_1()
            .border_color(rgb(LINE))
            .bg(rgb(WHITE))
            .text_xs()
            .text_color(rgb(ACCENT))
            .cursor_pointer()
            .hover(|style| style.bg(rgb(SURFACE)))
            .on_click(cx.listener(move |this, _, _, cx| handler(this, cx)))
            .child(label)
            .into_any_element()
    }

    fn notice(&self, text: impl Into<String>) -> gpui::AnyElement {
        div()
            .p_3()
            .rounded_md()
            .bg(rgb(SURFACE))
            .text_xs()
            .text_color(rgb(MUTED))
            .child(text.into())
            .into_any_element()
    }

    fn loading_block(&self, message: &'static str) -> gpui::AnyElement {
        div()
            .flex()
            .flex_col()
            .gap_1()
            .p_4()
            .rounded_lg()
            .border_1()
            .border_color(rgb(LINE))
            .bg(rgb(WHITE))
            .child(div().text_sm().text_color(rgb(INK)).child(message))
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child("Reading the local working tree without changing it."),
            )
            .into_any_element()
    }

    fn error_block<F>(
        &self,
        diagnostic: &RepositoryDiagnostic,
        pane_id: &str,
        retry_label: &'static str,
        retry: F,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement
    where
        F: Fn(&mut Self, &mut Context<Self>) + 'static,
    {
        let copy = diagnostic.copy_text();
        let (detail, detail_truncated) = bounded_preview(&diagnostic.detail, 12, 4 * 1024);
        div()
            .flex()
            .flex_col()
            .gap_2()
            .p_4()
            .rounded_lg()
            .border_1()
            .border_color(rgb(0xe5c9be))
            .bg(rgb(0xfff7f4))
            .child(
                div()
                    .text_sm()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(rgb(0xa14b32))
                    .child(diagnostic.summary.clone()),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child(diagnostic.hint.clone()),
            )
            .child(
                div()
                    .p_2()
                    .rounded_md()
                    .bg(rgb(WHITE))
                    .text_xs()
                    .text_color(rgb(INK))
                    .child(detail),
            )
            .when(detail_truncated, |block| {
                block.child(
                    div()
                        .text_xs()
                        .text_color(rgb(MUTED))
                        .child("Diagnostic shortened; Copy includes the full text."),
                )
            })
            .child(
                div()
                    .flex()
                    .gap_1()
                    .child(self.surface_button(
                        format!("{pane_id}-repo-retry"),
                        retry_label,
                        "Retry this read-only inspection".into(),
                        retry,
                        cx,
                    ))
                    .child(self.surface_button(
                        format!("{pane_id}-repo-copy-error"),
                        "Copy diagnostics",
                        "Copy the full repository diagnostic".into(),
                        move |this, cx| this.copy_repository_diagnostics(copy.clone(), cx),
                        cx,
                    )),
            )
            .into_any_element()
    }

    fn files_surface(&self, pane_id: &str, cx: &mut Context<Self>) -> gpui::AnyElement {
        match &self.repository.status {
            RepositoryLoad::Loading => self.loading_block("Reading changed files…"),
            RepositoryLoad::Failed(diagnostic) => self.error_block(
                diagnostic,
                pane_id,
                "Retry",
                move |this, cx| this.reload_repository_surfaces(cx),
                cx,
            ),
            RepositoryLoad::Ready(status) => self.files_ready(status, pane_id, cx),
        }
    }

    fn files_ready(
        &self,
        status: &Status,
        pane_id: &str,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let mut column = div().flex().flex_col().flex_1().min_h_0().gap_2().child(
            div()
                .flex()
                .flex_wrap()
                .gap_3()
                .text_xs()
                .text_color(rgb(MUTED))
                .child(format!("staged {}", status.staged()))
                .child(format!("unstaged {}", status.unstaged()))
                .child(format!("untracked {}", status.untracked()))
                .child(format!("conflicts {}", status.conflicted()))
                .when_some(status.upstream.clone(), |row, upstream| {
                    row.child(format!(
                        "upstream {upstream} ↑{} ↓{}",
                        status.ahead, status.behind
                    ))
                }),
        );

        if status.is_clean() {
            column = column.child(self.notice(
                "Working tree is clean. No staged, unstaged or untracked files under this project root.",
            ));
            return column.into_any_element();
        }

        let visible = status.entries.len().min(FILE_ROWS_MAX);
        let rows = status
            .entries
            .iter()
            .take(visible)
            .enumerate()
            .map(|(index, entry)| self.file_row(pane_id, index, entry, cx))
            .collect::<Vec<_>>();
        column = column.child(
            div()
                .id(format!("files-list-{pane_id}"))
                .flex()
                .flex_col()
                .gap_1()
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .children(rows),
        );
        if status.truncated || status.entries.len() > visible {
            let total = if status.truncated {
                format!("at least {}", status.entries.len())
            } else {
                status.entries.len().to_string()
            };
            column = column.child(self.notice(format!(
                "Showing {visible} of {total} changed entries. The list is bounded; the repository is unchanged."
            )));
        }
        column
            .child(self.notice(
                "Selecting a tracked file opens its unified diff. Untracked files have no Git baseline, so no diff is invented for them.",
            ))
            .into_any_element()
    }

    fn file_row(
        &self,
        pane_id: &str,
        index: usize,
        entry: &StatusEntry,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let path = entry.path.clone();
        let staged = entry.prefers_staged();
        let untracked = entry.is_untracked();
        let selected = self
            .repository
            .selection
            .as_ref()
            .is_some_and(|selection| selection.path == path);
        let codes = if entry.conflicted {
            "UU".to_owned()
        } else {
            format!("{}{}", entry.index.letter(), entry.worktree.letter())
        };
        let label = entry.path.display().into_owned();
        let original = entry
            .original_path
            .as_ref()
            .map(|path| format!("← {}", path.display()));
        let mut aria = format!(
            "{codes} {label} — {} / {}",
            entry.index.label(),
            entry.worktree.label()
        );
        if untracked {
            aria.push_str("; untracked, no diff baseline");
        }
        if !entry.path.is_utf8() {
            aria.push_str("; non-UTF-8 file name");
        }
        div()
            .id(format!("{pane_id}-file-{index}"))
            .role(Role::Button)
            .aria_label(aria)
            .aria_selected(selected)
            .focusable()
            .focus_visible(|style| style.border_color(rgb(ACCENT)))
            .flex()
            .items_center()
            .gap_2()
            .px_2()
            .py_1()
            .rounded_md()
            .border_1()
            .border_color(rgb(if selected { ACCENT } else { LINE }))
            .bg(rgb(if selected { 0xeef0f8 } else { WHITE }))
            .cursor_pointer()
            .hover(|style| style.bg(rgb(SURFACE)))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.select_repository_file(path.clone(), staged, untracked, cx);
            }))
            .child(
                div()
                    .w(px(26.0))
                    .flex_none()
                    .text_xs()
                    .text_color(rgb(if entry.conflicted { 0xa14b32 } else { MUTED }))
                    .child(codes),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_xs()
                    .text_color(rgb(INK))
                    .child(label),
            )
            .when_some(original, |row, original| {
                row.child(div().text_xs().text_color(rgb(MUTED)).child(original))
            })
            .into_any_element()
    }

    fn diff_surface(&self, pane_id: &str, cx: &mut Context<Self>) -> gpui::AnyElement {
        let selection = self.repository.selection.clone();
        if let Some(selection) = &selection {
            if selection.untracked {
                return div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(self.notice(format!(
                        "Untracked file · {}. Git has no baseline for an untracked file, so there is no unified diff. It is listed under Files; ukk does not synthesize a diff.",
                        selection.path.display()
                    )))
                    .child(
                        div()
                            .flex()
                            .gap_1()
                            .child(self.surface_button(
                                format!("{pane_id}-diff-whole"),
                                "Whole tree",
                                "Show the tracked whole-tree diff".into(),
                                move |this, cx| this.show_whole_tree_diff(cx),
                                cx,
                            )),
                    )
                    .into_any_element();
            }
        }

        let mut column = div().flex().flex_col().flex_1().min_h_0().gap_2();
        column = column.child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_1()
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(MUTED))
                        .child(match &selection {
                            Some(selection) => {
                                let mode = if selection.staged {
                                    "staged"
                                } else {
                                    "unstaged"
                                };
                                format!("{mode} diff · {}", selection.path.display())
                            }
                            None => "whole tree · HEAD versus working tree".to_owned(),
                        }),
                )
                .when_some(selection.clone(), |row, selection| {
                    let path = selection.path.clone();
                    let staged = !selection.staged;
                    let label = if staged {
                        "Show staged"
                    } else {
                        "Show unstaged"
                    };
                    row.child(self.surface_button(
                        format!("{pane_id}-diff-toggle"),
                        label,
                        "Switch between the staged and unstaged diff for this file".into(),
                        move |this, cx| {
                            this.select_repository_file(path.clone(), staged, false, cx);
                        },
                        cx,
                    ))
                    .child(self.surface_button(
                        format!("{pane_id}-diff-whole"),
                        "Whole tree",
                        "Show the tracked whole-tree diff".into(),
                        move |this, cx| this.show_whole_tree_diff(cx),
                        cx,
                    ))
                }),
        );

        column = column.child(match &self.repository.diff {
            RepositoryLoad::Loading => self.loading_block("Computing the unified diff…"),
            RepositoryLoad::Failed(diagnostic) => {
                let path = selection
                    .as_ref()
                    .map(|selection| (selection.path.clone(), selection.staged));
                self.error_block(
                    diagnostic,
                    pane_id,
                    "Retry",
                    move |this, cx| match &path {
                        Some((path, staged)) => {
                            let staged = *staged;
                            this.select_repository_file(path.clone(), staged, false, cx);
                        }
                        None => this.reload_repository_surfaces(cx),
                    },
                    cx,
                )
            }
            RepositoryLoad::Ready(diff) => self.diff_ready(diff, pane_id),
        });
        column.into_any_element()
    }

    fn diff_ready(&self, diff: &Diff, pane_id: &str) -> gpui::AnyElement {
        if diff.is_empty() {
            return self.notice(if self.repository.selection.is_none() {
                "No tracked differences. Untracked files are listed under Files and are not part of a Git diff."
            } else {
                "No differences for this selection."
            });
        }
        let (preview, preview_truncated) =
            bounded_preview(&diff.text, DIFF_PREVIEW_MAX_LINES, DIFF_PREVIEW_MAX_BYTES);
        let mut column = div().flex().flex_col().flex_1().min_h_0().gap_2();
        if diff.is_binary_only() {
            column = column.child(self.notice(format!(
                "{} binary file(s) differ. Git cannot render a textual diff for binary content.",
                diff.binary_files
            )));
        }
        column = column.child(
            div()
                .id(format!("repository-diff-text-{pane_id}"))
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .min_w_0()
                .overflow_y_scroll()
                .p_3()
                .rounded_md()
                .bg(rgb(SURFACE))
                .text_xs()
                .text_color(rgb(INK))
                .child(preview),
        );
        if preview_truncated || diff.truncated {
            column = column.child(self.notice(format!(
                "Preview bounded to {} lines.{} The full diff is available through Git on this machine.",
                DIFF_PREVIEW_MAX_LINES,
                if diff.truncated {
                    format!(" Git output was capped at {} bytes.", diff.bytes)
                } else {
                    String::new()
                }
            )));
        } else {
            column = column.child(
                div()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child(format!("{} file(s) · {} bytes", diff.files, diff.bytes)),
            );
        }
        column.into_any_element()
    }

    fn activity_surface(&self, pane_id: &str, cx: &mut Context<Self>) -> gpui::AnyElement {
        match &self.repository.activity {
            RepositoryLoad::Loading => self.loading_block("Reading recent commits…"),
            RepositoryLoad::Failed(diagnostic) => self.error_block(
                diagnostic,
                pane_id,
                "Retry",
                move |this, cx| this.reload_repository_surfaces(cx),
                cx,
            ),
            RepositoryLoad::Ready(activity) => self.activity_ready(activity, pane_id),
        }
    }

    fn activity_ready(&self, activity: &Activity, pane_id: &str) -> gpui::AnyElement {
        let mut column = div().flex().flex_col().flex_1().min_h_0().gap_1();
        if activity.commits.is_empty() {
            column = column.child(self.notice(if activity.has_commits {
                "No commits touch this project root. History outside the root is not shown."
            } else {
                "No commits for this project root yet. Staged and untracked files still appear under Files."
            }));
            return column.into_any_element();
        }
        let rows = activity
            .commits
            .iter()
            .map(|commit| {
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .px_2()
                    .py_2()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(LINE))
                    .bg(rgb(WHITE))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_xs()
                            .child(
                                div()
                                    .text_color(rgb(ACCENT))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child(commit.short.clone()),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_color(rgb(INK))
                                    .child(commit.subject.clone()),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(MUTED))
                            .child(format!("{} · {}", commit.author, commit.time)),
                    )
                    .into_any_element()
            })
            .collect::<Vec<_>>();
        column = column.child(
            div()
                .id(format!("repository-activity-list-{pane_id}"))
                .flex()
                .flex_col()
                .gap_1()
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .children(rows),
        );
        if activity.truncated {
            column = column.child(self.notice(
                "Activity is bounded to the most recent commits. Older history is not loaded.",
            ));
        }
        column.into_any_element()
    }
}
