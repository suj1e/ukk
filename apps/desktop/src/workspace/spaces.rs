//! Explicit Space creation and rename.
//!
//! The form is local UI state. Submitting goes through the same
//! capability-gated `workspace.space.*` mutations as every other metadata
//! edit. It never starts, signals, approves or attaches to an Agent, and it
//! never writes a Space from optimistic client input.

use super::*;
use gpui::KeyDownEvent;
use ukk_domain::Space;
use ukk_host_client::HostError;

/// Mirrors the Store boundary in `crates/store/src/records.rs`.
pub(super) const MAX_SPACE_FIELD_CHARS: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SpaceField {
    Title,
    ProjectRoot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SpaceTarget {
    New,
    Rename { space_id: String },
}

/// Local form state. `request_id` is generated once when a new Space form is
/// opened and reused by every retry, so a duplicated submission is idempotent.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct SpaceEditor {
    pub(super) target: Option<SpaceTarget>,
    pub(super) title: String,
    pub(super) project_root: String,
    pub(super) request_id: String,
    pub(super) error: Option<String>,
}

impl SpaceEditor {
    pub(super) fn open_new(&mut self) {
        *self = Self {
            target: Some(SpaceTarget::New),
            request_id: new_id(),
            ..Self::default()
        };
    }

    pub(super) fn open_rename(&mut self, space: &Space) {
        *self = Self {
            target: Some(SpaceTarget::Rename {
                space_id: space.id.clone(),
            }),
            title: space.title.clone(),
            ..Self::default()
        };
    }

    pub(super) fn close(&mut self) {
        *self = Self::default();
    }

    pub(super) fn is_open(&self) -> bool {
        self.target.is_some()
    }

    /// Returns whether the field changed. Input is bounded before the Store
    /// sees it; the Store remains the authoritative validator.
    pub(super) fn insert(&mut self, field: SpaceField, text: &str) -> bool {
        if text.is_empty() || text.chars().any(char::is_control) {
            return false;
        }
        let value = match field {
            SpaceField::Title => &mut self.title,
            SpaceField::ProjectRoot => &mut self.project_root,
        };
        if value.chars().count() + text.chars().count() > MAX_SPACE_FIELD_CHARS {
            return false;
        }
        value.push_str(text);
        true
    }

    pub(super) fn backspace(&mut self, field: SpaceField) -> bool {
        let value = match field {
            SpaceField::Title => &mut self.title,
            SpaceField::ProjectRoot => &mut self.project_root,
        };
        value.pop().is_some()
    }
}

/// A key's meaning inside a Space text field. Pure so the keyboard submit and
/// cancel contract is testable without constructing a GPUI window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SpaceFieldKey {
    Submit,
    Cancel,
    Backspace,
    Insert(String),
    Ignore,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SpaceFormAction {
    Submit,
    Cancel,
}

pub(super) fn space_field_key(key: &str, key_char: Option<&str>, modifier: bool) -> SpaceFieldKey {
    match key {
        "enter" | "return" => SpaceFieldKey::Submit,
        "escape" => SpaceFieldKey::Cancel,
        "backspace" if !modifier => SpaceFieldKey::Backspace,
        _ if modifier => SpaceFieldKey::Ignore,
        _ => match key_char {
            Some(text) if !text.is_empty() && !text.chars().any(char::is_control) => {
                SpaceFieldKey::Insert(text.to_owned())
            }
            _ => SpaceFieldKey::Ignore,
        },
    }
}

pub(super) fn validated_space_title(raw: &str) -> Result<String, String> {
    let title = raw.trim();
    if title.is_empty() {
        return Err("Enter a Space name.".into());
    }
    if title.chars().count() > MAX_SPACE_FIELD_CHARS {
        return Err(format!(
            "Space name must be 1–{MAX_SPACE_FIELD_CHARS} characters."
        ));
    }
    Ok(title.to_owned())
}

pub(super) fn validated_project_root(raw: &str) -> Result<String, String> {
    let root = raw.trim();
    if root.is_empty() {
        return Err("Enter the project root for this Space.".into());
    }
    if root.chars().count() > MAX_SPACE_FIELD_CHARS {
        return Err(format!(
            "Project root must be 1–{MAX_SPACE_FIELD_CHARS} characters."
        ));
    }
    Ok(root.to_owned())
}

pub(super) fn create_space_payload(request_id: &str, title: &str, project_root: &str) -> Value {
    json!({
        "request_id": request_id,
        "title": title,
        "project_root": project_root
    })
}

pub(super) fn rename_space_payload(space_id: &str, title: &str) -> Value {
    json!({"space_id": space_id, "title": title})
}

/// Host errors are shown to the user without tokens. A duplicate request id is
/// labelled as a conflict instead of the pane-layout reload wording.
pub(super) fn space_error_text(error: &HostError) -> String {
    match error {
        HostError::Remote(value) => {
            let message = value
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("the Host rejected this Space change");
            if value.get("code").and_then(Value::as_i64) == Some(-32031) {
                format!("Conflict: {message}. Cancel and start a new Space.")
            } else {
                message.to_owned()
            }
        }
        other => other.to_string(),
    }
}

impl Workspace {
    pub(super) fn open_new_space(&mut self, cx: &mut Context<Self>) {
        if self.mutation_busy {
            return;
        }
        self.space_editor.open_new();
        self.mutation_error = None;
        cx.notify();
    }

    pub(super) fn open_rename_space(&mut self, space_id: String, cx: &mut Context<Self>) {
        if self.mutation_busy {
            return;
        }
        let Some(space) = self
            .domain
            .spaces
            .iter()
            .find(|space| space.id == space_id)
            .cloned()
        else {
            self.mutation_error = Some("That Space no longer exists; reload the workspace.".into());
            cx.notify();
            return;
        };
        self.space_editor.open_rename(&space);
        self.mutation_error = None;
        cx.notify();
    }

    pub(super) fn cancel_space_form(&mut self, cx: &mut Context<Self>) {
        self.space_editor.close();
        cx.notify();
    }

    /// One explicit submit path for both keyboard Enter and the form button.
    pub(super) fn submit_space_form(&mut self, cx: &mut Context<Self>) {
        if self.mutation_busy {
            return;
        }
        let Some(target) = self.space_editor.target.clone() else {
            return;
        };
        let title = match validated_space_title(&self.space_editor.title) {
            Ok(title) => title,
            Err(error) => {
                self.space_editor.error = Some(error);
                cx.notify();
                return;
            }
        };
        match target {
            SpaceTarget::New => {
                let project_root = match validated_project_root(&self.space_editor.project_root) {
                    Ok(root) => root,
                    Err(error) => {
                        self.space_editor.error = Some(error);
                        cx.notify();
                        return;
                    }
                };
                self.space_editor.error = None;
                let params =
                    create_space_payload(&self.space_editor.request_id, &title, &project_root);
                self.mutate(
                    "workspace.space.create",
                    params,
                    MutationIntent::CreatedSpace,
                    cx,
                );
            }
            SpaceTarget::Rename { space_id } => {
                if !self.domain.spaces.iter().any(|space| space.id == space_id) {
                    self.space_editor.error =
                        Some("That Space no longer exists; reload the workspace.".into());
                    cx.notify();
                    return;
                }
                self.space_editor.error = None;
                let params = rename_space_payload(&space_id, &title);
                self.mutate(
                    "workspace.space.rename",
                    params,
                    MutationIntent::RenamedSpace,
                    cx,
                );
            }
        }
    }

    fn space_field_key(&mut self, field: SpaceField, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let modifiers = event.keystroke.modifiers;
        let action = space_field_key(
            event.keystroke.key.as_str(),
            event.keystroke.key_char.as_deref(),
            modifiers.control || modifiers.platform || modifiers.alt,
        );
        match action {
            SpaceFieldKey::Submit => self.submit_space_form(cx),
            SpaceFieldKey::Cancel => self.cancel_space_form(cx),
            SpaceFieldKey::Backspace => {
                self.space_editor.error = None;
                self.space_editor.backspace(field);
                cx.notify();
            }
            SpaceFieldKey::Insert(text) => {
                if self.space_editor.insert(field, &text) {
                    self.space_editor.error = None;
                    cx.notify();
                }
            }
            // Unhandled keys (Tab and shortcuts) keep their normal behaviour.
            SpaceFieldKey::Ignore => return,
        }
        cx.stop_propagation();
    }

    /// Rendered only while the form is open. Local form state never reaches
    /// SQLite until the capability-gated mutation succeeds.
    pub(super) fn space_form(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Some(target) = self.space_editor.target.clone() else {
            return div().into_any_element();
        };
        let busy = self.mutation_busy;
        let (heading, submit_label) = match target {
            SpaceTarget::New => ("New Space", if busy { "Creating…" } else { "Create Space" }),
            SpaceTarget::Rename { .. } => {
                ("Rename Space", if busy { "Saving…" } else { "Save name" })
            }
        };
        let is_new = self.space_editor.target == Some(SpaceTarget::New);
        let title = self.space_editor.title.clone();
        let project_root = self.space_editor.project_root.clone();
        let mut form = div()
            .id("space-form")
            .role(Role::Group)
            .aria_label(heading)
            .flex()
            .flex_col()
            .gap_2()
            .mt_3()
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
                    .child(heading),
            )
            .child(self.space_text_field(
                SpaceField::Title,
                "Space name",
                "e.g. ukk web",
                &self.space_title_focus,
                title,
                cx,
            ));
        if is_new {
            form = form.child(self.space_text_field(
                SpaceField::ProjectRoot,
                "Project root",
                "/path/to/project",
                &self.space_root_focus,
                project_root,
                cx,
            ));
        }
        form = form.child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_2()
                .child(self.space_form_button(SpaceFormAction::Submit, submit_label, busy, cx))
                .child(self.space_form_button(SpaceFormAction::Cancel, "Cancel", false, cx)),
        );
        if let Some(error) = self.space_editor.error.clone() {
            form = form.child(
                div()
                    .id("space-form-error")
                    .role(Role::Alert)
                    .aria_label(format!("Space change failed: {error}"))
                    .text_xs()
                    .text_color(rgb(0xa14b32))
                    .child(error),
            );
        }
        form.into_any_element()
    }

    fn space_text_field(
        &self,
        field: SpaceField,
        label: &'static str,
        placeholder: &'static str,
        focus: &gpui::FocusHandle,
        value: String,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let empty = value.is_empty();
        let id = match field {
            SpaceField::Title => "space-title",
            SpaceField::ProjectRoot => "space-root",
        };
        div()
            .id(id)
            .role(Role::TextInput)
            .aria_label(label)
            .track_focus(focus)
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
            .text_color(rgb(if empty { MUTED } else { INK }))
            .on_click(cx.listener(move |this, _, window, cx| {
                window.focus(
                    match field {
                        SpaceField::Title => &this.space_title_focus,
                        SpaceField::ProjectRoot => &this.space_root_focus,
                    },
                    cx,
                );
            }))
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                this.space_field_key(field, event, cx);
            }))
            .child(if empty { placeholder.to_owned() } else { value })
    }

    fn space_form_button(
        &self,
        action: SpaceFormAction,
        label: &'static str,
        disabled: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let (id, aria, primary) = match action {
            SpaceFormAction::Submit => ("space-submit", "Submit this Space change", true),
            SpaceFormAction::Cancel => ("space-cancel", "Cancel the Space change", false),
        };
        div()
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
            .bg(rgb(if primary { ACCENT } else { WHITE }))
            .text_xs()
            .text_color(rgb(if primary { WHITE } else { INK }))
            .cursor_pointer()
            .when(disabled, |element| element.opacity(0.6))
            .on_click(cx.listener(move |this, _, _, cx| {
                if disabled {
                    return;
                }
                match action {
                    SpaceFormAction::Submit => this.submit_space_form(cx),
                    SpaceFormAction::Cancel => this.cancel_space_form(cx),
                }
            }))
            .child(label)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enter_submits_and_escape_cancels_space_form_fields() {
        assert_eq!(space_field_key("enter", None, false), SpaceFieldKey::Submit);
        assert_eq!(
            space_field_key("escape", None, false),
            SpaceFieldKey::Cancel
        );
        assert_eq!(
            space_field_key("enter", Some("\r"), true),
            SpaceFieldKey::Submit
        );
        assert_eq!(
            space_field_key("backspace", None, false),
            SpaceFieldKey::Backspace
        );
        assert_eq!(
            space_field_key("backspace", None, true),
            SpaceFieldKey::Ignore
        );
        assert_eq!(
            space_field_key("a", Some("a"), false),
            SpaceFieldKey::Insert("a".into())
        );
        assert_eq!(
            space_field_key("a", Some("a"), true),
            SpaceFieldKey::Ignore,
            "platform/control/alt shortcuts must not type into the field"
        );
        assert_eq!(
            space_field_key("tab", None, false),
            SpaceFieldKey::Ignore,
            "Tab must keep moving focus"
        );
        assert_eq!(
            space_field_key("unknown", Some("\u{7}"), false),
            SpaceFieldKey::Ignore
        );
    }

    #[test]
    fn new_space_keeps_one_request_id_across_typing_and_retries() {
        let mut editor = SpaceEditor::default();
        editor.open_new();
        let request_id = editor.request_id.clone();
        assert_eq!(editor.target, Some(SpaceTarget::New));
        assert_eq!(request_id.len(), 36, "new_id returns a canonical UUID");
        assert_eq!(request_id.matches('-').count(), 4);
        editor.insert(SpaceField::Title, "ukk");
        editor.insert(SpaceField::ProjectRoot, "/work");
        editor.backspace(SpaceField::ProjectRoot);
        assert_eq!(
            create_space_payload(&editor.request_id, "ukk", "/work"),
            json!({"request_id":request_id,"title":"ukk","project_root":"/work"})
        );
        editor.close();
        assert!(!editor.is_open());
        assert!(editor.request_id.is_empty());
        editor.open_new();
        assert_ne!(
            editor.request_id, request_id,
            "a deliberate new form gets a fresh id"
        );
    }

    #[test]
    fn rename_target_carries_the_space_identity_into_the_payload() {
        let mut editor = SpaceEditor::default();
        editor.open_rename(&Space {
            id: "space-1".into(),
            title: "Before".into(),
            project_root: "/work".into(),
            created_ms: 0,
        });
        assert_eq!(
            editor.target,
            Some(SpaceTarget::Rename {
                space_id: "space-1".into()
            })
        );
        assert_eq!(editor.title, "Before");
        editor.insert(SpaceField::Title, " after");
        assert_eq!(
            rename_space_payload("space-1", &editor.title),
            json!({"space_id":"space-1","title":"Before after"})
        );
    }

    #[test]
    fn form_validation_matches_the_store_bounds() {
        assert_eq!(validated_space_title("  ukk  ").unwrap(), "ukk".to_owned());
        assert!(validated_space_title("   ").is_err());
        assert!(validated_space_title(&"x".repeat(201)).is_err());
        assert_eq!(
            validated_space_title(&"x".repeat(200))
                .unwrap()
                .chars()
                .count(),
            200
        );
        assert_eq!(validated_project_root("/work").unwrap(), "/work".to_owned());
        assert!(validated_project_root(" ").is_err());
        assert!(validated_project_root(&"x".repeat(201)).is_err());

        let mut editor = SpaceEditor::default();
        editor.open_new();
        assert!(editor.insert(SpaceField::Title, &"x".repeat(200)));
        assert!(!editor.insert(SpaceField::Title, "y"));
        assert!(!editor.insert(SpaceField::Title, "\u{1}"));
        assert!(!editor.insert(SpaceField::Title, ""));
    }

    #[test]
    fn payloads_never_include_capability_runtime_or_agent_fields() {
        let create = create_space_payload("request-1", "ukk", "/work");
        let object = create.as_object().unwrap();
        assert_eq!(object.len(), 3);
        for key in ["request_id", "title", "project_root"] {
            assert!(object.contains_key(key));
        }
        let rename = rename_space_payload("space-1", "ukk");
        let object = rename.as_object().unwrap();
        assert_eq!(object.len(), 2);
        for forbidden in [
            "auth",
            "control",
            "intent",
            "terminal",
            "terminal_address",
            "core_id",
            "agent_run_id",
            "session_id",
        ] {
            assert!(!object.contains_key(forbidden), "{forbidden}");
        }
    }

    #[test]
    fn host_errors_are_shown_without_layout_retry_wording() {
        let conflict = space_error_text(&HostError::Remote(json!({
            "code": -32031,
            "message": "request_id was already used with different Space details"
        })));
        assert!(conflict.starts_with("Conflict:"));
        assert!(!conflict.contains("layout"));
        let missing = space_error_text(&HostError::Remote(json!({
            "code": -32034,
            "message": "not found: space missing"
        })));
        assert_eq!(missing, "not found: space missing");
        assert_eq!(
            space_error_text(&HostError::Protocol("offline".into())),
            "Host protocol error: offline"
        );
    }
}
