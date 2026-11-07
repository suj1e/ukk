//! Explicit unsupported states for Agent conversation and terminal attach.
//!
//! R1 supplies live terminal attachment and R2 supplies structured Agent
//! events. Until those contracts are merged, these panes show the real
//! persisted identity plus a truthful "not connected" state. They never print
//! invented chat turns, tool calls, approvals or terminal output.

use super::*;

impl Workspace {
    pub(super) fn unsupported_agent_pane(
        &self,
        agent_run_id: &str,
        pane_id: &str,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let binding = self
            .domain
            .agent_runs
            .iter()
            .find(|run| run.id == agent_run_id);
        let (title, subtitle, identity) = match binding {
            Some(run) => (
                format!("Agent · {}", run.adapter),
                format!("AgentRun {}", run.id),
                format!(
                    "ukk Agent conversation surface\npane: {pane_id}\nagent run: {}\nadapter: {}\ncore: {}\nterminal: {}\nconversation: NOT CONNECTED (R1/R2)\n",
                    run.id, run.adapter, run.core_id, run.terminal_address
                ),
            ),
            None => (
                "Unresolved Agent".to_owned(),
                format!("AgentRun {agent_run_id} is not present in the workspace metadata."),
                format!(
                    "ukk Agent conversation surface\npane: {pane_id}\nagent run: {agent_run_id}\nbinding: MISSING\nconversation: NOT CONNECTED (R1/R2)\n"
                ),
            ),
        };
        let mut envelope = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .gap_2()
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(rgb(MUTED))
                                    .child("AGENT CONVERSATION · NOT CONNECTED"),
                            )
                            .child(div().text_sm().text_color(rgb(INK)).child(title))
                            .child(div().text_xs().text_color(rgb(MUTED)).child(subtitle)),
                    )
                    .child(self.surface_button(
                        format!("{pane_id}-agent-copy"),
                        "Copy identity",
                        "Copy the Agent runtime identity".into(),
                        move |this, cx| this.copy_repository_diagnostics(identity.clone(), cx),
                        cx,
                    )),
            )
            .child(
                div()
                    .p_3()
                    .rounded_md()
                    .bg(rgb(0xfff4e5))
                    .text_xs()
                    .text_color(rgb(0x7a4b1e))
                    .child("Structured Agent events are not part of this build. The Host conversation adapter (R1/R2) is still in progress, so this pane renders no messages, tool calls, progress or approvals. Nothing here is synthesized from terminal text or fallback data."),
            );

        if let Some(run) = binding {
            envelope = envelope.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .p_3()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(LINE))
                    .bg(rgb(WHITE))
                    .child(Self::meta_row("AgentRun", run.id.clone()))
                    .child(Self::meta_row("Adapter", run.adapter.clone()))
                    .child(Self::meta_row("herdr-core", run.core_id.clone()))
                    .child(Self::meta_row(
                        "Terminal address",
                        run.terminal_address.clone(),
                    ))
                    .child(Self::meta_row(
                        "Parent run",
                        run.parent_run_id.clone().unwrap_or_else(|| "—".into()),
                    )),
            );
        }
        envelope
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child("The terminal process is owned by herdr-core. This pane never starts, stops or signals it."),
            )
            .into_any_element()
    }

    pub(super) fn unsupported_terminal_pane(
        &self,
        agent_run_id: &str,
        pane_id: &str,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let binding = self
            .domain
            .agent_runs
            .iter()
            .find(|run| run.id == agent_run_id);
        let (title, subtitle, identity) = match binding {
            Some(run) => (
                "Terminal · not attached".to_owned(),
                format!("{} · {}", run.core_id, run.terminal_address),
                format!(
                    "ukk terminal surface\npane: {pane_id}\nagent run: {}\ncore: {}\nterminal: {}\nattach: NOT CONNECTED (R1)\n",
                    run.id, run.core_id, run.terminal_address
                ),
            ),
            None => (
                "Unresolved Terminal".to_owned(),
                format!("AgentRun {agent_run_id} is not present in the workspace metadata."),
                format!(
                    "ukk terminal surface\npane: {pane_id}\nagent run: {agent_run_id}\nbinding: MISSING\nattach: NOT CONNECTED (R1)\n"
                ),
            ),
        };
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .gap_2()
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(rgb(MUTED))
                                    .child("TERMINAL ATTACH · NOT CONNECTED"),
                            )
                            .child(div().text_sm().text_color(rgb(INK)).child(title))
                            .child(div().text_xs().text_color(rgb(MUTED)).child(subtitle)),
                    )
                    .child(self.surface_button(
                        format!("{pane_id}-terminal-copy"),
                        "Copy identity",
                        "Copy the terminal runtime identity".into(),
                        move |this, cx| this.copy_repository_diagnostics(identity.clone(), cx),
                        cx,
                    )),
            )
            .child(
                div()
                    .p_3()
                    .rounded_md()
                    .bg(rgb(0xfff4e5))
                    .text_xs()
                    .text_color(rgb(0x7a4b1e))
                    .child("Live PTY framing and attach are supplied by R1 and are not wired into this build. This pane never fabricates terminal output, keystrokes or Agent state, and it never signals the herdr-core process."),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child("Runtime status from the Host snapshot remains authoritative in the sidebar and inspector."),
            )
            .into_any_element()
    }
}
