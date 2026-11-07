//! Top navigation and runtime refresh affordance.
use super::*;

impl Workspace {
    pub(super) fn topbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let is_loading = self.loading;
        div()
            .flex()
            .items_center()
            .justify_between()
            .h(px(58.0))
            .w_full()
            .px_5()
            .bg(rgb(WHITE))
            .border_b_1()
            .border_color(rgb(LINE))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(30.0))
                            .rounded_md()
                            .bg(rgb(INK))
                            .text_color(rgb(WHITE))
                            .text_sm()
                            .child("u"),
                    )
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_lg()
                            .text_color(rgb(INK))
                            .child("ukk"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(MUTED))
                            .child("/  Agent Workspace"),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .px_3()
                            .py_1()
                            .rounded_full()
                            .bg(rgb(SURFACE))
                            .text_xs()
                            .text_color(rgb(MUTED))
                            .child("READ-ONLY PROTOTYPE"),
                    )
                    .child(
                        div()
                            .id("reload-host")
                            .px_3()
                            .py_2()
                            .rounded_md()
                            .border_1()
                            .border_color(rgb(LINE))
                            .bg(rgb(WHITE))
                            .text_sm()
                            .text_color(rgb(INK))
                            .cursor_pointer()
                            .hover(|style| style.bg(rgb(SURFACE)))
                            .on_click(cx.listener(|this, _, _, cx| this.refresh(cx)))
                            .child(if is_loading {
                                "Refreshing…"
                            } else {
                                "↻  Refresh"
                            }),
                    ),
            )
    }
}
