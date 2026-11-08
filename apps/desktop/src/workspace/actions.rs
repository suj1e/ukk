//! User-intent Workspace metadata commands; never Agent lifecycle actions.
use super::pane_focus::split_ratio_payload;
use super::*;
use ukk_host_client::HostError;

/// Host RPC code for an optimistic layout revision conflict. The Desktop
/// must surface this rather than silently retrying with a guessed revision.
const REVISION_CONFLICT_CODE: i64 = -32031;

fn retry_session_matches(selected: Option<&str>, rejected_session: &str) -> bool {
    selected == Some(rejected_session)
}

fn revision_conflict(error: &HostError) -> bool {
    match error {
        HostError::Remote(value) => {
            value.get("code").and_then(Value::as_i64) == Some(REVISION_CONFLICT_CODE)
                || value
                    .get("message")
                    .and_then(Value::as_str)
                    .is_some_and(|message| message.contains("revision conflict"))
        }
        _ => false,
    }
}

impl Workspace {
    pub(super) fn mutate(
        &mut self,
        method: &'static str,
        params: Value,
        intent: MutationIntent,
        cx: &mut Context<Self>,
    ) {
        if self.mutation_busy {
            return;
        }
        self.mutation_busy = true;
        self.mutation_error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let (result, fresh) = cx
                .background_executor()
                .spawn(async move {
                    let client = HostClient::from_env();
                    let result = client.local_workspace_mutation(method, params);
                    let fresh = if result.is_ok() {
                        Some(client.workspace_state())
                    } else {
                        None
                    };
                    (result, fresh)
                })
                .await;
            this.update(cx, |view, cx| {
                view.mutation_busy = false;
                match result {
                    Ok(value) => {
                        view.in_flight_action = None;
                        view.in_flight_session = None;
                        view.conflict = None;
                        match intent {
                            MutationIntent::CreatedSession => {
                                view.selected_session =
                                    value.get("id").and_then(Value::as_str).map(str::to_owned);
                                view.selected = None;
                                view.focus_touched = true;
                                view.local_focus
                                    .set_selected_session(view.selected_session.as_deref());
                                view.persist_local_focus(cx);
                            }
                            MutationIntent::CreatedSpace => {
                                match value.get("id").and_then(Value::as_str) {
                                    Some(id) => {
                                        view.selected_space = Some(id.to_owned());
                                        view.selected = None;
                                        // A new Space has no Session yet; focus it
                                        // explicitly instead of guessing one.
                                        view.selected_session = None;
                                        view.focus_touched = true;
                                        view.local_focus.set_selected_session(None);
                                        view.persist_local_focus(cx);
                                    }
                                    None => {
                                        view.mutation_error = Some(
                                            "Host did not return the Space identity.".into(),
                                        )
                                    }
                                }
                                view.space_editor.close();
                            }
                            MutationIntent::RenamedSpace => {
                                // Rename never changes the selected Space or
                                // Session; the refreshed snapshot carries the
                                // new title.
                                view.space_editor.close();
                            }
                            MutationIntent::BoundAgentRun => {
                                let run_id = value
                                    .get("id")
                                    .and_then(Value::as_str)
                                    .unwrap_or("unknown");
                                view.agent_bind.close();
                                view.agent_bind_notice =
                                    Some(format!("Bound AgentRun {run_id}."));
                            }
                            MutationIntent::None => {}
                        }
                        if let Some(Ok(update)) = fresh {
                            view.apply_workspace_update(update, cx);
                        } else if intent == MutationIntent::BoundAgentRun {
                            view.agent_bind_notice = Some(
                                "Binding was accepted, but workspace refresh failed; use Refresh."
                                    .into(),
                            );
                        } else {
                            view.mutation_error = Some(
                                "Change was accepted, but workspace refresh failed; use Refresh."
                                    .into(),
                            );
                        }
                    }
                    Err(error) => {
                        view.pending_tab_focus = None;
                        view.pending_pane_focus = None;
                        if matches!(
                            intent,
                            MutationIntent::CreatedSpace | MutationIntent::RenamedSpace
                        ) {
                            // Space metadata has no revision CAS. Surface the
                            // Host message inside the form and keep the form
                            // open; never offer the pane-layout retry path.
                            view.in_flight_action = None;
                            view.in_flight_session = None;
                            view.space_editor.error = Some(spaces::space_error_text(&error));
                        } else if intent == MutationIntent::BoundAgentRun {
                            // Binding has no layout revision. Keep the
                            // confirmation open and show the Host reason;
                            // never route it into the pane retry path.
                            view.in_flight_action = None;
                            view.in_flight_session = None;
                            view.agent_bind.error =
                                Some(super::agent_bind::bind_error_text(&error));
                        } else if revision_conflict(&error) {
                            let action = view.in_flight_action.take();
                            let session_id = view.in_flight_session.take();
                            view.conflict = Some(ConflictNotice {
                                message: "Another window or client changed this Session layout. Your change was not applied.".into(),
                                action,
                                session_id,
                            });
                        } else {
                            view.in_flight_action = None;
                            view.in_flight_session = None;
                            view.mutation_error = Some(error.to_string());
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn create_session(&mut self, cx: &mut Context<Self>) {
        if self.mutation_busy {
            return;
        }
        let space_id = self
            .selected_space
            .clone()
            .or_else(|| self.domain.spaces.first().map(|s| s.id.clone()));
        let Some(space_id) = space_id else {
            self.mutation_error =
                Some("No Space yet. Use + New Space in the sidebar to create one.".into());
            cx.notify();
            return;
        };
        let count = self
            .domain
            .sessions
            .iter()
            .filter(|s| s.space_id == space_id)
            .count();
        self.mutate(
            "workspace.session.create",
            json!({
                "request_id":new_id(),
                "space_id":space_id,
                "title":format!("New Session {}", count + 1)
            }),
            MutationIntent::CreatedSession,
            cx,
        );
    }

    pub(super) fn pane_action(&mut self, action: PaneAction, cx: &mut Context<Self>) {
        if self.mutation_busy {
            return;
        }
        let Some(session_id) = self.selected_session.clone() else {
            return;
        };
        let Some(layout) = self
            .domain
            .layouts
            .iter()
            .find(|layout| layout.session_id == session_id)
        else {
            return;
        };
        let revision = layout.revision;
        let retry_action = action.clone();
        let (method, params) = match action {
            PaneAction::Split {
                pane_id,
                axis,
                content,
            } => (
                "workspace.pane.split",
                json!({
                    "session_id":session_id,"pane_id":pane_id,
                    "axis":axis,"content":content,"expected_revision":revision
                }),
            ),
            PaneAction::Close { pane_id } => (
                "workspace.pane.close",
                json!({
                    "session_id":session_id,"pane_id":pane_id,"expected_revision":revision
                }),
            ),
            PaneAction::Resize { split_id, ratio } => (
                "workspace.pane.resize",
                json!({
                    "session_id":session_id,"split_id":split_id,
                    "ratio":split_ratio_payload(ratio),"expected_revision":revision
                }),
            ),
            PaneAction::TabAdd {
                pane_id,
                title,
                content,
            } => (
                "workspace.tab.add",
                json!({
                    "session_id":session_id,"pane_id":pane_id,
                    "title":title,"content":content,"expected_revision":revision
                }),
            ),
            PaneAction::TabClose { group_id, tab_id } => (
                "workspace.tab.close",
                json!({
                    "session_id":session_id,"group_id":group_id,
                    "tab_id":tab_id,"expected_revision":revision
                }),
            ),
            PaneAction::TabReorder {
                group_id,
                tab_id,
                direction,
            } => (
                "workspace.tab.reorder",
                json!({
                    "session_id":session_id,"group_id":group_id,
                    "tab_id":tab_id,"direction":direction,"expected_revision":revision
                }),
            ),
            PaneAction::TabMove {
                source_group_id,
                destination_pane_id,
                tab_id,
            } => (
                "workspace.tab.move",
                json!({
                    "session_id":session_id,"source_group_id":source_group_id,
                    "destination_pane_id":destination_pane_id,
                    "tab_id":tab_id,"expected_revision":revision
                }),
            ),
        };
        self.in_flight_action = Some(retry_action);
        self.in_flight_session = Some(session_id);
        self.mutate(method, params, MutationIntent::None, cx);
    }

    /// Explicit user retry after a stale CAS rejection: reload the authoritative
    /// revision first, then re-issue the same intent exactly once. The intent
    /// carries its originating Session so its IDs are never applied elsewhere.
    pub(super) fn retry_conflict(&mut self, cx: &mut Context<Self>) {
        let Some(conflict) = self.conflict.take() else {
            return;
        };
        self.mutation_error = None;
        if let Some(action) = conflict.action {
            if let Some(session_id) = conflict.session_id {
                let exists = self
                    .domain
                    .sessions
                    .iter()
                    .any(|session| session.id == session_id);
                if !exists {
                    self.mutation_error = Some(
                        "The Session that rejected this change no longer exists; review the reloaded layout."
                            .into(),
                    );
                } else {
                    if !retry_session_matches(self.selected_session.as_deref(), &session_id) {
                        // Switch back to the owning Session explicitly; never
                        // translate old Pane/Tab IDs into the current one.
                        self.selected_space = self
                            .domain
                            .sessions
                            .iter()
                            .find(|session| session.id == session_id)
                            .map(|session| session.space_id.clone())
                            .or_else(|| self.selected_space.clone());
                        self.selected_session = Some(session_id.clone());
                        self.selected = None;
                        self.focus_touched = true;
                        self.local_focus.set_selected_session(Some(&session_id));
                        self.persist_local_focus(cx);
                    }
                    self.retry_after_reload = Some((session_id, action));
                }
            }
        }
        self.refresh(cx);
        cx.notify();
    }

    pub(super) fn apply_retry_after_reload(&mut self, cx: &mut Context<Self>) {
        let Some((session_id, action)) = self.retry_after_reload.take() else {
            return;
        };
        if !retry_session_matches(self.selected_session.as_deref(), &session_id) {
            self.mutation_error = Some(
                "Retry was cancelled because the focused Session changed; the layout was reloaded."
                    .into(),
            );
            cx.notify();
            return;
        }
        self.pane_action(action, cx);
    }

    pub(super) fn dismiss_conflict(&mut self, cx: &mut Context<Self>) {
        self.conflict = None;
        cx.notify();
    }

    /// Shared Session selection path so per-device focus is remembered.
    pub(super) fn select_session(&mut self, session_id: String, cx: &mut Context<Self>) {
        self.cancel_split_drag(cx);
        self.selected_session = Some(session_id.clone());
        self.selected = None;
        self.focus_touched = true;
        self.local_focus.set_selected_session(Some(&session_id));
        self.persist_local_focus(cx);
        self.sync_repository_surfaces(cx);
        self.refresh_plugins(cx);
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revision_conflict_is_recognised_by_code_and_message() {
        let by_code = HostError::Remote(serde_json::json!({
            "code": -32031,
            "message": "layout revision conflict; reload before retrying"
        }));
        let by_message =
            HostError::Remote(serde_json::json!({"message": "layout revision conflict"}));
        let other =
            HostError::Remote(serde_json::json!({"code": -32032, "message": "invalid tab"}));
        assert!(revision_conflict(&by_code));
        assert!(revision_conflict(&by_message));
        assert!(!revision_conflict(&other));
        assert!(!revision_conflict(&HostError::Protocol("offline".into())));
    }

    #[test]
    fn retry_is_rejected_after_switching_session() {
        assert!(retry_session_matches(Some("session-a"), "session-a"));
        assert!(!retry_session_matches(Some("session-b"), "session-a"));
        assert!(!retry_session_matches(None, "session-a"));
    }
}
