//! Runtime-backed Agent inspector and detail panels.
use super::*;

impl Workspace {
    pub(super) fn tab_button(
        &self,
        cx: &mut Context<Self>,
        kind: DetailTab,
        label: &'static str,
    ) -> impl IntoElement {
        let selected = self.tab == kind;
        div()
            .id(label)
            .px_4()
            .py_3()
            .cursor_pointer()
            .text_sm()
            .text_color(rgb(if selected { ACCENT } else { MUTED }))
            .border_b_2()
            .border_color(rgb(if selected { ACCENT } else { WHITE }))
            .hover(|style| style.bg(rgb(SURFACE)))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.tab = kind;
                cx.notify();
            }))
            .child(label)
    }

    pub(super) fn detail(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        if let Some(id) = self.selected_session.as_deref() {
            if let Some(session) = self.domain.sessions.iter().find(|session| session.id == id) {
                return self.session_detail(session, cx).into_any_element();
            }
        }
        let selected = self.selected_terminal().cloned();
        let title = selected
            .as_ref()
            .map(|t| t.display_name())
            .unwrap_or("No agents running")
            .to_string();

        let mut body = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
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
                            .child("LOCAL MACHINE  /  AGENT WORKSPACE"),
                    )
                    .child(
                        div()
                            .mt_2()
                            .text_2xl()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgb(INK))
                            .child(title),
                    )
                    .child(
                        div()
                            .mt_2()
                            .text_sm()
                            .text_color(rgb(MUTED))
                            .child("Runtime state is sourced from herdr-core, not inferred from terminal text."),
                    ),
            )
            .child(
                div()
                    .flex()
                    .gap_1()
                    .mx_6()
                    .bg(rgb(WHITE))
                    .border_b_1()
                    .border_color(rgb(LINE))
                    .child(self.tab_button(cx, DetailTab::Overview, "Overview"))
                    .child(self.tab_button(cx, DetailTab::Runtime, "Runtime")),
            );

        if let Some(term) = selected {
            body = body.child(self.terminal_detail(&term));
        } else {
            body = body.child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .gap_3()
                    .text_color(rgb(MUTED))
                    .child("No active terminal snapshot")
                    .child(
                        div()
                            .text_sm()
                            .child("Start herdr-core and an agent, then refresh."),
                    ),
            );
        }

        body.into_any_element()
    }

    pub(super) fn terminal_detail(&self, term: &Terminal) -> impl IntoElement {
        let title = term.display_name().to_owned();
        let state = if self.online {
            term.state_label().to_owned()
        } else {
            "Last known · Host offline".to_owned()
        };
        let argv = if term.argv.is_empty() {
            "Not reported".to_owned()
        } else {
            term.argv.join(" ")
        };
        let parent = term.parent.clone().unwrap_or_else(|| "—".to_owned());
        let app = term
            .reported_app
            .clone()
            .unwrap_or_else(|| "Not reported".to_owned());
        let lifecycle = term.lifecycle.clone();
        let address = term.address.clone();
        let cwd = term.cwd.clone();

        div()
            .id("agent-detail-scroll")
            .flex()
            .flex_col()
            .flex_1()
            .p_6()
            .gap_4()
            .overflow_y_scroll()
            .child(
                div()
                    .p_5()
                    .bg(rgb(WHITE))
                    .rounded_xl()
                    .border_1()
                    .border_color(rgb(LINE))
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgb(MUTED))
                            .child("SELECTED AGENT"),
                    )
                    .child(
                        div()
                            .mt_3()
                            .text_xl()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgb(INK))
                            .child(title),
                    )
                    .child(
                        div()
                            .mt_2()
                            .text_sm()
                            .text_color(rgb(if lifecycle == "finished" { MUTED } else { ACCENT }))
                            .child(state),
                    )
                    .child(
                        div()
                            .mt_4()
                            .text_sm()
                            .text_color(rgb(MUTED))
                            .child(if self.tab == DetailTab::Overview {
                                "Terminal and provider information comes directly from the Host snapshot."
                            } else {
                                "Runtime identity and process metadata. Structured turns and approvals are not wired yet."
                            }),
                    ),
            )
            .child(
                div()
                    .p_5()
                    .bg(rgb(WHITE))
                    .rounded_xl()
                    .border_1()
                    .border_color(rgb(LINE))
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(Self::meta_row("Terminal address", address))
                    .child(Self::meta_row("Working directory", cwd))
                    .child(Self::meta_row("Command", argv))
                    .child(Self::meta_row("Lifecycle", lifecycle))
                    .child(Self::meta_row("Parent terminal", parent))
                    .child(Self::meta_row("Reported Agent", app)),
            )
            .child(
                div()
                    .p_4()
                    .rounded_lg()
                    .bg(rgb(0xeef0f8))
                    .text_color(rgb(0x666b85))
                    .text_sm()
                    .child("Coming next: persistent Sessions, structured Agent turns, Diff and multi-pane views. This prototype deliberately does not fabricate these."),
            )
    }

    pub(super) fn meta_row(name: &'static str, value: String) -> impl IntoElement {
        div()
            .flex()
            .items_start()
            .gap_4()
            .child(
                div()
                    .w(px(156.0))
                    .flex_none()
                    .text_sm()
                    .text_color(rgb(MUTED))
                    .child(name),
            )
            .child(div().flex_1().text_sm().text_color(rgb(INK)).child(value))
    }

    pub(super) fn inspector(&self) -> impl IntoElement {
        let count = self.snapshot.terminals.len();
        let running = self
            .snapshot
            .terminals
            .iter()
            .filter(|t| t.lifecycle == "live")
            .count();
        let needs_you = self
            .snapshot
            .terminals
            .iter()
            .filter(|t| t.reported_status.as_deref() == Some("blocked"))
            .count();
        let message = self.message.clone();
        div()
            .flex()
            .flex_col()
            .w(px(245.0))
            .h_full()
            .p_5()
            .gap_4()
            .bg(rgb(WHITE))
            .border_l_1()
            .border_color(rgb(LINE))
            .child(div().text_xs().font_weight(gpui::FontWeight::SEMIBOLD).text_color(rgb(MUTED)).child("RUNTIME"))
            .child(
                div()
                    .p_4()
                    .bg(rgb(SURFACE))
                    .rounded_lg()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(Self::meta_row("Visible", count.to_string()))
                    .child(Self::meta_row("Live", running.to_string()))
                    .child(Self::meta_row("Blocked", needs_you.to_string())),
            )
            .child(div().text_xs().font_weight(gpui::FontWeight::SEMIBOLD).text_color(rgb(MUTED)).child("CONNECTION"))
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(if self.online { 0x408463 } else { 0xb06e55 }))
                    .child(if self.online { "herdr-core connected" } else { "Runtime unavailable" }),
            )
            .when_some(message, |view, error| {
                view.child(
                    div()
                        .text_xs()
                        .text_color(rgb(MUTED))
                        .child(error),
                )
            })
            .child(
                div()
                    .mt_4()
                    .border_t_1()
                    .pt_4()
                    .border_color(rgb(LINE))
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child("No Agents are started or stopped by this prototype. Closing this window does not signal herdr-core."),
            )
    }
}
