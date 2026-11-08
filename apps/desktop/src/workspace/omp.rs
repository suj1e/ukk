//! omp RPC conversation surface.
//!
//! The Desktop only talks to the Host: `omp.snapshot` / `omp.wait` are
//! read-only long-polls; `omp.start` / `omp.prompt` / `omp.abort` run only
//! from an explicit user action and carry the separate agent-control token via
//! [`HostClient`]. Closing the panel, the Session or the window stops this
//! polling task and nothing else: the Host owns the omp child.
//!
//! Rendering uses typed timeline entries from the Host. There is no fixture
//! data, no approval synthesis and no terminal-text inference.

use super::*;
use gpui::KeyDownEvent;
use ukk_host_client::HostError;
use ukk_omp_client::{OmpSnapshot, OmpTimelineEntry, OmpTimelineKind, OmpTurnState};

const OMP_NOT_STARTED_CODE: i64 = -32034;
const MAX_INPUT_BYTES: usize = 8192;
/// Rendering only the newest entries keeps the GPUI tree light; the Host
/// snapshot itself is already bounded and reports its own truncation.
const VISIBLE_ENTRIES: usize = 200;

#[derive(Default)]
pub(super) struct OmpPanel {
    pub(super) open: bool,
    pub(super) session_id: Option<String>,
    pub(super) input: String,
    /// A Host mutation or start is in flight.
    pub(super) busy: bool,
    pub(super) error: Option<String>,
    pub(super) snapshot: Option<OmpSnapshot>,
    /// Incremented on close/session change to cancel the long-poll task.
    pub(super) poll_token: u64,
}

impl Workspace {
    /// Session-detail section: a real toggle plus the conversation panel.
    pub(super) fn omp_section(
        &self,
        session_id: &str,
        cwd: &str,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let open = self.omp.open && self.omp.session_id.as_deref() == Some(session_id);
        let toggle_session = session_id.to_owned();
        div()
            .flex()
            .flex_col()
            .gap_2()
            .px_6()
            .py_3()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(rgb(MUTED))
                                    .child("OMP RPC · LOCAL"),
                            )
                            .child(
                                div().text_xs().text_color(rgb(MUTED)).child(
                                    "Structured conversation from the Host-owned omp child.",
                                ),
                            ),
                    )
                    .child(self.surface_button(
                        format!("omp-toggle-{toggle_session}"),
                        if open { "Close omp RPC" } else { "omp RPC" },
                        format!(
                            "{} the local omp RPC conversation panel",
                            if open { "Close" } else { "Open" }
                        ),
                        move |this, cx| this.toggle_omp(toggle_session.clone(), cx),
                        cx,
                    )),
            )
            .when(open, |section| section.child(self.omp_panel(cwd, cx)))
            .into_any_element()
    }

    fn toggle_omp(&mut self, session_id: String, cx: &mut Context<Self>) {
        if self.omp.open && self.omp.session_id.as_deref() == Some(session_id.as_str()) {
            self.close_omp(cx);
            return;
        }
        self.omp.open = true;
        self.omp.session_id = Some(session_id);
        self.omp.input.clear();
        self.omp.busy = false;
        self.omp.error = None;
        self.omp.snapshot = None;
        self.omp.poll_token = self.omp.poll_token.wrapping_add(1);
        self.start_omp_poll(cx);
        cx.notify();
    }

    pub(super) fn close_omp(&mut self, cx: &mut Context<Self>) {
        self.omp.open = false;
        self.omp.session_id = None;
        self.omp.snapshot = None;
        self.omp.busy = false;
        self.omp.error = None;
        self.omp.input.clear();
        self.omp.poll_token = self.omp.poll_token.wrapping_add(1);
        cx.notify();
    }

    /// Poll the Host long-poll only while this panel is open. Dropping the
    /// entity (window close) ends the task; it never signals the child.
    fn start_omp_poll(&mut self, cx: &mut Context<Self>) {
        let Some(session_id) = self.omp.session_id.clone() else {
            return;
        };
        let token = self.omp.poll_token;
        cx.spawn(async move |this, cx| {
            let mut generation = 0_u64;
            loop {
                let request_session = session_id.clone();
                let result =
                    cx.background_executor()
                        .spawn(async move {
                            HostClient::from_env().omp_wait(&request_session, generation)
                        })
                        .await;
                let (keep_going, next_generation, back_off) = this
                    .update(cx, |view, cx| {
                        if !view.omp.open
                            || view.omp.poll_token != token
                            || view.omp.session_id.as_deref() != Some(session_id.as_str())
                        {
                            return (false, generation, false);
                        }
                        match result {
                            Ok(snapshot) => {
                                let generation = snapshot.generation;
                                let keep_going = snapshot.is_running();
                                if view.omp.snapshot.as_ref() != Some(&snapshot) {
                                    view.omp.snapshot = Some(snapshot);
                                    cx.notify();
                                }
                                view.omp.error = None;
                                (keep_going, generation, false)
                            }
                            Err(HostError::Remote(value))
                                if value.get("code").and_then(Value::as_i64)
                                    == Some(OMP_NOT_STARTED_CODE) =>
                            {
                                // No child for this Session yet. Stay ready
                                // for an explicit Start click.
                                view.omp.snapshot = None;
                                view.omp.error = None;
                                cx.notify();
                                (true, generation, false)
                            }
                            Err(error) => {
                                let message = error.to_string();
                                if view.omp.error.as_deref() != Some(message.as_str()) {
                                    view.omp.error = Some(message);
                                    cx.notify();
                                }
                                (true, generation, true)
                            }
                        }
                    })
                    .unwrap_or((false, generation, false));
                if !keep_going {
                    break;
                }
                generation = next_generation;
                if back_off {
                    cx.background_executor().timer(Duration::from_secs(2)).await;
                }
            }
        })
        .detach();
    }

    /// Explicit user action. Requires the agent-control capability inside
    /// `HostClient`; a missing token is a visible error, never a silent retry.
    pub(super) fn start_omp(&mut self, cwd: String, cx: &mut Context<Self>) {
        let Some(session_id) = self.omp.session_id.clone() else {
            return;
        };
        if self.omp.busy {
            return;
        }
        self.omp.busy = true;
        self.omp.error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { HostClient::from_env().omp_start(&session_id, &cwd) })
                .await;
            this.update(cx, |view, cx| {
                view.omp.busy = false;
                match result {
                    Ok(snapshot) => {
                        view.omp.snapshot = Some(snapshot);
                        view.omp.error = None;
                        // A previous poll task may have stopped because the
                        // old child exited. Rebind to the new generation.
                        view.omp.poll_token = view.omp.poll_token.wrapping_add(1);
                        view.start_omp_poll(cx);
                    }
                    Err(error) => view.omp.error = Some(error.to_string()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn send_omp_prompt(&mut self, cx: &mut Context<Self>) {
        if self.omp.busy {
            return;
        }
        let message = self.omp.input.trim().to_owned();
        if message.is_empty() {
            return;
        }
        let Some(session_id) = self.omp.session_id.clone() else {
            return;
        };
        let Some(snapshot) = self.omp.snapshot.as_ref() else {
            return;
        };
        if !snapshot.is_running()
            || matches!(
                snapshot.turn.state,
                OmpTurnState::Running | OmpTurnState::Aborting
            )
        {
            return;
        }
        self.omp.busy = true;
        self.omp.error = None;
        self.omp.input.clear();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let request_message = message.clone();
            let result = cx
                .background_executor()
                .spawn(
                    async move { HostClient::from_env().omp_prompt(&session_id, &request_message) },
                )
                .await;
            this.update(cx, |view, cx| {
                view.omp.busy = false;
                match result {
                    Ok(snapshot) => {
                        view.omp.snapshot = Some(snapshot);
                        view.omp.error = None;
                    }
                    Err(error) => {
                        // Restore the text only when the user has not typed
                        // something new while the request was in flight.
                        if view.omp.input.is_empty() {
                            view.omp.input = message;
                        }
                        view.omp.error = Some(error.to_string());
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn abort_omp(&mut self, cx: &mut Context<Self>) {
        if self.omp.busy {
            return;
        }
        let Some(session_id) = self.omp.session_id.clone() else {
            return;
        };
        self.omp.busy = true;
        self.omp.error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { HostClient::from_env().omp_abort(&session_id) })
                .await;
            this.update(cx, |view, cx| {
                view.omp.busy = false;
                match result {
                    Ok(snapshot) => {
                        view.omp.snapshot = Some(snapshot);
                        view.omp.error = None;
                    }
                    Err(error) => view.omp.error = Some(error.to_string()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Minimal composer key handling. GPUI in this app has no shared text
    /// input yet; only explicit typing, Backspace and Enter are supported.
    fn omp_input_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let modifiers = event.keystroke.modifiers;
        match event.keystroke.key.as_str() {
            "enter" => self.send_omp_prompt(cx),
            "backspace" => {
                self.omp.input.pop();
                cx.notify();
            }
            "escape" => self.close_omp(cx),
            _ => {
                if modifiers.control || modifiers.platform || modifiers.alt {
                    return;
                }
                if let Some(text) = event.keystroke.key_char.as_deref() {
                    if text.chars().any(|character| character.is_control()) {
                        return;
                    }
                    if self.omp.input.len() + text.len() <= MAX_INPUT_BYTES {
                        self.omp.input.push_str(text);
                        cx.notify();
                    }
                }
            }
        }
        cx.stop_propagation();
    }

    fn omp_panel(&self, cwd: &str, cx: &mut Context<Self>) -> gpui::AnyElement {
        let snapshot = self.omp.snapshot.as_ref();
        let running = snapshot.is_some_and(OmpSnapshot::is_running);
        let turn_running = snapshot.is_some_and(|snapshot| {
            matches!(
                snapshot.turn.state,
                OmpTurnState::Running | OmpTurnState::Aborting
            )
        });
        let state_label = match snapshot {
            None => "not started",
            Some(snapshot) => match snapshot.state.as_str() {
                "running" => "running",
                "exited" => "exited",
                other => other,
            },
        };
        let generation = snapshot.map(|snapshot| snapshot.generation).unwrap_or(0);
        let protocol = snapshot
            .map(|snapshot| snapshot.protocol_version)
            .unwrap_or(1);
        let cwd_ready = !cwd.trim().is_empty();

        let mut panel = div()
            .id("omp-panel")
            .flex()
            .flex_col()
            .gap_3()
            .mx_6()
            .mb_3()
            .p_4()
            .bg(rgb(WHITE))
            .rounded_xl()
            .border_1()
            .border_color(rgb(LINE))
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
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .px_2()
                                    .py_1()
                                    .rounded_md()
                                    .bg(rgb(SURFACE))
                                    .text_xs()
                                    .text_color(rgb(MUTED))
                                    .child("omp RPC · local"),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(if running { 0x408463 } else { 0xb06e55 }))
                                    .child(format!("{state_label} · protocol v{protocol}")),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(MUTED))
                                    .child(format!("generation {generation}")),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(MUTED))
                            .child(format!("cwd · {cwd}")),
                    ),
            );

        if let Some(error) = &self.omp.error {
            panel = panel.child(
                div()
                    .p_3()
                    .rounded_lg()
                    .bg(rgb(0xfdf2f2))
                    .text_sm()
                    .text_color(rgb(0xb03a3a))
                    .child(omp_sanitize(error)),
            );
        }

        match snapshot {
            None => {
                panel = panel
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .text_sm()
                            .text_color(rgb(INK))
                            .child(
                                "No omp RPC session has been started for this Session. \
                                 Starting one spawns a single local `omp --mode rpc \
                                 --no-session` child owned by the Host. It streams typed \
                                 messages and tool events; nothing is inferred from terminal text.",
                            )
                            .child(div().text_xs().text_color(rgb(MUTED)).child(
                                "Requires the separate agent-control capability. \
                                         Closing this panel or the window never stops the child.",
                            )),
                    )
                    .child(if cwd_ready {
                        self.omp_start_button(cwd, "Start omp session", cx)
                    } else {
                        self.omp_hint(
                            "Configure this Space's project root before starting omp; \
                             the Host needs a working directory for the child.",
                        )
                    });
            }
            Some(snapshot) => {
                panel = panel.child(self.omp_transcript(snapshot));
                panel = panel.child(self.omp_composer(snapshot, turn_running, cx));
                if !running {
                    panel = panel.child(if cwd_ready {
                        self.omp_start_button(cwd, "Start a new omp session", cx)
                    } else {
                        self.omp_hint("Configure this Space's project root before starting omp.")
                    });
                }
            }
        }

        panel
            .child(div().text_xs().text_color(rgb(MUTED)).child(
                "omp RPC v1 · Host-owned process · close does not kill · \
                         Host restart does not reattach",
            ))
            .into_any_element()
    }

    fn omp_start_button(
        &self,
        cwd: &str,
        label: &'static str,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let cwd = cwd.to_owned();
        let busy = self.omp.busy;
        let button = div()
            .id("omp-start")
            .role(Role::Button)
            .aria_label(label)
            .focusable()
            .focus_visible(|style| style.border_color(rgb(ACCENT)))
            .px_3()
            .py_2()
            .rounded_md()
            .bg(rgb(ACCENT))
            .text_sm()
            .text_color(rgb(WHITE))
            .cursor_pointer()
            .child(if busy { "Starting…" } else { label });
        if busy {
            return button.opacity(0.6).into_any_element();
        }
        button
            .on_click(cx.listener(move |this, _, _, cx| {
                this.start_omp(cwd.clone(), cx);
            }))
            .into_any_element()
    }

    fn omp_transcript(&self, snapshot: &OmpSnapshot) -> gpui::AnyElement {
        let entries = &snapshot.timeline;
        let hidden = visible_entry_start(entries.len());
        let mut transcript = div()
            .id("omp-transcript")
            .flex()
            .flex_col()
            .gap_2()
            .max_h(px(360.0))
            .overflow_y_scroll();
        if snapshot.truncated {
            transcript = transcript.child(self.omp_hint(
                "The Host bounded this conversation timeline; older entries were discarded.",
            ));
        }
        if hidden > 0 {
            transcript = transcript.child(self.omp_hint(&format!(
                "{hidden} older entries are retained by the Host but not rendered here."
            )));
        }
        if entries.is_empty() {
            transcript = transcript.child(self.omp_hint("No messages yet."));
        }
        transcript
            .children(
                entries
                    .iter()
                    .skip(hidden)
                    .map(|entry| self.omp_entry(entry)),
            )
            .into_any_element()
    }

    fn omp_entry(&self, entry: &OmpTimelineEntry) -> gpui::AnyElement {
        let (label, color) = match entry.kind {
            OmpTimelineKind::User => ("YOU", ACCENT),
            OmpTimelineKind::Assistant => ("OMP", INK),
            OmpTimelineKind::Thinking => ("THINKING", MUTED),
            OmpTimelineKind::Tool => ("TOOL", 0x408463),
            OmpTimelineKind::Notice => ("NOTICE", MUTED),
            OmpTimelineKind::Error => ("ERROR", 0xb03a3a),
            OmpTimelineKind::Status => ("SESSION", MUTED),
        };
        let text = omp_sanitize(&entry.text);
        let error_color = 0xb03a3a;
        let text_color = if entry.is_error { error_color } else { INK };
        div()
            .p_3()
            .rounded_lg()
            .bg(rgb(if entry.is_error { 0xfdf2f2 } else { SURFACE }))
            .border_1()
            .border_color(rgb(LINE))
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgb(color))
                            .child(label),
                    )
                    .when_some(entry.tool_name.clone(), |row, name| {
                        row.child(
                            div()
                                .text_xs()
                                .text_color(rgb(MUTED))
                                .child(omp_sanitize(&name)),
                        )
                    })
                    .when(entry.truncated, |row| {
                        row.child(
                            div()
                                .text_xs()
                                .text_color(rgb(MUTED))
                                .child("truncated by Host"),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .text_sm()
                    .text_color(rgb(text_color))
                    .children(if text.is_empty() {
                        vec![div().child("…").into_any_element()]
                    } else {
                        text.split('\n')
                            .map(|line| {
                                div()
                                    .min_h(px(if line.is_empty() { 6.0 } else { 0.0 }))
                                    .child(line.to_owned())
                                    .into_any_element()
                            })
                            .collect()
                    }),
            )
            .into_any_element()
    }

    fn omp_composer(
        &self,
        snapshot: &OmpSnapshot,
        turn_running: bool,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let running = snapshot.is_running();
        let disabled = !running || turn_running || self.omp.busy;
        let input = self.omp.input.clone();
        let placeholder = if !running {
            "omp is not running"
        } else if turn_running {
            "omp is working · abort to send another prompt"
        } else {
            "Ask omp… (Enter to send)"
        };
        let mut row = div()
            .id("omp-composer")
            .role(Role::TextInput)
            .aria_label("omp prompt composer")
            .track_focus(&self.omp_input_focus)
            .focusable()
            .focus_visible(|style| style.border_color(rgb(ACCENT)))
            .flex()
            .flex_col()
            .gap_2();
        row = row.child(
            div()
                .id("omp-composer-field")
                .px_3()
                .py_2()
                .min_h(px(38.0))
                .rounded_lg()
                .bg(rgb(BG))
                .border_1()
                .border_color(rgb(LINE))
                .cursor_pointer()
                .on_click(cx.listener(|this, _, window, cx| {
                    window.focus(&this.omp_input_focus, cx);
                }))
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                    this.omp_input_key(event, cx);
                }))
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(if input.is_empty() { MUTED } else { INK }))
                        .child(if input.is_empty() {
                            placeholder.to_owned()
                        } else {
                            input
                        }),
                ),
        );
        row = row.child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(self.omp_action_button(
                    "omp-send",
                    if self.omp.busy { "Sending…" } else { "Send" },
                    "Send the composed prompt to omp",
                    disabled,
                    |this, cx| this.send_omp_prompt(cx),
                    cx,
                ))
                .when(turn_running, |bar| {
                    bar.child(self.omp_action_button(
                        "omp-abort",
                        "Abort",
                        "Abort the active omp turn",
                        self.omp.busy || !running,
                        |this, cx| this.abort_omp(cx),
                        cx,
                    ))
                })
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(MUTED))
                        .child("Typing, Backspace, Enter · ⌘/Ctrl shortcuts are not captured"),
                ),
        );
        row.into_any_element()
    }

    fn omp_action_button(
        &self,
        id: &'static str,
        label: &'static str,
        aria: &'static str,
        disabled: bool,
        handler: impl Fn(&mut Self, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let button = div()
            .id(id)
            .role(Role::Button)
            .aria_label(aria)
            .focusable()
            .focus_visible(|style| style.border_color(rgb(ACCENT)))
            .px_3()
            .py_1()
            .rounded_md()
            .border_1()
            .border_color(rgb(LINE))
            .bg(rgb(WHITE))
            .text_xs()
            .text_color(rgb(ACCENT))
            .child(label);
        if disabled {
            return button.opacity(0.5).into_any_element();
        }
        button
            .cursor_pointer()
            .hover(|style| style.bg(rgb(SURFACE)))
            .on_click(cx.listener(move |this, _, _, cx| handler(this, cx)))
            .into_any_element()
    }

    fn omp_hint(&self, text: &str) -> gpui::AnyElement {
        div()
            .p_2()
            .rounded_md()
            .bg(rgb(SURFACE))
            .text_xs()
            .text_color(rgb(MUTED))
            .child(omp_sanitize(text))
            .into_any_element()
    }
}

/// Number of leading entries hidden by the view's render budget. Pure helper
/// so the bound is testable without a window.
fn visible_entry_start(len: usize) -> usize {
    len.saturating_sub(VISIBLE_ENTRIES)
}

/// Conversation text may be longer than helper output; keep newlines and tabs,
/// strip other control characters and bidi overrides, and bound the preview.
fn omp_sanitize(text: &str) -> String {
    const MAX_BYTES: usize = 8 * 1024;
    let mut output = String::with_capacity(text.len().min(MAX_BYTES));
    for character in text.chars() {
        if output.len() >= MAX_BYTES {
            output.push('…');
            break;
        }
        if (character == '\n' || character == '\t' || !character.is_control())
            && !matches!(character, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        {
            output.push(character);
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panel_starts_closed_and_unbound() {
        let panel = OmpPanel::default();
        assert!(!panel.open);
        assert!(panel.session_id.is_none());
        assert!(panel.snapshot.is_none());
        assert!(!panel.busy);
    }

    #[test]
    fn render_budget_only_hides_old_entries() {
        assert_eq!(visible_entry_start(0), 0);
        assert_eq!(visible_entry_start(10), 0);
        assert_eq!(visible_entry_start(VISIBLE_ENTRIES + 5), 5);
    }

    #[test]
    fn conversation_text_keeps_newlines_and_strips_controls() {
        let text = omp_sanitize("line one\nline two\u{7}\u{202e}tail");
        assert_eq!(text, "line one\nline twotail");
        let long = "x".repeat(8 * 1024 + 10);
        let bounded = omp_sanitize(&long);
        assert!(bounded.len() <= 8 * 1024 + 4);
        assert!(bounded.ends_with('…'));
    }
}
