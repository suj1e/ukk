//! Native ukk GPUI workspace: state orchestration, native panes, and Agent views.
//! herdr-core processes and SQLite data are owned outside this window.

use crate::theme::*;
use gpui::{div, prelude::*, px, rgb, Context, Render, Window};
use serde_json::{json, Value};
use std::time::Duration;
use ukk_domain::{new_id, DomainSnapshot, PaneContent, PaneNode, Session, SplitAxis};
use ukk_host_client::{HostClient, HostSnapshot, LiveHostState, Terminal};

mod agent_detail;
mod panes;
mod render;
mod sidebar;
mod toolbar;

#[derive(Clone, Copy, PartialEq, Eq)]
enum DetailTab {
    Overview,
    Runtime,
}

#[derive(Clone)]
enum PaneAction {
    Split { pane_id: String, axis: SplitAxis, content: PaneContent },
    Close { pane_id: String },
    Resize { split_id: String, ratio: f32 },
}

pub(crate) struct Workspace {
    snapshot: HostSnapshot,
    selected: Option<String>,
    selected_session: Option<String>,
    selected_space: Option<String>,
    domain: DomainSnapshot,
    tab: DetailTab,
    loading: bool,
    mutation_busy: bool,
    mutation_error: Option<String>,
    online: bool,
    message: Option<String>,
    host_epoch: String,
    host_revision: u64,
}

impl Workspace {
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        let mut view = Self {
            snapshot: HostSnapshot::default(),
            selected: None,
            selected_session: None,
            selected_space: None,
            domain: DomainSnapshot::default(),
            tab: DetailTab::Overview,
            loading: false,
            mutation_busy: false,
            mutation_error: None,
            online: false,
            message: None,
            host_epoch: String::new(),
            host_revision: 0,
        };
        view.watch_updates(cx);
        view.refresh(cx);
        view
    }

    fn selected_terminal(&self) -> Option<&Terminal> {
        let id = self.selected.as_deref()?;
        self.snapshot.terminals.iter().find(|terminal| terminal.address == id)
    }

    /// Update the UI from an authoritative Host view. The Host reports
    /// core connectivity separately from the existence of its local socket.
    fn apply_host_state(&mut self, live: LiveHostState) {
        if live.host_epoch == self.host_epoch && live.revision < self.host_revision {
            return; // avoid stale concurrent refresh replies
        }
        self.host_epoch = live.host_epoch;
        self.host_revision = live.revision;
        self.online = live.core_connected;
        self.message = live.message;
        if !self
            .selected
            .as_ref()
            .is_some_and(|address| live.snapshot.terminals.iter().any(|t| &t.address == address))
        {
            self.selected = None; // A Space/Session should be the initial focus.
        }
        self.snapshot = live.snapshot;
    }

    /// A long-poll task stays alive only as long as this GPUI entity exists.
    /// Neither rendering nor click handlers ever block on herdr-core.
    fn watch_updates(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let mut epoch = String::new();
            let mut revision = 0_u64;
            loop {
                let last_epoch = epoch.clone();
                let result = cx
                    .background_executor()
                    .spawn(async move {
                        HostClient::from_env().wait_for_change(&last_epoch, revision)
                    })
                    .await;
                match result {
                    Ok(live) => {
                        epoch = live.host_epoch.clone();
                        revision = live.revision;
                        if this
                            .update(cx, |view, cx| {
                                view.apply_host_state(live);
                                cx.notify();
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(error) => {
                        if this
                            .update(cx, |view, cx| {
                                view.online = false;
                                view.message = Some(error.to_string());
                                cx.notify();
                            })
                            .is_err()
                        {
                            break;
                        }
                        cx.background_executor().timer(Duration::from_secs(2)).await;
                    }
                }
            }
        })
        .detach();
    }

    /// All native editing actions run off the UI thread and resolve to a
    /// fresh authoritative SQLite snapshot. No direct SQLite access in GPUI.
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
            let (result, fresh) = cx.background_executor().spawn(async move {
                let client = HostClient::from_env();
                let result = client.local_workspace_mutation(method, params);
                let fresh = if result.is_ok() {
                    Some(client.workspace_snapshot())
                } else {
                    None
                };
                (result, fresh)
            }).await;
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
                                "Change was accepted, but workspace refresh failed; use Refresh.".into()
                            );
                        }
                    }
                    Err(error) => {
                        view.mutation_error = Some(error.to_string());
                    }
                }
                cx.notify();
            }).ok();
        }).detach();
    }

    fn create_session(&mut self, cx: &mut Context<Self>) {
        let space_id = self.selected_space.clone()
            .or_else(|| self.domain.spaces.first().map(|s| s.id.clone()));
        let Some(space_id) = space_id else {
            self.mutation_error = Some(
                "No Space yet. Create one with ukk-host space-create first.".into()
            );
            cx.notify();
            return;
        };
        let count = self.domain.sessions.iter()
            .filter(|s| s.space_id == space_id).count();
        self.mutate("workspace.session.create", json!({
            "request_id":new_id(),
            "space_id":space_id,
            "title":format!("New Session {}", count + 1)
        }), true, cx);
    }

    fn pane_action(&mut self, action: PaneAction, cx: &mut Context<Self>) {
        let Some(session_id) = self.selected_session.clone() else { return };
        let Some(layout) = self.domain.layouts.iter()
            .find(|layout| layout.session_id == session_id)
        else { return };
        let revision = layout.revision;
        let (method, params) = match action {
            PaneAction::Split { pane_id, axis, content } => (
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

    /// A manual fallback for inspecting Host connectivity and state.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.loading {
            return;
        }
        self.loading = true;
        cx.notify();

        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let client = HostClient::from_env();
                    (client.host_snapshot(), client.workspace_snapshot())
                })
                .await;
            this.update(cx, |view, cx| {
                view.loading = false;
                let (host, domain) = result;
                match host {
                    Ok(live) => view.apply_host_state(live),
                    Err(error) => {
                        view.online = false;
                        view.message = Some(error.to_string());
                        // Keep last known metadata, but visibly label it stale.
                    }
                }
                match domain {
                    Ok(domain) => {
                        if view.selected_space.as_ref().is_some_and(
                            |id| !domain.spaces.iter().any(|s| &s.id == id)
                        ) {
                            view.selected_space = None;
                        }
                        if view.selected_space.is_none() {
                            view.selected_space = domain.spaces.first().map(|s| s.id.clone());
                        }
                        if view.selected_session.as_ref().is_some_and(
                            |id| !domain.sessions.iter().any(|s| &s.id == id)
                        ) {
                            view.selected_session = None;
                        }
                        if view.selected_session.is_none() && view.selected.is_none() {
                            view.selected_session = domain.sessions.iter()
                                .find(|s| !s.archived &&
                                    view.selected_space.as_ref().is_none_or(
                                        |space| &s.space_id == space
                                    ))
                                .map(|s| s.id.clone());
                        }
                        view.domain = domain;
                    }
                    Err(error) => {
                        view.message = Some(format!("Workspace storage unavailable: {error}"));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}
