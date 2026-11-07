//! Space, Session and dynamic Agent tree navigation.
use super::*;

impl Workspace {
    pub(super) fn space_row(
        &self,
        space: &ukk_domain::Space,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let id = space.id.clone();
        let title = space.title.clone();
        let active = self.selected_space.as_deref() == Some(id.as_str());
        div()
            .id(format!("space-{id}"))
            .px_3()
            .py_2()
            .rounded_lg()
            .border_1()
            .border_color(rgb(0x00000000))
            .cursor_pointer()
            .bg(rgb(if active { 0xe9ebf7 } else { WHITE }))
            .hover(|style| style.bg(rgb(SURFACE)))
            .role(Role::Button)
            .aria_label(format!("Space {title}"))
            .aria_selected(active)
            .focusable()
            .focus_visible(|style| style.border_color(rgb(ACCENT)))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.selected_space = Some(id.clone());
                let first = this
                    .domain
                    .sessions
                    .iter()
                    .find(|s| s.space_id == id && !s.archived)
                    .map(|s| s.id.clone());
                match first {
                    Some(session_id) => this.select_session(session_id, cx),
                    None => {
                        this.cancel_split_drag(cx);
                        this.selected_session = None;
                        this.selected = None;
                        cx.notify();
                    }
                }
            }))
            .child(div().text_sm().text_color(rgb(INK)).child(title))
    }

    pub(super) fn session_row(
        &self,
        session: &Session,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let active = self.selected_session.as_deref() == Some(session.id.as_str());
        let id = session.id.clone();
        let title = session.title.clone();
        div()
            .id(format!("session-{id}"))
            .px_3()
            .py_2()
            .rounded_lg()
            .border_1()
            .border_color(rgb(0x00000000))
            .cursor_pointer()
            .bg(rgb(if active { 0xe9ebf7 } else { WHITE }))
            .hover(|style| style.bg(rgb(SURFACE)))
            .role(Role::Button)
            .aria_label(format!(
                "Session {title}, {}",
                if session.archived {
                    "archived"
                } else {
                    "active"
                }
            ))
            .aria_selected(active)
            .focusable()
            .focus_visible(|style| style.border_color(rgb(ACCENT)))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.select_session(id.clone(), cx);
            }))
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(if session.archived { MUTED } else { INK }))
                    .child(title),
            )
    }

    /// Render a terminal from the live Host snapshot; display only.
    pub(super) fn terminal_row(
        &self,
        terminal: &Terminal,
        depth: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let active = self.selected.as_deref() == Some(terminal.address.as_str());
        let address = terminal.address.clone();
        let display_name = if depth > 0 {
            format!("↳ {}", terminal.display_name())
        } else {
            terminal.display_name().to_owned()
        };
        let status = if self.online {
            terminal.state_label().to_owned()
        } else {
            "Last known · Host offline".to_owned()
        };

        div()
            .id(format!("terminal-{}", terminal.address))
            .flex()
            .flex_col()
            .gap_1()
            .w_full()
            .px_3()
            .py_3()
            .rounded_lg()
            .border_1()
            .border_color(rgb(0x00000000))
            .cursor_pointer()
            .bg(rgb(if active { 0xe9ebf7 } else { WHITE }))
            .hover(|style| style.bg(rgb(if active { 0xe9ebf7 } else { SURFACE })))
            .role(Role::Button)
            .aria_label(format!("Agent terminal {display_name}, {status}"))
            .aria_selected(active)
            .focusable()
            .focus_visible(|style| style.border_color(rgb(ACCENT)))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.cancel_split_drag(cx);
                this.selected = Some(address.clone());
                this.selected_session = None;
                this.tab = DetailTab::Overview;
                cx.notify();
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().size(px(7.0)).rounded_full().bg(rgb(
                        if !self.online || terminal.lifecycle == "finished" {
                            0xbfc3cc
                        } else if terminal.reported_status.as_deref() == Some("blocked") {
                            0xe4a84e
                        } else if terminal.reported_status.as_deref() == Some("working") {
                            0x58ab83
                        } else {
                            0xa5aab6
                        },
                    )))
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgb(INK))
                            .child(display_name),
                    ),
            )
            .child(
                div()
                    .pl(px(15.0))
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child(status),
            )
    }

    pub(super) fn sidebar(&self, width: f32, cx: &mut Context<Self>) -> impl IntoElement {
        let entries: Vec<(Terminal, usize)> = self
            .snapshot
            .hierarchy()
            .into_iter()
            .map(|(t, depth)| (t.clone(), depth))
            .collect();
        let sessions: Vec<Session> = self
            .domain
            .sessions
            .iter()
            .filter(|session| {
                !session.archived
                    && self
                        .selected_space
                        .as_ref()
                        .is_none_or(|space| &session.space_id == space)
            })
            .cloned()
            .collect();
        let spaces = self.domain.spaces.clone();
        div()
            .id("workspace-sidebar")
            .role(Role::Navigation)
            .aria_label("Spaces, Sessions and Agent terminals")
            .flex()
            .flex_col()
            .w(px(width))
            .h_full()
            .min_w_0()
            .p_4()
            .bg(rgb(WHITE))
            .border_r_1()
            .border_color(rgb(LINE))
            .child(
                div()
                    .text_xs()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(rgb(MUTED))
                    .mb_3()
                    .child("SPACES"),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_3()
                    .rounded_lg()
                    .bg(rgb(SURFACE))
                    .child(
                        div()
                            .text_color(rgb(ACCENT))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("⌘"),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgb(INK))
                            .child("Local machine"),
                    ),
            )
            .child(
                div()
                    .mt_4()
                    .text_xs()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(rgb(MUTED))
                    .child("PROJECT SPACES"),
            )
            .children(spaces.iter().map(|space| self.space_row(space, cx)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .mt_5()
                    .mb_2()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgb(MUTED))
                            .child("SESSIONS"),
                    )
                    .child(
                        div()
                            .id("new-session")
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .border_1()
                            .border_color(rgb(0x00000000))
                            .bg(rgb(SURFACE))
                            .text_xs()
                            .text_color(rgb(ACCENT))
                            .cursor_pointer()
                            .hover(|style| style.bg(rgb(0xe9ebf7)))
                            .role(Role::Button)
                            .aria_label("Create a new Session")
                            .focusable()
                            .focus_visible(|style| style.border_color(rgb(ACCENT)))
                            .on_click(cx.listener(|this, _, _, cx| this.create_session(cx)))
                            .child(if self.mutation_busy {
                                "Saving…"
                            } else {
                                "+ New"
                            }),
                    ),
            )
            .children(sessions.iter().map(|session| self.session_row(session, cx)))
            .when_some(self.mutation_error.clone(), |element, message| {
                element.child(
                    div()
                        .mt_2()
                        .px_3()
                        .py_2()
                        .rounded_md()
                        .bg(rgb(0xffefeb))
                        .text_xs()
                        .text_color(rgb(0xa14b32))
                        .child(message),
                )
            })
            .when_some(self.workspace_sync_error.clone(), |element, message| {
                element.child(
                    div()
                        .mt_2()
                        .px_3()
                        .py_2()
                        .rounded_md()
                        .bg(rgb(0xffefeb))
                        .text_xs()
                        .text_color(rgb(0xa14b32))
                        .child(message),
                )
            })
            .child(
                div()
                    .flex()
                    .justify_between()
                    .items_center()
                    .mt_6()
                    .mb_3()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgb(MUTED))
                            .child("AGENTS & TERMINALS"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(MUTED))
                            .child(entries.len().to_string()),
                    ),
            )
            .child(
                div()
                    .id("agent-tree-scroll")
                    .flex_1()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .overflow_y_scroll()
                    .children(
                        entries
                            .iter()
                            .map(|(term, depth)| self.terminal_row(term, *depth, cx)),
                    ),
            )
            .child(
                div()
                    .border_t_1()
                    .border_color(rgb(LINE))
                    .pt_3()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child(if self.online {
                        "●  herdr-core connected"
                    } else {
                        "○  Runtime offline / reconnecting"
                    }),
            )
    }
}
