//! Explicit AgentRun binding of an existing Host terminal to a Session.
//!
//! The form is local UI state until an explicit confirmation. Submission goes
//! through the capability-gated `workspace.agent.bind` mutation, which the
//! Host verifies against its live herdr-core snapshot. Nothing here starts,
//! attaches to, interrupts or stops a terminal process, and selecting a
//! terminal or refreshing the workspace never opens or submits this form.

use super::*;
use gpui::KeyDownEvent;
use ukk_domain::AgentRun;
use ukk_host_client::HostError;

/// The Store trims the adapter and bounds every field at 200 characters. The
/// Desktop uses a smaller bound for a readable confirmation.
pub(super) const MAX_ADAPTER_CHARS: usize = 64;

/// Why the bind action is unavailable. `Ready` is the only state that offers
/// the confirmation form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BindAvailability {
    Ready,
    HostOffline,
    CoreUnknown,
    TerminalFinished,
    AlreadyBound,
    NoSessions,
}

/// Pure state derivation so the rules are testable without a GPUI window.
pub(super) fn bind_availability(
    online: bool,
    core_id: Option<&str>,
    terminal_lifecycle: &str,
    already_bound: bool,
    has_sessions: bool,
) -> BindAvailability {
    if !online {
        BindAvailability::HostOffline
    } else if core_id.is_none_or(str::is_empty) {
        BindAvailability::CoreUnknown
    } else if terminal_lifecycle != "live" {
        BindAvailability::TerminalFinished
    } else if already_bound {
        BindAvailability::AlreadyBound
    } else if !has_sessions {
        BindAvailability::NoSessions
    } else {
        BindAvailability::Ready
    }
}

/// The exact terminal identity shown in the confirmation. It is re-checked by
/// the Host; the Desktop never sends a guessed terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AgentBindTarget {
    pub(super) core_id: String,
    pub(super) terminal_address: String,
}

/// Local form state. `request_id` is minted when the form opens and reused by
/// every retry of the same confirmation, so the Host can deduplicate it.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct AgentBindEditor {
    pub(super) target: Option<AgentBindTarget>,
    pub(super) session_id: Option<String>,
    pub(super) adapter: String,
    pub(super) request_id: String,
    pub(super) error: Option<String>,
}

impl AgentBindEditor {
    pub(super) fn open(
        &mut self,
        core_id: &str,
        terminal_address: &str,
        default_adapter: Option<&str>,
    ) {
        *self = Self {
            target: Some(AgentBindTarget {
                core_id: core_id.to_owned(),
                terminal_address: terminal_address.to_owned(),
            }),
            session_id: None,
            adapter: default_adapter
                .filter(|adapter| adapter_valid(adapter))
                .unwrap_or_default()
                .to_owned(),
            request_id: new_id(),
            error: None,
        };
    }

    pub(super) fn close(&mut self) {
        *self = Self::default();
    }

    pub(super) fn is_open(&self) -> bool {
        self.target.is_some()
    }

    pub(super) fn is_open_for(&self, core_id: &str, terminal_address: &str) -> bool {
        self.target.as_ref().is_some_and(|target| {
            target.core_id == core_id && target.terminal_address == terminal_address
        })
    }

    pub(super) fn select_session(&mut self, session_id: &str) {
        if self.session_id.as_deref() == Some(session_id) {
            return;
        }
        self.session_id = Some(session_id.to_owned());
        // A different Session is a different deliberate action. The old
        // request id belongs to the previous confirmation only.
        self.request_id = new_id();
    }

    pub(super) fn backspace(&mut self) -> bool {
        self.adapter.pop().is_some()
    }

    /// Bounds input before the Host sees it; the Host remains authoritative.
    pub(super) fn insert(&mut self, text: &str) -> bool {
        if text.is_empty() || text.chars().any(char::is_control) {
            return false;
        }
        if self.adapter.chars().count() + text.chars().count() > MAX_ADAPTER_CHARS {
            return false;
        }
        self.adapter.push_str(text);
        true
    }

    pub(super) fn can_confirm(&self) -> bool {
        self.target.is_some() && self.session_id.is_some() && adapter_valid(&self.adapter)
    }

    /// The exact mutation payload. `None` means the form is incomplete and no
    /// request may be sent.
    pub(super) fn payload(&self) -> Option<Value> {
        let target = self.target.as_ref()?;
        let session_id = self.session_id.as_deref()?;
        if !adapter_valid(&self.adapter) {
            return None;
        }
        Some(json!({
            "request_id": self.request_id,
            "session_id": session_id,
            "core_id": target.core_id,
            "terminal_address": target.terminal_address,
            "adapter": self.adapter.trim(),
        }))
    }
}

pub(super) fn adapter_valid(adapter: &str) -> bool {
    let trimmed = adapter.trim();
    let length = trimmed.chars().count();
    (1..=MAX_ADAPTER_CHARS).contains(&length) && !adapter.chars().any(char::is_control)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum AgentBindKey {
    Confirm,
    Cancel,
    Backspace,
    Insert(String),
    Ignore,
}

pub(super) fn agent_bind_key(key: &str, key_char: Option<&str>, modifier: bool) -> AgentBindKey {
    match key {
        "enter" | "return" => AgentBindKey::Confirm,
        "escape" => AgentBindKey::Cancel,
        "backspace" if !modifier => AgentBindKey::Backspace,
        _ if modifier => AgentBindKey::Ignore,
        _ => match key_char {
            Some(text) if !text.is_empty() && !text.chars().any(char::is_control) => {
                AgentBindKey::Insert(text.to_owned())
            }
            _ => AgentBindKey::Ignore,
        },
    }
}

/// Truthful user-facing text for a Host rejection. Capability and conflict
/// states stay worded as such; no error is turned into a success.
pub(super) fn bind_error_text(error: &HostError) -> String {
    let HostError::Remote(value) = error else {
        return error.to_string();
    };
    let message = value
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("the Host rejected this binding");
    match value.get("code").and_then(Value::as_i64) {
        Some(-32031) => format!("Conflict: {message}. The terminal or request is already bound."),
        Some(-32045) => format!("Core mismatch: {message}"),
        Some(-32034) => format!("Terminal not verified: {message}"),
        Some(-32061) => format!("Ambiguous terminal: {message}"),
        Some(-32040) => format!("Cannot verify the live terminal: {message}"),
        _ => message.to_owned(),
    }
}

impl Workspace {
    /// Open the confirmation only from the explicit button. Selection and
    /// refresh never call this.
    pub(super) fn open_agent_bind(&mut self, cx: &mut Context<Self>) {
        if self.mutation_busy {
            return;
        }
        let Some(term) = self.selected_terminal().cloned() else {
            return;
        };
        let Some(core_id) = self.core_id.clone().filter(|core| !core.is_empty()) else {
            return;
        };
        let existing = self.bound_agent_run(&core_id, &term.address).is_some();
        let has_sessions = self.domain.sessions.iter().any(|session| !session.archived);
        if bind_availability(
            self.online,
            Some(&core_id),
            &term.lifecycle,
            existing,
            has_sessions,
        ) != BindAvailability::Ready
        {
            return;
        }
        let default_adapter = term.reported_app.as_deref();
        self.agent_bind
            .open(&core_id, &term.address, default_adapter);
        self.agent_bind_notice = None;
        cx.notify();
    }

    pub(super) fn cancel_agent_bind(&mut self, cx: &mut Context<Self>) {
        self.agent_bind.close();
        cx.notify();
    }

    pub(super) fn select_agent_bind_session(&mut self, session_id: String, cx: &mut Context<Self>) {
        if !self
            .domain
            .sessions
            .iter()
            .any(|session| session.id == session_id && !session.archived)
        {
            return;
        }
        self.agent_bind.select_session(&session_id);
        self.agent_bind.error = None;
        cx.notify();
    }

    pub(super) fn agent_bind_adapter_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        let modifier = event.keystroke.modifiers.control
            || event.keystroke.modifiers.alt
            || event.keystroke.modifiers.platform;
        match agent_bind_key(key, event.keystroke.key_char.as_deref(), modifier) {
            AgentBindKey::Confirm => self.confirm_agent_bind(cx),
            AgentBindKey::Cancel => self.cancel_agent_bind(cx),
            AgentBindKey::Backspace => {
                if self.agent_bind.backspace() {
                    self.agent_bind.error = None;
                    cx.notify();
                }
            }
            AgentBindKey::Insert(text) => {
                if self.agent_bind.insert(&text) {
                    self.agent_bind.error = None;
                    cx.notify();
                }
            }
            AgentBindKey::Ignore => return,
        }
        cx.stop_propagation();
    }

    /// One explicit submit path for the button and the adapter field. The
    /// form is never submitted implicitly.
    pub(super) fn confirm_agent_bind(&mut self, cx: &mut Context<Self>) {
        if self.mutation_busy || !self.agent_bind.is_open() {
            return;
        }
        if self.agent_bind.session_id.is_none() {
            self.agent_bind.error = Some("Select a Session before binding.".into());
            cx.notify();
            return;
        }
        if !adapter_valid(&self.agent_bind.adapter) {
            self.agent_bind.error =
                Some("Enter an adapter name (1–64 printable characters).".into());
            cx.notify();
            return;
        }
        let Some(params) = self.agent_bind.payload() else {
            self.agent_bind.error = Some("The binding is incomplete.".into());
            cx.notify();
            return;
        };
        self.mutate(
            "workspace.agent.bind",
            params,
            MutationIntent::BoundAgentRun,
            cx,
        );
    }

    pub(super) fn bound_agent_run(&self, core_id: &str, address: &str) -> Option<&AgentRun> {
        self.domain
            .agent_runs
            .iter()
            .find(|run| run.core_id == core_id && run.terminal_address == address)
    }

    fn bind_session_title(&self, session_id: &str) -> String {
        self.domain
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .map(|session| session.title.clone())
            .unwrap_or_else(|| "Unknown Session".to_owned())
    }

    /// Durable Session binding card in the selected terminal detail view.
    /// It is metadata only and has no terminal lifecycle action.
    pub(super) fn agent_bind_card(
        &self,
        term: &Terminal,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let existing = self
            .core_id
            .as_deref()
            .and_then(|core_id| self.bound_agent_run(core_id, &term.address));
        let has_sessions = self.domain.sessions.iter().any(|session| !session.archived);
        let availability = bind_availability(
            self.online,
            self.core_id.as_deref(),
            &term.lifecycle,
            existing.is_some(),
            has_sessions,
        );

        let mut card = div()
            .id("agent-bind-card")
            .role(Role::Group)
            .aria_label("Terminal Session binding")
            .p_5()
            .bg(rgb(WHITE))
            .rounded_xl()
            .border_1()
            .border_color(rgb(LINE))
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .text_xs()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(rgb(MUTED))
                    .child("SESSION BINDING"),
            );
        if let Some(notice) = self.agent_bind_notice.clone() {
            card = card.child(
                div()
                    .id("agent-bind-notice")
                    .role(Role::Status)
                    .aria_label(format!("Terminal binding status: {notice}"))
                    .text_xs()
                    .text_color(rgb(0x408463))
                    .child(notice),
            );
        }

        if let Some(run) = existing {
            let title = self.bind_session_title(&run.session_id);
            card = card
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(INK))
                        .child(format!("Bound to {title} · AgentRun {}", run.id)),
                )
                .child(Self::meta_row("herdr-core", run.core_id.clone()))
                .child(Self::meta_row("Terminal", run.terminal_address.clone()))
                .child(Self::meta_row("Adapter", run.adapter.clone()))
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(MUTED))
                        .child("Durable metadata only. The terminal process belongs to herdr-core; this view never starts, stops or attaches to it."),
                );
            return card.into_any_element();
        }

        let reason = match availability {
            BindAvailability::Ready => None,
            BindAvailability::HostOffline => {
                Some("Host offline or herdr-core unreachable; binding needs a live snapshot.")
            }
            BindAvailability::CoreUnknown => {
                Some("herdr-core identity is not available yet; refresh after it connects.")
            }
            BindAvailability::TerminalFinished => {
                Some("This terminal is finished; only a live terminal can be bound.")
            }
            BindAvailability::AlreadyBound => {
                Some("This terminal is already bound to an AgentRun.")
            }
            BindAvailability::NoSessions => Some("Create a Session before binding this terminal."),
        };
        if let Some(reason) = reason {
            return card
                .child(
                    div()
                        .id("agent-bind-unavailable")
                        .role(Role::Alert)
                        .aria_label(format!("Binding unavailable: {reason}"))
                        .text_sm()
                        .text_color(rgb(MUTED))
                        .child(reason),
                )
                .into_any_element();
        }

        let adapter_focus = self.agent_bind_adapter_focus.clone();
        if self
            .core_id
            .as_deref()
            .is_some_and(|core_id| self.agent_bind.is_open_for(core_id, &term.address))
        {
            return self.agent_bind_confirmation(&adapter_focus, cx);
        }

        card.child(
            div()
                .text_sm()
                .text_color(rgb(MUTED))
                .child("This terminal is not bound to a Session. Binding records a durable AgentRun; it never starts or signals the process."),
        )
        .child(
            div()
                .id("agent-bind-open")
                .role(Role::Button)
                .aria_label(format!(
                    "Bind terminal {} to a Session",
                    term.display_name()
                ))
                .focusable()
                .focus_visible(|style| style.border_color(rgb(ACCENT)))
                .px_3()
                .py_1()
                .rounded_md()
                .bg(rgb(ACCENT))
                .text_xs()
                .text_color(rgb(WHITE))
                .cursor_pointer()
                .hover(|style| style.opacity(0.9))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.open_agent_bind(cx);
                }))
                .child("Bind terminal to this Session…"),
        )
        .into_any_element()
    }

    fn agent_bind_confirmation(
        &self,
        adapter_focus: &gpui::FocusHandle,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let editor = &self.agent_bind;
        let Some(target) = editor.target.as_ref() else {
            return div().into_any_element();
        };
        let busy = self.mutation_busy;
        let can_confirm = editor.can_confirm() && !busy;
        let adapter_empty = editor.adapter.is_empty();
        let mut form = div()
            .id("agent-bind-confirm")
            .role(Role::Group)
            .aria_label(format!(
                "Confirm binding terminal {} on herdr-core {}",
                target.terminal_address, target.core_id
            ))
            .flex()
            .flex_col()
            .gap_2()
            .mt_2()
            .p_3()
            .rounded_lg()
            .bg(rgb(SURFACE))
            .border_1()
            .border_color(rgb(LINE))
            .child(
                div()
                    .text_xs()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(rgb(MUTED))
                    .child("CONFIRM BINDING"),
            )
            .child(Self::meta_row("herdr-core", target.core_id.clone()))
            .child(Self::meta_row("Terminal", target.terminal_address.clone()))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().text_xs().text_color(rgb(MUTED)).child("Session"))
                    .child(
                        div().flex().flex_wrap().gap_1().children(
                            self.domain
                                .sessions
                                .iter()
                                .filter(|session| !session.archived)
                                .map(|session| {
                                    let selected =
                                        editor.session_id.as_deref() == Some(&session.id);
                                    let session_id = session.id.clone();
                                    div()
                                        .id(format!("agent-bind-session-{}", session.id))
                                        .role(Role::Button)
                                        .aria_label(format!("Bind to Session {}", session.title))
                                        .aria_selected(selected)
                                        .focusable()
                                        .px_2()
                                        .py_1()
                                        .rounded_md()
                                        .border_1()
                                        .border_color(rgb(if selected { ACCENT } else { LINE }))
                                        .bg(rgb(if selected { ACCENT } else { WHITE }))
                                        .text_xs()
                                        .text_color(rgb(if selected { WHITE } else { INK }))
                                        .cursor_pointer()
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.select_agent_bind_session(session_id.clone(), cx);
                                        }))
                                        .child(session.title.clone())
                                }),
                        ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().text_xs().text_color(rgb(MUTED)).child("Adapter"))
                    .child(
                        div()
                            .id("agent-bind-adapter")
                            .role(Role::TextInput)
                            .aria_label("Agent adapter name")
                            .track_focus(adapter_focus)
                            .focusable()
                            .focus_visible(|style| style.border_color(rgb(ACCENT)))
                            .px_2()
                            .py_1()
                            .min_h(px(30.0))
                            .rounded_md()
                            .bg(rgb(WHITE))
                            .border_1()
                            .border_color(rgb(LINE))
                            .cursor_pointer()
                            .text_sm()
                            .text_color(rgb(if adapter_empty { MUTED } else { INK }))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                window.focus(&this.agent_bind_adapter_focus, cx);
                            }))
                            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                                this.agent_bind_adapter_key(event, cx);
                            }))
                            .child(if adapter_empty {
                                "e.g. codex".to_owned()
                            } else {
                                editor.adapter.clone()
                            }),
                    ),
            );

        form = form.child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .id("agent-bind-confirm-button")
                        .role(Role::Button)
                        .aria_label("Confirm binding this terminal to the selected Session")
                        .focusable()
                        .focus_visible(|style| style.border_color(rgb(ACCENT)))
                        .px_3()
                        .py_1()
                        .rounded_md()
                        .border_1()
                        .border_color(rgb(LINE))
                        .bg(rgb(ACCENT))
                        .text_xs()
                        .text_color(rgb(WHITE))
                        .cursor_pointer()
                        .when(!can_confirm, |element| element.opacity(0.6))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if !can_confirm {
                                return;
                            }
                            this.confirm_agent_bind(cx);
                        }))
                        .child(if busy { "Binding…" } else { "Confirm bind" }),
                )
                .child(
                    div()
                        .id("agent-bind-cancel")
                        .role(Role::Button)
                        .aria_label("Cancel terminal binding")
                        .focusable()
                        .focus_visible(|style| style.border_color(rgb(ACCENT)))
                        .px_3()
                        .py_1()
                        .rounded_md()
                        .border_1()
                        .border_color(rgb(LINE))
                        .bg(rgb(WHITE))
                        .text_xs()
                        .text_color(rgb(INK))
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.cancel_agent_bind(cx);
                        }))
                        .child("Cancel"),
                ),
        );

        if let Some(error) = editor.error.clone() {
            form = form.child(
                div()
                    .id("agent-bind-error")
                    .role(Role::Alert)
                    .aria_label(format!("Terminal binding failed: {error}"))
                    .text_xs()
                    .text_color(rgb(0xa14b32))
                    .child(error),
            );
        }
        form.into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn availability_is_truthful_and_never_ready_for_unverified_states() {
        assert_eq!(
            bind_availability(false, Some("core"), "live", false, true),
            BindAvailability::HostOffline
        );
        assert_eq!(
            bind_availability(true, None, "live", false, true),
            BindAvailability::CoreUnknown
        );
        assert_eq!(
            bind_availability(true, Some(""), "live", false, true),
            BindAvailability::CoreUnknown
        );
        assert_eq!(
            bind_availability(true, Some("core"), "finished", false, true),
            BindAvailability::TerminalFinished
        );
        assert_eq!(
            bind_availability(true, Some("core"), "live", true, true),
            BindAvailability::AlreadyBound
        );
        assert_eq!(
            bind_availability(true, Some("core"), "live", false, false),
            BindAvailability::NoSessions
        );
        assert_eq!(
            bind_availability(true, Some("core"), "live", false, true),
            BindAvailability::Ready
        );
    }

    #[test]
    fn editor_is_closed_by_default_and_requires_an_explicit_open() {
        let editor = AgentBindEditor::default();
        assert!(!editor.is_open());
        assert!(!editor.can_confirm());
        assert!(editor.payload().is_none());
    }

    #[test]
    fn payload_is_exact_and_request_id_is_stable_across_retries() {
        let mut editor = AgentBindEditor::default();
        editor.open("core-1", "t7", Some("codex"));
        assert!(editor.is_open());
        // No Session is selected implicitly, even though the adapter is set.
        assert!(!editor.can_confirm());
        assert!(editor.payload().is_none());
        editor.select_session("session-1");
        assert!(editor.can_confirm());
        let first = editor.payload().unwrap();
        assert_eq!(first["request_id"], editor.request_id);
        assert_eq!(first["session_id"], "session-1");
        assert_eq!(first["core_id"], "core-1");
        assert_eq!(first["terminal_address"], "t7");
        assert_eq!(first["adapter"], "codex");
        assert!(first.get("parent_run_id").is_none());
        let retry = editor.payload().unwrap();
        assert_eq!(first, retry);
        editor.select_session("session-2");
        let changed = editor.payload().unwrap();
        assert_ne!(changed["request_id"], first["request_id"]);
    }

    #[test]
    fn confirm_is_blocked_without_a_session_or_a_valid_adapter() {
        let mut editor = AgentBindEditor::default();
        editor.open("core-1", "t7", None);
        assert!(!editor.can_confirm());
        editor.select_session("session-1");
        assert!(!editor.can_confirm());
        assert!(editor.insert("   "));
        // Whitespace alone is not an adapter; the Host would reject it too.
        assert!(editor.payload().is_none());
        editor.backspace();
        editor.backspace();
        editor.backspace();
        assert!(!editor.can_confirm());
        assert!(editor.insert("claude"));
        assert!(editor.can_confirm());
        assert_eq!(editor.payload().unwrap()["adapter"], "claude");
        assert!(!editor.insert("\n"));
        assert!(!editor.insert(&"x".repeat(MAX_ADAPTER_CHARS)));
    }

    #[test]
    fn cancel_and_reopen_mint_distinct_requests() {
        let mut editor = AgentBindEditor::default();
        editor.open("core-1", "t7", None);
        editor.select_session("session-1");
        let first = editor.request_id.clone();
        editor.close();
        assert!(!editor.is_open());
        assert!(editor.payload().is_none());
        editor.open("core-1", "t7", None);
        assert_ne!(editor.request_id, first);
        // Reopening never carries a Session selection or confirmation.
        assert!(editor.session_id.is_none());
        assert!(!editor.can_confirm());
    }

    #[test]
    fn keyboard_contract_confirms_cancels_and_edits_only_when_addressed() {
        assert_eq!(agent_bind_key("enter", None, false), AgentBindKey::Confirm);
        assert_eq!(agent_bind_key("escape", None, false), AgentBindKey::Cancel);
        assert_eq!(
            agent_bind_key("backspace", None, false),
            AgentBindKey::Backspace
        );
        assert_eq!(
            agent_bind_key("a", Some("a"), false),
            AgentBindKey::Insert("a".into())
        );
        assert_eq!(agent_bind_key("a", Some("a"), true), AgentBindKey::Ignore);
        assert_eq!(
            agent_bind_key("a", Some("\u{7f}"), false),
            AgentBindKey::Ignore
        );
    }

    #[test]
    fn host_errors_keep_their_specific_meaning() {
        let remote =
            |code: i64, message: &str| HostError::Remote(json!({"code":code,"message":message}));
        assert!(bind_error_text(&remote(-32031, "already bound")).contains("Conflict"));
        assert!(bind_error_text(&remote(-32045, "core")).contains("Core mismatch"));
        assert!(bind_error_text(&remote(-32034, "missing")).contains("not verified"));
        assert!(bind_error_text(&remote(-32061, "two")).contains("Ambiguous"));
        assert!(bind_error_text(&remote(-32040, "offline")).contains("Cannot verify"));
        assert_eq!(bind_error_text(&remote(-32602, "bad")), "bad");
    }
}
