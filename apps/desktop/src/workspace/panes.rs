//! Validated Session split-tree presentation and view-only controls.
use super::*;

impl Workspace {
    pub(super) fn pane_button(
        &self,
        cx: &mut Context<Self>,
        id: String,
        label: &'static str,
        aria_label: String,
        keyshortcuts: Option<&'static str>,
        action: PaneAction,
    ) -> impl IntoElement {
        div()
            .id(id)
            .role(Role::Button)
            .aria_label(aria_label)
            .when_some(keyshortcuts, |button, shortcuts| {
                button.aria_keyshortcuts(shortcuts)
            })
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
            // GPUI activates a focused role=Button element on Enter/Space key
            // up; adding another key handler here would double-apply actions.
            .on_click(cx.listener(move |this, _, _, cx| {
                if !this.mutation_busy {
                    this.pane_action(action.clone(), cx);
                }
            }))
            .child(label)
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

    pub(super) fn session_pane(&self, node: &PaneNode, cx: &mut Context<Self>) -> gpui::AnyElement {
        match node {
            PaneNode::Leaf { id, content } => {
                let (heading, description) = match content {
                    PaneContent::Activity => (
                        "Activity".to_string(),
                        "Recent commits for this Session's project root.".to_string(),
                    ),
                    PaneContent::Files => (
                        "Files".to_string(),
                        "Read-only changed files for this Session's project root.".to_string(),
                    ),
                    PaneContent::Diff => (
                        "Diff".to_string(),
                        "Read-only unified diff for this Session's project root.".to_string(),
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
                let body: gpui::AnyElement =
                    if let Some(surface) = repository::RepositorySurface::for_content(content) {
                        self.repository_surface_view(surface, id, cx)
                    } else {
                        match content {
                            PaneContent::Agent { agent_run_id } => {
                                self.unsupported_agent_pane(agent_run_id, id, cx)
                            }
                            PaneContent::Terminal { agent_run_id } => {
                                self.unsupported_terminal_pane(agent_run_id, id, cx)
                            }
                            // Unreachable today; reported instead of panicking.
                            _ => div()
                                .text_xs()
                                .text_color(rgb(MUTED))
                                .child("Unknown pane surface")
                                .into_any_element(),
                        }
                    };
                let new_content = super::pane_focus::adjacent_content(content);
                let destination = id.clone();
                let focused = self.is_focused_pane(id);
                let click_target = id.clone();
                div()
                    .id(format!("drop-target-leaf-{id}"))
                    .role(Role::Pane)
                    .aria_label(format!("{heading} pane"))
                    .aria_description(description.clone())
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .p_4()
                    .bg(rgb(WHITE))
                    .rounded_lg()
                    .border_2()
                    .border_color(rgb(if focused { ACCENT } else { LINE }))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.focus_pane_click(&click_target, cx);
                    }))
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(rgb(INK))
                                    .child(heading.clone()),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_wrap()
                                    .gap_1()
                                    .child(self.pane_button(
                                        cx,
                                        format!("{id}-new-tab"),
                                        "+ Tab",
                                        format!("Add a Tab to the {heading} pane"),
                                        Some(super::keymap::Shortcut::AddTab.help_keys()),
                                        PaneAction::TabAdd {
                                            pane_id: id.clone(),
                                            title: "Activity".into(),
                                            content: PaneContent::Activity,
                                        },
                                    ))
                                    .child(self.pane_button(
                                        cx,
                                        format!("{id}-split-right"),
                                        "Split →",
                                        "Split this pane horizontally".into(),
                                        Some(super::keymap::Shortcut::SplitPaneRight.help_keys()),
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
                                        "Split this pane vertically".into(),
                                        Some(super::keymap::Shortcut::SplitPaneDown.help_keys()),
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
                                        "Close this pane".into(),
                                        Some(super::keymap::Shortcut::ClosePane.help_keys()),
                                        PaneAction::Close {
                                            pane_id: id.clone(),
                                        },
                                    )),
                            ),
                    )
                    .child(
                        div()
                            .mt_3()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_h_0()
                            .child(body),
                    )
                    .child(
                        div()
                            .mt_2()
                            .text_xs()
                            .text_color(rgb(MUTED))
                            .child(format!("View ID: {id}")),
                    )
                    .on_drop::<DraggedPaneTab>(cx.listener(
                        move |this, payload: &DraggedPaneTab, _, cx| {
                            this.accept_dragged_tab(payload, &destination, cx);
                        },
                    ))
                    .into_any_element()
            }
            PaneNode::Tabs { id, tabs } => self.tab_group(id, tabs, cx),
            PaneNode::Split {
                id,
                axis,
                ratio,
                first,
                second,
            } => self.split_container(id, *axis, *ratio, first, second, cx),
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
        let project_root = self
            .domain
            .spaces
            .iter()
            .find(|space| space.id == session.space_id)
            .map(|space| space.project_root.clone())
            .unwrap_or_default();
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
                    ))
                    .child(div().mt_1().text_xs().text_color(rgb(MUTED)).child(
                        if project_root.trim().is_empty() {
                            "Project root not configured for this Space.".to_owned()
                        } else {
                            format!(
                                "Project root · {project_root} · read-only local Git inspection"
                            )
                        },
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
