//! ukk Desktop — an experimental native GPUI shell.
//!
//! Unlike herdr-app, this is a new UI. The first screen displays only the
//! actual terminals exposed by a local ukk Host; no synthetic Agent activity,
//! fake chats, or invented providers. Agent runtime lives outside this window.

use gpui::{
    div, prelude::*, px, rgb, size, App, Bounds, Context, Window, WindowBounds, WindowOptions,
};
use gpui_platform::application;
use serde_json::{json, Value};
use std::time::Duration;
use ukk_host_client::{HostClient, HostSnapshot, LiveHostState, Terminal};
use ukk_domain::{new_id, DomainSnapshot, PaneContent, PaneNode, Session, SplitAxis};

const INK: u32 = 0x242730;
const MUTED: u32 = 0x858b97;
const LINE: u32 = 0xe9ebf0;
const BG: u32 = 0xf8f9fb;
const WHITE: u32 = 0xffffff;
const SURFACE: u32 = 0xf2f4f7;
const ACCENT: u32 = 0x5359bb;

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

struct Workspace {
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
    fn new(cx: &mut Context<Self>) -> Self {
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
                                .find(|s| !s.archived).map(|s| s.id.clone());
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

    fn topbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
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
                            .font_semibold()
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
                            .child(if is_loading { "Refreshing…" } else { "↻  Refresh" }),
                    ),
            )
    }

    fn space_row(&self, space: &ukk_domain::Space, cx: &mut Context<Self>) -> impl IntoElement {
        let id = space.id.clone();
        let title = space.title.clone();
        let active = self.selected_space.as_deref() == Some(id.as_str());
        div()
            .id(format!("space-{id}"))
            .px_3()
            .py_2()
            .rounded_lg()
            .cursor_pointer()
            .bg(rgb(if active { 0xe9ebf7 } else { WHITE }))
            .hover(|style| style.bg(rgb(SURFACE)))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.selected_space = Some(id.clone());
                this.selected_session = this.domain.sessions.iter()
                    .find(|s| s.space_id == id && !s.archived)
                    .map(|s| s.id.clone());
                this.selected = None;
                cx.notify();
            }))
            .child(div().text_sm().text_color(rgb(INK)).child(title))
    }

    fn pane_button(
        &self,
        cx: &mut Context<Self>,
        id: String,
        label: &'static str,
        action: PaneAction,
    ) -> impl IntoElement {
        div()
            .id(id)
            .px_2()
            .py_1()
            .rounded_md()
            .border_1()
            .border_color(rgb(LINE))
            .bg(rgb(WHITE))
            .text_xs()
            .text_color(rgb(ACCENT))
            .cursor_pointer()
            .hover(|style| style.bg(rgb(SURFACE)))
            .on_click(cx.listener(move |this, _, _, cx| {
                if !this.mutation_busy {
                    this.pane_action(action.clone(), cx);
                }
            }))
            .child(label)
    }

    fn session_row(&self, session: &Session, cx: &mut Context<Self>) -> impl IntoElement {
        let active = self.selected_session.as_deref() == Some(session.id.as_str());
        let id = session.id.clone();
        let title = session.title.clone();
        div()
            .id(format!("session-{id}"))
            .px_3()
            .py_2()
            .rounded_lg()
            .cursor_pointer()
            .bg(rgb(if active { 0xe9ebf7 } else { WHITE }))
            .hover(|style| style.bg(rgb(SURFACE)))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.selected_session = Some(id.clone());
                this.selected = None;
                cx.notify();
            }))
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(if session.archived { MUTED } else { INK }))
                    .child(title),
            )
    }

    /// Recursive native composition. Splits are ordinary GPUI layout nodes;
    /// only the Host changes durable layout data. No Pane owns an Agent.
    fn session_pane(&self, node: &PaneNode, cx: &mut Context<Self>) -> gpui::AnyElement {
        match node {
            PaneNode::Leaf { id, content } => {
                let (heading, description) = match content {
                    PaneContent::Activity => (
                        "Activity".to_string(),
                        "Structured activity stream will be connected here.".to_string(),
                    ),
                    PaneContent::Files => (
                        "Files".to_string(),
                        "Project browser is not connected yet.".to_string(),
                    ),
                    PaneContent::Diff => (
                        "Diff".to_string(),
                        "Git diff integration is upcoming.".to_string(),
                    ),
                    PaneContent::Agent { agent_run_id } => {
                        let binding = self.domain.agent_runs.iter().find(|a| &a.id == agent_run_id);
                        match binding {
                            Some(run) => (
                                format!("Agent · {}", run.adapter),
                                format!("{} / {} · runtime owned by herdr-core.",
                                    run.core_id, run.terminal_address),
                            ),
                            None => ("Unresolved Agent".into(), "Binding not found.".into()),
                        }
                    }
                    PaneContent::Terminal { agent_run_id } => {
                        let binding = self.domain.agent_runs.iter().find(|a| &a.id == agent_run_id);
                        match binding {
                            Some(run) => ("Terminal".into(),
                                format!("{} · PTY attachment is upcoming.", run.terminal_address)),
                            None => ("Unresolved Terminal".into(), "Binding not found.".into()),
                        }
                    }
                };
                let new_content = match content {
                    PaneContent::Agent { agent_run_id } =>
                        PaneContent::Terminal { agent_run_id: agent_run_id.clone() },
                    PaneContent::Diff => PaneContent::Files,
                    PaneContent::Files => PaneContent::Activity,
                    PaneContent::Terminal { .. } | PaneContent::Activity => PaneContent::Diff,
                };
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .p_4()
                    .bg(rgb(WHITE))
                    .rounded_lg()
                    .border_1()
                    .border_color(rgb(LINE))
                    .child(
                        div().flex().items_center().justify_between().gap_2()
                            .child(div().text_sm().font_semibold()
                                .text_color(rgb(INK)).child(heading))
                            .child(
                                div().flex().gap_1()
                                    .child(self.pane_button(cx, format!("{id}-split-right"), "Split →",
                                        PaneAction::Split { pane_id: id.clone(),
                                            axis: SplitAxis::Horizontal,
                                            content: new_content.clone() }))
                                    .child(self.pane_button(cx, format!("{id}-split-down"), "Split ↓",
                                        PaneAction::Split { pane_id: id.clone(),
                                            axis: SplitAxis::Vertical,
                                            content: new_content }))
                                    .child(self.pane_button(cx, format!("{id}-close"), "×",
                                        PaneAction::Close { pane_id: id.clone() })),
                            ),
                    )
                    .child(div().mt_3().text_sm().text_color(rgb(MUTED)).child(description))
                    .child(div().mt_5().text_xs().text_color(rgb(MUTED))
                        .child(format!("View ID: {id}")))
                    .into_any_element()
            }
            PaneNode::Split { id, axis, ratio, first, second } => {
                let smaller = (*ratio - 0.1).max(0.15);
                let larger = (*ratio + 0.1).min(0.85);
                let controls = div()
                    .flex()
                    .justify_end()
                    .items_center()
                    .gap_2()
                    .mb_2()
                    .child(
                        div().text_xs().text_color(rgb(MUTED))
                            .child(format!("{}% / {}%",
                                (*ratio * 100.0).round() as i32,
                                ((1.0 - ratio) * 100.0).round() as i32)),
                    )
                    .child(self.pane_button(cx, format!("{id}-smaller"), "−",
                        PaneAction::Resize { split_id: id.clone(), ratio: smaller }))
                    .child(self.pane_button(cx, format!("{id}-larger"), "+",
                        PaneAction::Resize { split_id: id.clone(), ratio: larger }));
                let first_view = div()
                    .flex()
                    .flex_shrink_1()
                    .min_w_0()
                    .min_h_0()
                    .flex_basis(gpui::DefiniteLength::Fraction(*ratio))
                    .child(self.session_pane(first, cx));
                let second_view = div()
                    .flex()
                    .flex_shrink_1()
                    .min_w_0()
                    .min_h_0()
                    .flex_basis(gpui::DefiniteLength::Fraction(1.0 - ratio))
                    .child(self.session_pane(second, cx));
                let columns = div().flex().flex_1().min_w_0().min_h_0().gap_3();
                let columns = if *axis == SplitAxis::Vertical {
                    columns.flex_col()
                } else {
                    columns.flex_row()
                };
                div()
                    .flex().flex_col().flex_1().min_w_0().min_h_0()
                    .child(controls)
                    .child(columns.child(first_view).child(second_view))
                    .into_any_element()
            }
        }
    }

    fn session_detail(&self, session: &Session, cx: &mut Context<Self>) -> impl IntoElement {
        let root = self.domain.layouts.iter()
            .find(|layout| layout.session_id == session.id)
            .map(|layout| &layout.root);
        let title = session.title.clone();
        let scope = self.domain.spaces.iter()
            .find(|space| space.id == session.space_id)
            .map(|space| space.title.clone())
            .unwrap_or_else(|| "Unknown Space".to_owned());
        let status = if session.archived { "ARCHIVED" } else { "ACTIVE" };
        let mut view = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .bg(rgb(BG))
            .child(
                div().px_6().pt_6().pb_4()
                    .child(div().text_xs().text_color(rgb(MUTED))
                        .child(format!("{scope}  /  SESSION  /  {status}")))
                    .child(div().mt_2().text_2xl().font_semibold().text_color(rgb(INK)).child(title))
                    .child(div().mt_2().text_sm().text_color(rgb(MUTED))
                        .child("Pane layout is restored from SQLite; no view owns an Agent process.")),
            );
        if let Some(root) = root {
            view = view.child(div().flex().flex_1().min_h_0().p_5().child(self.session_pane(root, cx)));
        } else {
            view = view.child(div().p_6().text_color(rgb(MUTED)).child("Session layout unavailable"));
        }
        view
    }

    fn terminal_row(&self, terminal: &Terminal, depth: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let active = self.selected.as_deref() == Some(terminal.address.as_str());
        let address = terminal.address.clone();
        let display_name = if depth > 0 {
            format!("↳ {}", terminal.display_name())
        } else {
            terminal.display_name().to_owned()
        };
        let status = if self.online {
            terminal.state_label().to_owned()
        } else {
            "Last known · Host offline".to_owned()
        };

        div()
            .id(format!("terminal-{}", terminal.address))
            .flex()
            .flex_col()
            .gap_1()
            .w_full()
            .px_3()
            .py_3()
            .rounded_lg()
            .cursor_pointer()
            .bg(rgb(if active { 0xe9ebf7 } else { WHITE }))
            .hover(|style| style.bg(rgb(if active { 0xe9ebf7 } else { SURFACE })))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.selected = Some(address.clone());
                this.selected_session = None;
                this.tab = DetailTab::Overview;
                cx.notify();
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .size(px(7.0))
                            .rounded_full()
                            .bg(rgb(if !self.online {
                                0xbfc3cc
                            } else if terminal.lifecycle == "finished" {
                                0xbfc3cc
                            } else if terminal.reported_status.as_deref() == Some("blocked") {
                                0xe4a84e
                            } else if terminal.reported_status.as_deref() == Some("working") {
                                0x58ab83
                            } else {
                                0xa5aab6
                            })),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .font_semibold()
                            .text_color(rgb(INK))
                            .child(display_name),
                    ),
            )
            .child(
                div()
                    .pl(px(15.0))
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child(status),
            )
    }

    fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let entries: Vec<(Terminal, usize)> = self.snapshot.hierarchy().into_iter().map(|(t, depth)| (t.clone(), depth)).collect();
        let sessions = self.domain.sessions.clone();
        let spaces = self.domain.spaces.clone();
        div()
            .flex()
            .flex_col()
            .w(px(260.0))
            .h_full()
            .p_4()
            .bg(rgb(WHITE))
            .border_r_1()
            .border_color(rgb(LINE))
            .child(
                div()
                    .text_xs()
                    .font_semibold()
                    .text_color(rgb(MUTED))
                    .mb_3()
                    .child("SPACES"),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_3()
                    .rounded_lg()
                    .bg(rgb(SURFACE))
                    .child(
                        div().text_color(rgb(ACCENT)).font_semibold().child("⌘"),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .font_semibold()
                            .text_color(rgb(INK))
                            .child("Local machine"),
                    ),
            )
            .child(
                div().mt_4().text_xs().font_semibold().text_color(rgb(MUTED))
                    .child("PROJECT SPACES"),
            )
            .children(spaces.into_iter().map(|space| {
                div().px_3().py_2().text_sm().text_color(rgb(INK))
                    .child(space.title)
            }))
            .child(
                div().mt_5().mb_2().text_xs().font_semibold().text_color(rgb(MUTED))
                    .child("SESSIONS"),
            )
            .children(sessions.iter().map(|session| self.session_row(session, cx)))
            .child(
                div()
                    .flex()
                    .justify_between()
                    .items_center()
                    .mt_6()
                    .mb_3()
                    .child(
                        div()
                            .text_xs()
                            .font_semibold()
                            .text_color(rgb(MUTED))
                            .child("AGENTS & TERMINALS"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(MUTED))
                            .child(entries.len().to_string()),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .overflow_y_scroll()
                    .children(entries.iter().map(|(term, depth)| self.terminal_row(term, *depth, cx))),
            )
            .child(
                div()
                    .border_t_1()
                    .border_color(rgb(LINE))
                    .pt_3()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child(if self.online {
                        "●  herdr-core connected"
                    } else {
                        "○  Runtime offline / reconnecting"
                    }),
            )
    }

    fn tab_button(
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

    fn detail(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        if let Some(id) = self.selected_session.as_deref() {
            if let Some(session) = self.domain.sessions.iter().find(|session| session.id == id) {
                return self.session_detail(session, cx).into_any_element();
            }
        }
        let selected = self.selected_terminal().cloned();
        let title = selected.as_ref().map(|t| t.display_name()).unwrap_or("No agents running").to_string();

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
                            .font_semibold()
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

    fn terminal_detail(&self, term: &Terminal) -> impl IntoElement {
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
        let app = term.reported_app.clone().unwrap_or_else(|| "Not reported".to_owned());
        let lifecycle = term.lifecycle.clone();
        let address = term.address.clone();
        let cwd = term.cwd.clone();

        div()
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
                            .font_semibold()
                            .text_color(rgb(MUTED))
                            .child("SELECTED AGENT"),
                    )
                    .child(
                        div()
                            .mt_3()
                            .text_xl()
                            .font_semibold()
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

    fn meta_row(name: &'static str, value: String) -> impl IntoElement {
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
            .child(
                div()
                    .flex_1()
                    .text_sm()
                    .text_color(rgb(INK))
                    .child(value),
            )
    }

    fn inspector(&self) -> impl IntoElement {
        let count = self.snapshot.terminals.len();
        let running = self.snapshot.terminals.iter().filter(|t| t.lifecycle == "live").count();
        let needs_you = self.snapshot.terminals.iter().filter(|t| t.reported_status.as_deref() == Some("blocked")).count();
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
            .child(div().text_xs().font_semibold().text_color(rgb(MUTED)).child("RUNTIME"))
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
            .child(div().text_xs().font_semibold().text_color(rgb(MUTED)).child("CONNECTION"))
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

impl Render for Workspace {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(BG))
            .text_color(rgb(INK))
            .child(self.topbar(cx))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .child(self.sidebar(cx))
                    .child(self.detail(cx))
                    .child(self.inspector()),
            )
    }
}

fn main() {
    application().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1280.0), px(790.0)), cx);
        if let Err(error) = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_, cx| cx.new(Workspace::new),
        ) {
            eprintln!("Unable to open ukk Desktop window: {error}");
            return;
        }
        cx.activate(true);
    });
}
