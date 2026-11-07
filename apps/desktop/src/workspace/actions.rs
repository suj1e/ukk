//! User-intent Workspace metadata commands; never Agent lifecycle actions.
use super::*;

impl Workspace {
    fn mutate(
        &mut self,
        method: &'static str,
        params: Value,
        select_created_session: bool,
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
                        Some(client.workspace_snapshot())
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
                        if select_created_session {
                            view.selected_session =
                                value.get("id").and_then(Value::as_str).map(str::to_owned);
                            view.selected = None;
                        }
                        if let Some(Ok(domain)) = fresh {
                            view.domain = domain;
                        } else {
                            view.mutation_error = Some(
                                "Change was accepted, but workspace refresh failed; use Refresh."
                                    .into(),
                            );
                        }
                    }
                    Err(error) => {
                        view.mutation_error = Some(error.to_string());
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn create_session(&mut self, cx: &mut Context<Self>) {
        let space_id = self
            .selected_space
            .clone()
            .or_else(|| self.domain.spaces.first().map(|s| s.id.clone()));
        let Some(space_id) = space_id else {
            self.mutation_error =
                Some("No Space yet. Create one with ukk-host space-create first.".into());
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
            true,
            cx,
        );
    }

    pub(super) fn pane_action(&mut self, action: PaneAction, cx: &mut Context<Self>) {
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
                    "ratio":ratio,"expected_revision":revision
                }),
            ),
        };
        self.mutate(method, params, false, cx);
    }
}
