//! Runtime subscription, snapshot reconciliation and foreground refresh.
use super::*;

impl Workspace {
    pub(super) fn selected_terminal(&self) -> Option<&Terminal> {
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
