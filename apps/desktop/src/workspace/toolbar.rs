//! Top navigation and runtime refresh affordance.
use super::*;

impl Workspace {
    pub(super) fn topbar(&self, show_badge: bool, cx: &mut Context<Self>) -> impl IntoElement {
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
                    .when(show_badge, |row| {
                        row.child(
                            div()
                                .px_3()
                                .py_1()
                                .rounded_full()
                                .bg(rgb(SURFACE))
                                .text_xs()
                                .text_color(rgb(MUTED))
                                .child("READ-ONLY PROTOTYPE"),
                        )
                    })
                    .child(
                        div()
                            .id("reload-host")
                            .role(Role::Button)
                            .aria_label("Refresh workspace and layout")
                            .aria_keyshortcuts(super::keymap::Shortcut::ReloadLayout.help_keys())
                            .focusable()
                            .focus_visible(|style| style.border_color(rgb(ACCENT)))
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
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.refresh(cx);
                                this.reload_repository_surfaces(cx);
                            }))
                            .child(if is_loading {
                                "Refreshing…"
                            } else {
                                "↻  Refresh"
                            }),
                    ),
            )
    }
}

impl Workspace {
    /// One visible status strip for stale CAS conflicts and local failures.
    /// A conflict always offers Reload; retrying the rejected intent is a
    /// separate, explicit button and never happens automatically.
    pub(super) fn status_banner(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        #[derive(Clone, Copy)]
        enum Kind {
            Conflict,
            Failure,
            Sync,
            Local,
        }
        let (message, kind, retry) = if let Some(conflict) = &self.conflict {
            (
                conflict.message.clone(),
                Kind::Conflict,
                conflict.action.is_some(),
            )
        } else if let Some(error) = &self.mutation_error {
            (error.clone(), Kind::Failure, false)
        } else if let Some(error) = &self.workspace_sync_error {
            (error.clone(), Kind::Sync, false)
        } else {
            let error = self.local_focus_status()?;
            (error, Kind::Local, false)
        };
        let aria_label = match kind {
            Kind::Conflict => format!("Layout conflict: {message}"),
            Kind::Failure => format!("Workspace change failed: {message}"),
            Kind::Sync => format!("Workspace sync problem: {message}"),
            Kind::Local => format!("Local focus is not persisted: {message}"),
        };
        let row = div()
            .id("workspace-status")
            .role(Role::Alert)
            .aria_label(aria_label)
            .flex()
            .flex_wrap()
            .items_center()
            .justify_between()
            .gap_3()
            .px_5()
            .py_2()
            .bg(rgb(0xfff4e5))
            .text_xs()
            .text_color(rgb(0x7a4b1e));
        let mut actions = div().flex().items_center().gap_2();
        if !matches!(kind, Kind::Local) {
            actions = actions.child(
                div()
                    .id("status-reload")
                    .role(Role::Button)
                    .aria_label("Reload the authoritative layout")
                    .focusable()
                    .focus_visible(|style| style.border_color(rgb(ACCENT)))
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(0xd9b48a))
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, _, cx| this.refresh(cx)))
                    .child("Reload layout"),
            );
        }
        if retry {
            actions = actions.child(
                div()
                    .id("status-retry")
                    .role(Role::Button)
                    .aria_label("Reload, then retry the rejected change once")
                    .focusable()
                    .focus_visible(|style| style.border_color(rgb(ACCENT)))
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .bg(rgb(WHITE))
                    .border_1()
                    .border_color(rgb(0xd9b48a))
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, _, cx| this.retry_conflict(cx)))
                    .child("Retry change"),
            );
        }
        if self.conflict.is_some() {
            actions = actions.child(
                div()
                    .id("status-dismiss")
                    .role(Role::Button)
                    .aria_label("Dismiss the layout conflict notice")
                    .focusable()
                    .focus_visible(|style| style.border_color(rgb(ACCENT)))
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(0x00000000))
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, _, cx| this.dismiss_conflict(cx)))
                    .child("Dismiss"),
            );
        }
        Some(
            row.child(div().flex_1().child(message))
                .child(actions)
                .into_any_element(),
        )
    }
}
