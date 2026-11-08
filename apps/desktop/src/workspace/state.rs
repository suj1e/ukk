//! Runtime subscription, snapshot reconciliation and foreground refresh.
use super::focus_store::{pane_entries, pane_order, recovery_target};
use super::pane_focus::remote_tab_focus;
use super::*;

impl Workspace {
    pub(super) fn selected_terminal(&self) -> Option<&Terminal> {
        let id = self.selected.as_deref()?;
        self.snapshot
            .terminals
            .iter()
            .find(|terminal| terminal.address == id)
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
        if !self.selected.as_ref().is_some_and(|address| {
            live.snapshot
                .terminals
                .iter()
                .any(|t| &t.address == address)
        }) {
            self.selected = None; // A Space/Session should be the initial focus.
        }
        self.snapshot = live.snapshot;
        self.terminal_feeds.retain_known_addresses(&self.snapshot);
    }

    /// A long-poll task stays alive only as long as this GPUI entity exists.
    /// Neither rendering nor click handlers ever block on herdr-core.
    pub(super) fn watch_updates(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let mut epoch = String::new();
            let mut revision = 0_u64;
            loop {
                let last_epoch = epoch.clone();
                let result =
                    cx.background_executor()
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
                                let was_online = view.online;
                                view.apply_host_state(live);
                                // The Host may have been unreachable when the
                                // last capability read failed. Re-read the
                                // read-only status once connectivity returns;
                                // this never retries a send.
                                if !was_online && view.online {
                                    view.refresh_terminal_capability(cx);
                                }
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

    /// Domain metadata has its own durable SQLite cursor, independent from
    /// the volatile herdr-core Agent event stream. Preserve user selection
    /// whenever the corresponding Space/Session still exists.
    pub(super) fn apply_workspace_update(
        &mut self,
        update: WorkspaceUpdate,
        cx: &mut Context<Self>,
    ) {
        let Some(domain) = update.snapshot else {
            return; // normal long-poll timeout
        };
        if self.workspace_epoch == update.db_epoch && update.revision < self.workspace_revision {
            return; // older concurrent response, do not overwrite new layout
        }
        self.workspace_epoch = update.db_epoch;
        self.workspace_revision = update.revision;
        self.workspace_sync_error = None;

        // Capture the old focus target and visual order before replacing the
        // tree, so a remote deletion can hand focus to a real neighbour.
        let previous_session = self.selected_session.clone();
        let previous_pane = previous_session
            .as_deref()
            .and_then(|session_id| self.local_focus.pane(session_id))
            .map(str::to_owned);
        let previous_order = previous_session.as_deref().and_then(|session_id| {
            self.domain
                .layouts
                .iter()
                .find(|layout| layout.session_id == session_id)
                .map(|layout| pane_order(&layout.root))
        });
        // Capture active Tabs before reconciliation drops a remotely removed
        // ID, so the Desktop can move focus to a real neighbour.
        let previous_tab_focus: Vec<(String, String, Vec<String>)> = previous_session
            .as_deref()
            .and_then(|session_id| {
                let layout = self
                    .domain
                    .layouts
                    .iter()
                    .find(|layout| layout.session_id == session_id)?;
                let focus = self.local_focus.session(session_id)?;
                Some(
                    pane_entries(&layout.root)
                        .into_iter()
                        .filter(|entry| !entry.tabs.is_empty())
                        .filter_map(|entry| {
                            let active = focus.tabs.get(&entry.id)?;
                            entry
                                .tabs
                                .contains(active)
                                .then(|| (entry.id, active.clone(), entry.tabs))
                        })
                        .collect(),
                )
            })
            .unwrap_or_default();

        let mut focus_changed = self.local_focus.reconcile(&domain);

        if self
            .selected_space
            .as_ref()
            .is_some_and(|id| !domain.spaces.iter().any(|space| &space.id == id))
        {
            self.selected_space = None;
        }
        if self.selected_space.is_none() {
            self.selected_space = domain.spaces.first().map(|space| space.id.clone());
        }
        if self
            .selected_session
            .as_ref()
            .is_some_and(|id| !domain.sessions.iter().any(|session| &session.id == id))
        {
            self.selected_session = None;
        }
        if self.selected_session.is_none() && self.selected.is_none() {
            // A restarted Desktop restores its own Session choice, but only
            // when that Session still exists on this Host.
            let restored = self.local_focus.selected_session.clone().filter(|id| {
                domain
                    .sessions
                    .iter()
                    .any(|session| &session.id == id && !session.archived)
            });
            self.selected_session = restored.or_else(|| {
                domain
                    .sessions
                    .iter()
                    .find(|session| {
                        !session.archived
                            && self
                                .selected_space
                                .as_ref()
                                .is_none_or(|space| &session.space_id == space)
                    })
                    .map(|session| session.id.clone())
            });
        }
        if let Some(session_id) = self.selected_session.clone() {
            if let Some(session) = domain
                .sessions
                .iter()
                .find(|session| session.id == session_id)
            {
                self.selected_space = Some(session.space_id.clone());
            }
            self.local_focus.set_selected_session(Some(&session_id));
        }
        self.domain = domain;
        self.terminal_feeds.retain_layouts(&self.domain.layouts);
        self.workspace_snapshot_loaded = true;
        // The omp panel is scoped to one Session's Host conversation. A
        // selection change only stops the local long-poll; it never signals
        // the Host-owned child.
        if self.omp.open && self.omp.session_id.as_deref() != self.selected_session.as_deref() {
            self.close_omp(cx);
        }

        if let (Some(session_id), Some(previous_pane), Some(previous_order)) =
            (previous_session.clone(), previous_pane, previous_order)
        {
            if self.selected_session.as_deref() == Some(session_id.as_str()) {
                let next_order = self
                    .domain
                    .layouts
                    .iter()
                    .find(|layout| layout.session_id == session_id)
                    .map(|layout| pane_order(&layout.root))
                    .unwrap_or_default();
                if !next_order.contains(&previous_pane) {
                    if let Some(target) =
                        recovery_target(&previous_order, &previous_pane, &next_order)
                    {
                        self.local_focus.set_pane(&session_id, &target);
                        focus_changed = true;
                    }
                }
            }
        }
        if self.selected_session.as_deref() == previous_session.as_deref() {
            if let Some(session_id) = self.selected_session.clone() {
                if let Some(layout) = self
                    .domain
                    .layouts
                    .iter()
                    .find(|layout| layout.session_id == session_id)
                {
                    let entries = pane_entries(&layout.root);
                    for (group_id, active, old_tabs) in previous_tab_focus {
                        if let Some(resolved) =
                            remote_tab_focus(&old_tabs, &active, &group_id, &entries)
                        {
                            self.local_focus.set_pane(&session_id, &resolved.pane_id);
                            if let Some(tab_id) = resolved.tab_id {
                                self.local_focus
                                    .set_tab(&session_id, &resolved.pane_id, &tab_id);
                            }
                            focus_changed = true;
                        }
                    }
                }
            }
        }
        focus_changed |= self.apply_pending_focus();
        if focus_changed {
            self.persist_local_focus(cx);
        }
        // Re-read Git only when the selected Session or its project root
        // changed; this is a local, read-only inspection.
        self.sync_repository_surfaces(cx);
    }

    /// Listen for metadata writes from a second Desktop or the local CLI.
    /// Reads happen on the background executor; GPUI only applies snapshots.
    pub(super) fn watch_workspace(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let mut epoch = String::new();
            let mut revision = -1_i64;
            loop {
                let last_epoch = epoch.clone();
                let result = cx
                    .background_executor()
                    .spawn(async move {
                        HostClient::from_env().wait_for_workspace_change(&last_epoch, revision)
                    })
                    .await;
                match result {
                    Ok(update) => {
                        epoch = update.db_epoch.clone();
                        revision = update.revision;
                        if this
                            .update(cx, |view, cx| {
                                view.apply_workspace_update(update, cx);
                                view.workspace_sync_error = None;
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
                                view.workspace_sync_error =
                                    Some(format!("Workspace sync paused: {error}"));
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

    /// Refresh authoritative Host and SQLite snapshots off the UI thread.
    pub(super) fn refresh(&mut self, cx: &mut Context<Self>) {
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
                    (
                        client.host_snapshot(),
                        client.workspace_state(),
                        client.plugin_snapshot(),
                        client.runtime_status(),
                    )
                })
                .await;
            this.update(cx, |view, cx| {
                view.loading = false;
                let (host, domain, plugins, capability) = result;
                match host {
                    Ok(live) => view.apply_host_state(live),
                    Err(error) => {
                        view.online = false;
                        view.message = Some(error.to_string());
                        // Keep last known metadata, but visibly label it stale.
                    }
                }
                match domain {
                    Ok(update) => {
                        view.apply_workspace_update(update, cx);
                        view.apply_retry_after_reload(cx);
                    }
                    Err(error) => {
                        view.retry_after_reload = None;
                        view.workspace_sync_error =
                            Some(format!("Workspace storage unavailable: {error}"));
                    }
                }
                match plugins {
                    Ok(snapshot) => {
                        view.plugin_snapshot = snapshot;
                        view.plugin_snapshot_error = None;
                    }
                    Err(error) => {
                        view.plugin_snapshot_error = Some(error.to_string());
                    }
                }
                view.apply_terminal_capability(capability);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}
