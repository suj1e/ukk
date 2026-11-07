//! Validated Session split-tree presentation and view-only controls.
use super::*;

impl Workspace {
    pub(super) fn pane_button(
        &self,
        cx: &mut Context<Self>,
        id: String,
        label: &'static str,
        action: PaneAction,
    ) -> impl IntoElement {
        div()
            .id(id)
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
            .on_click(cx.listener(move |this, _, _, cx| {
                if !this.mutation_busy {
                    this.pane_action(action.clone(), cx);
                }
            }))
            .child(label)
    }

    pub(super) fn session_pane(&self, node: &PaneNode, cx: &mut Context<Self>) -> gpui::AnyElement {
        match node {
            PaneNode::Leaf { id, content } => {
                let (heading, description) = match content {
                    PaneContent::Activity => (
                        "Activity".to_string(),
                        "Structured activity stream will be connected here.".to_string(),
                    ),
                    PaneContent::Files => (
                        "Files".to_string(),
                        "Project browser is not connected yet.".to_string(),
                    ),
                    PaneContent::Diff => (
                        "Diff".to_string(),
                        "Git diff integration is upcoming.".to_string(),
                    ),
                    PaneContent::Agent { agent_run_id } => {
                        let binding = self
                            .domain
                            .agent_runs
                            .iter()
                            .find(|a| &a.id == agent_run_id);
                        match binding {
                            Some(run) => (
                                format!("Agent · {}", run.adapter),
                                format!(
                                    "{} / {} · runtime owned by herdr-core.",
                                    run.core_id, run.terminal_address
                                ),
                            ),
                            None => ("Unresolved Agent".into(), "Binding not found.".into()),
                        }
                    }
                    PaneContent::Terminal { agent_run_id } => {
                        let binding = self
                            .domain
                            .agent_runs
                            .iter()
                            .find(|a| &a.id == agent_run_id);
                        match binding {
                            Some(run) => (
                                "Terminal".into(),
                                format!("{} · PTY attachment is upcoming.", run.terminal_address),
                            ),
                            None => ("Unresolved Terminal".into(), "Binding not found.".into()),
                        }
                    }
                };
                let new_content = match content {
                    PaneContent::Agent { agent_run_id } => PaneContent::Terminal {
                        agent_run_id: agent_run_id.clone(),
                    },
                    PaneContent::Diff => PaneContent::Files,
                    PaneContent::Files => PaneContent::Activity,
                    PaneContent::Terminal { .. } | PaneContent::Activity => PaneContent::Diff,
                };
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .p_4()
                    .bg(rgb(WHITE))
                    .rounded_lg()
                    .border_1()
                    .border_color(rgb(LINE))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(rgb(INK))
                                    .child(heading),
                            )
                            .child(
                                div()
                                    .flex()
                                    .gap_1()
                                    .child(self.pane_button(
                                        cx,
                                        format!("{id}-split-right"),
                                        "Split →",
                                        PaneAction::Split {
                                            pane_id: id.clone(),
                                            axis: SplitAxis::Horizontal,
                                            content: new_content.clone(),
                                        },
                                    ))
                                    .child(self.pane_button(
                                        cx,
                                        format!("{id}-split-down"),
                                        "Split ↓",
                                        PaneAction::Split {
                                            pane_id: id.clone(),
                                            axis: SplitAxis::Vertical,
                                            content: new_content,
                                        },
                                    ))
                                    .child(self.pane_button(
                                        cx,
                                        format!("{id}-close"),
                                        "×",
                                        PaneAction::Close {
                                            pane_id: id.clone(),
                                        },
                                    )),
                            ),
                    )
                    .child(
                        div()
                            .mt_3()
                            .text_sm()
                            .text_color(rgb(MUTED))
                            .child(description),
                    )
                    .child(
                        div()
                            .mt_5()
                            .text_xs()
                            .text_color(rgb(MUTED))
                            .child(format!("View ID: {id}")),
                    )
                    .into_any_element()
            }
            PaneNode::Split {
                id,
                axis,
                ratio,
                first,
                second,
            } => {
                let smaller = (*ratio - 0.1).max(0.15);
                let larger = (*ratio + 0.1).min(0.85);
                let controls = div()
                    .flex()
                    .justify_end()
                    .items_center()
                    .gap_2()
                    .mb_2()
                    .child(div().text_xs().text_color(rgb(MUTED)).child(format!(
                        "{}% / {}%",
                        (*ratio * 100.0).round() as i32,
                        ((1.0 - *ratio) * 100.0).round() as i32
                    )))
                    .child(self.pane_button(
                        cx,
                        format!("{id}-smaller"),
                        "−",
                        PaneAction::Resize {
                            split_id: id.clone(),
                            ratio: smaller,
                        },
                    ))
                    .child(self.pane_button(
                        cx,
                        format!("{id}-larger"),
                        "+",
                        PaneAction::Resize {
                            split_id: id.clone(),
                            ratio: larger,
                        },
                    ));
                let first_view = div()
                    .flex()
                    .flex_shrink_1()
                    .min_w_0()
                    .min_h_0()
                    .flex_basis(gpui::DefiniteLength::Fraction(*ratio))
                    .child(self.session_pane(first, cx));
                let second_view = div()
                    .flex()
                    .flex_shrink_1()
                    .min_w_0()
                    .min_h_0()
                    .flex_basis(gpui::DefiniteLength::Fraction(1.0 - *ratio))
                    .child(self.session_pane(second, cx));
                let columns = div().flex().flex_1().min_w_0().min_h_0().gap_3();
                let columns = if *axis == SplitAxis::Vertical {
                    columns.flex_col()
                } else {
                    columns.flex_row()
                };
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .child(controls)
                    .child(columns.child(first_view).child(second_view))
                    .into_any_element()
            }
        }
    }

    pub(super) fn session_detail(
        &self,
        session: &Session,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let root = self
            .domain
            .layouts
            .iter()
            .find(|layout| layout.session_id == session.id)
            .map(|layout| &layout.root);
        let title = session.title.clone();
        let scope = self
            .domain
            .spaces
            .iter()
            .find(|space| space.id == session.space_id)
            .map(|space| space.title.clone())
            .unwrap_or_else(|| "Unknown Space".to_owned());
        let status = if session.archived {
            "ARCHIVED"
        } else {
            "ACTIVE"
        };
        let mut view = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .bg(rgb(BG))
            .child(
                div()
                    .px_6()
                    .pt_6()
                    .pb_4()
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(MUTED))
                            .child(format!("{scope}  /  SESSION  /  {status}")),
                    )
                    .child(
                        div()
                            .mt_2()
                            .text_2xl()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgb(INK))
                            .child(title),
                    )
                    .child(div().mt_2().text_sm().text_color(rgb(MUTED)).child(
                        "Pane layout is restored from SQLite; no view owns an Agent process.",
                    )),
            );
        if let Some(root) = root {
            view = view.child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .p_5()
                    .child(self.session_pane(root, cx)),
            );
        } else {
            view = view.child(
                div()
                    .p_6()
                    .text_color(rgb(MUTED))
                    .child("Session layout unavailable"),
            );
        }
        view
    }
}
