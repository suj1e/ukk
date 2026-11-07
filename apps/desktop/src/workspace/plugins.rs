//! Session surfaces contributed by the Host plugin broker.
//!
//! The Desktop never runs a plugin on load. It reads `plugin.snapshot` through
//! the same-user Host RPC, renders the declared Session action/tab/overlay, and
//! calls `plugin.invoke` only from an explicit click. The separate
//! `plugin.control` token is read by `HostClient` at invoke time and never
//! enters UI state, logs or snapshots.
use super::*;
use ukk_host_client::{HostError, PluginOverlay, PluginSnapshot, PluginView};

/// Last explicit invocation result, scoped to the Session it was made in.
#[derive(Clone)]
pub(super) struct PluginNotice {
    pub(super) session_id: String,
    pub(super) view_id: Option<String>,
    pub(super) plugin_id: String,
    pub(super) command: String,
    pub(super) state: PluginNoticeState,
}

#[derive(Clone)]
pub(super) enum PluginNoticeState {
    Succeeded {
        provenance: String,
        value: serde_json::Value,
    },
    Denied {
        kind: String,
        message: String,
    },
    LocalError {
        kind: String,
        message: String,
    },
}

/// Display bounds for one overlay. The descriptor is already capped at
/// 1080 px by manifest validation; the Desktop additionally keeps it small
/// enough for the content column and never lets it take over the Session.
pub(super) fn overlay_display_size(overlay: &PluginOverlay) -> (f32, f32) {
    let width = overlay.max_width.clamp(120, 1080).min(420) as f32;
    let height = overlay.max_height.clamp(120, 1080).min(320) as f32;
    (width, height)
}

/// Pick the selected plugin tab, falling back to the first declared tab.
pub(super) fn active_plugin_tab<'a>(
    snapshot: &'a PluginSnapshot,
    selected: Option<&str>,
) -> Option<&'a PluginView> {
    selected
        .and_then(|view_id| {
            snapshot
                .session_tabs
                .iter()
                .find(|view| view.view_id == view_id)
        })
        .or_else(|| snapshot.session_tabs.first())
}

/// Escape control characters and bidi overrides before rendering text that a
/// helper produced. Snapshot/error text is untrusted even though the Host
/// already redacts its own diagnostics.
pub(super) fn sanitize_untrusted(text: &str) -> String {
    let mut output = String::with_capacity(text.len().min(512));
    for (written, character) in text.chars().enumerate() {
        if written >= 400 {
            output.push('…');
            break;
        }
        if character.is_control()
            || matches!(character, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        {
            output.push(' ');
        } else {
            output.push(character);
        }
    }
    output
}

/// Map a Host RPC failure to a stable typed label for the UI.
pub(super) fn plugin_error_kind(error: &HostError) -> (String, String) {
    match error {
        HostError::Remote(value) => {
            let kind = value
                .pointer("/data/kind")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("plugin_error")
                .to_owned();
            let message = value
                .get("message")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("plugin command failed")
                .to_owned();
            (kind, sanitize_untrusted(&message))
        }
        HostError::Protocol(message) => (
            "plugin_control_unavailable".into(),
            sanitize_untrusted(message),
        ),
        other => ("local_error".into(), sanitize_untrusted(&other.to_string())),
    }
}

impl Workspace {
    /// Refresh the read-only plugin snapshot through HostClient.
    pub(super) fn refresh_plugins(&mut self, cx: &mut Context<Self>) {
        if self.plugin_refreshing {
            return;
        }
        self.plugin_refreshing = true;
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async { HostClient::from_env().plugin_snapshot() })
                .await;
            this.update(cx, |view, cx| {
                view.plugin_refreshing = false;
                match result {
                    Ok(snapshot) => {
                        if snapshot != view.plugin_snapshot {
                            view.plugin_snapshot = snapshot;
                            cx.notify();
                        }
                        view.plugin_snapshot_error = None;
                    }
                    Err(error) => {
                        view.plugin_snapshot_error = Some(error.to_string());
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    /// Periodic read-only poll so an operator's CLI enable/disable becomes
    /// visible without a Desktop restart. It never invokes a command.
    pub(super) fn watch_plugins(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| loop {
            let result = cx
                .background_executor()
                .spawn(async { HostClient::from_env().plugin_snapshot() })
                .await;
            let keep_going = this
                .update(cx, |view, cx| {
                    match result {
                        Ok(snapshot) => {
                            if snapshot != view.plugin_snapshot {
                                view.plugin_snapshot = snapshot;
                                cx.notify();
                            }
                            view.plugin_snapshot_error = None;
                        }
                        Err(error) => {
                            let message = error.to_string();
                            if view.plugin_snapshot_error.as_deref() != Some(message.as_str()) {
                                view.plugin_snapshot_error = Some(message);
                                cx.notify();
                            }
                        }
                    }
                    true
                })
                .unwrap_or(false);
            if !keep_going {
                break;
            }
            cx.background_executor().timer(Duration::from_secs(5)).await;
        })
        .detach();
    }

    pub(super) fn clear_plugin_notice(&mut self, cx: &mut Context<Self>) {
        self.plugin_notice = None;
        cx.notify();
    }

    /// Explicit user click: invoke one declared command for the selected
    /// Session. No plugin code runs on load, refresh or tab selection.
    pub(super) fn invoke_plugin_view(
        &mut self,
        plugin_id: String,
        command: String,
        view_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if self.plugin_busy {
            return;
        }
        let Some(session_id) = self.selected_session.clone() else {
            return;
        };
        self.plugin_busy = true;
        self.plugin_notice = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let target_session = session_id.clone();
            let background_plugin = plugin_id.clone();
            let background_command = command.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    HostClient::from_env().invoke_plugin(
                        &background_plugin,
                        &background_command,
                        Some(&target_session),
                        serde_json::Value::Null,
                    )
                })
                .await;
            this.update(cx, |view, cx| {
                view.plugin_busy = false;
                view.plugin_notice = Some(match result {
                    Ok(outcome) => PluginNotice {
                        session_id: session_id.clone(),
                        view_id: view_id.clone(),
                        plugin_id: outcome.plugin_id,
                        command: outcome.command,
                        state: PluginNoticeState::Succeeded {
                            provenance: outcome.provenance,
                            value: outcome.value,
                        },
                    },
                    Err(error) => {
                        let (kind, message) = plugin_error_kind(&error);
                        let local = matches!(error, HostError::Protocol(_) | HostError::Io(_));
                        PluginNotice {
                            session_id: session_id.clone(),
                            view_id: view_id.clone(),
                            plugin_id: plugin_id.clone(),
                            command: command.clone(),
                            state: if local {
                                PluginNoticeState::LocalError { kind, message }
                            } else {
                                PluginNoticeState::Denied { kind, message }
                            },
                        }
                    }
                });
                cx.notify();
            })
            .ok();
            // Status may change on failure (for example a crashed helper).
            // This is still a read-only refresh, not a retry.
            this.update(cx, |view, cx| view.refresh_plugins(cx)).ok();
        })
        .detach();
    }

    pub(super) fn plugin_notice_for(&self, session_id: &str) -> Option<&PluginNotice> {
        self.plugin_notice
            .as_ref()
            .filter(|notice| notice.session_id == session_id)
    }

    /// Session surfaces: action row, plugin tab strip/panel and bounded overlay
    /// cards. Rendering is pure; only the explicit run/action clicks invoke.
    pub(super) fn session_plugin_section(
        &self,
        session_id: &str,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let actions = self.plugin_snapshot.session_actions.clone();
        let tabs = self.plugin_snapshot.session_tabs.clone();
        let overlays = self.plugin_snapshot.session_overlays.clone();
        let failed: Vec<String> = self
            .plugin_snapshot
            .plugins
            .iter()
            .filter_map(|plugin| {
                plugin
                    .failure
                    .as_ref()
                    .map(|failure| format!("Plugin {} failed: {}", plugin.plugin_id, failure))
            })
            .collect();
        let has_problem = self.plugin_snapshot_error.is_some()
            || self.plugin_snapshot.registry_error.is_some()
            || !failed.is_empty();
        if actions.is_empty() && tabs.is_empty() && overlays.is_empty() && !has_problem {
            return div().into_any_element();
        }

        let mut section = div()
            .id(format!("plugin-session-section-{session_id}"))
            .flex()
            .flex_col()
            .gap_2()
            .mx_6()
            .mb_3()
            .p_3()
            .rounded_lg()
            .border_1()
            .border_color(rgb(LINE))
            .bg(rgb(WHITE))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgb(MUTED))
                            .child("SESSION PLUGINS · HOST BROKER"),
                    )
                    .child(div().text_xs().text_color(rgb(MUTED)).child(
                        if self.plugin_snapshot.control_enabled {
                            "plugin.control: enabled · runs only on click"
                        } else {
                            "plugin.control: disabled · invoke will be refused"
                        },
                    )),
            );

        if let Some(error) = self.plugin_snapshot_error.clone() {
            section = section.child(self.plugin_problem("host", &error));
        }
        if let Some(error) = self.plugin_snapshot.registry_error.clone() {
            section = section.child(self.plugin_problem("registry", &error));
        }
        for failure in failed {
            section = section.child(self.plugin_problem("failed", &failure));
        }
        if !self.plugin_snapshot.control_enabled
            && !(actions.is_empty() && tabs.is_empty() && overlays.is_empty())
        {
            let detail = self
                .plugin_snapshot
                .control_problem
                .clone()
                .unwrap_or_else(|| {
                    "plugin-control token is not provisioned for this Host".to_owned()
                });
            section = section.child(self.plugin_problem("plugin.control", &detail));
        }

        if !actions.is_empty() {
            let busy = self.plugin_busy;
            section = section.child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .child(div().text_xs().text_color(rgb(MUTED)).child("ACTIONS"))
                    .children(actions.into_iter().map(|view| {
                        let plugin_id = view.plugin_id;
                        let view_id = view.view_id;
                        let command = view.command;
                        let id = format!("plugin-action-{plugin_id}-{view_id}");
                        let title = view.title;
                        div()
                            .id(id)
                            .role(Role::Button)
                            .aria_label(format!("Run plugin action {title}"))
                            .focusable()
                            .focus_visible(|style| style.border_color(rgb(ACCENT)))
                            .px_3()
                            .py_2()
                            .rounded_md()
                            .border_1()
                            .border_color(rgb(LINE))
                            .bg(rgb(if command.is_some() && !busy {
                                SURFACE
                            } else {
                                WHITE
                            }))
                            .text_xs()
                            .text_color(rgb(if command.is_some() { ACCENT } else { MUTED }))
                            .cursor_pointer()
                            .when(busy, |button| button.opacity(0.6))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(command) = command.clone() {
                                    this.invoke_plugin_view(
                                        plugin_id.clone(),
                                        command,
                                        Some(view_id.clone()),
                                        cx,
                                    );
                                }
                            }))
                            .child(title)
                    })),
            );
        }

        if !tabs.is_empty() {
            let active =
                active_plugin_tab(&self.plugin_snapshot, self.plugin_active_tab.as_deref())
                    .map(|view| view.view_id.clone());
            let busy = self.plugin_busy;
            let tab_buttons = tabs.iter().map(|view| {
                let view_id = view.view_id.clone();
                let selected = active.as_deref() == Some(view_id.as_str());
                let title = view.title.clone();
                div()
                    .id(format!("plugin-tab-{}-{view_id}", view.plugin_id))
                    .role(Role::Tab)
                    .aria_label(format!("Plugin tab {title}"))
                    .aria_selected(selected)
                    .focusable()
                    .focus_visible(|style| style.border_color(rgb(ACCENT)))
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(if selected { ACCENT } else { SURFACE }))
                    .bg(rgb(if selected { WHITE } else { SURFACE }))
                    .text_xs()
                    .text_color(rgb(if selected { ACCENT } else { MUTED }))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.plugin_active_tab = Some(view_id.clone());
                        cx.notify();
                    }))
                    .child(title)
            });
            let active_view =
                active_plugin_tab(&self.plugin_snapshot, self.plugin_active_tab.as_deref())
                    .cloned();
            let panel: gpui::AnyElement = match active_view {
                Some(view) => {
                    let plugin_id = view.plugin_id.clone();
                    let view_id = view.view_id.clone();
                    let command = view.command.clone();
                    let title = view.title;
                    let command_label = command
                        .clone()
                        .unwrap_or_else(|| "no command bound".to_owned());
                    div()
                        .id(format!("plugin-tab-panel-{plugin_id}-{view_id}"))
                        .role(Role::TabPanel)
                        .aria_label(format!("Plugin tab {title}"))
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .p_3()
                        .rounded_md()
                        .bg(rgb(SURFACE))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .text_color(rgb(INK))
                                        .child(title),
                                )
                                .child(div().text_xs().text_color(rgb(MUTED)).child(format!(
                                    "{command_label} · helper output is untrusted"
                                ))),
                        )
                        .when_some(command, |panel, command| {
                            panel.child(
                                div()
                                    .id(format!("plugin-tab-run-{plugin_id}-{view_id}"))
                                    .role(Role::Button)
                                    .aria_label("Run the selected plugin tab command".to_owned())
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
                                    .cursor_pointer()
                                    .when(busy, |button| button.opacity(0.6))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.invoke_plugin_view(
                                            plugin_id.clone(),
                                            command.clone(),
                                            Some(view_id.clone()),
                                            cx,
                                        );
                                    }))
                                    .child("Run"),
                            )
                        })
                        .into_any_element()
                }
                None => div()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child("No plugin tab selected")
                    .into_any_element(),
            };
            section = section.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .id(format!("plugin-tab-list-{session_id}"))
                            .role(Role::TabList)
                            .aria_label("Session plugin tabs")
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap_1()
                            .children(tab_buttons),
                    )
                    .child(panel),
            );
        }

        if !overlays.is_empty() {
            let busy = self.plugin_busy;
            section = section.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(MUTED))
                            .child("OVERLAYS · bounded, never a full-view takeover"),
                    )
                    .children(overlays.into_iter().map(|view| {
                        let (width, height) = view
                            .overlay
                            .as_ref()
                            .map(overlay_display_size)
                            .unwrap_or((280.0, 160.0));
                        let anchor = view
                            .overlay
                            .as_ref()
                            .map(|overlay| overlay.anchor.clone())
                            .unwrap_or_else(|| "top-right".to_owned());
                        let plugin_id = view.plugin_id;
                        let view_id = view.view_id;
                        let command = view.command;
                        let title = view.title;
                        div()
                            .id(format!("plugin-overlay-{plugin_id}-{view_id}"))
                            .role(Role::Group)
                            .aria_label(format!("Plugin overlay {title}, anchored {anchor}"))
                            .w(px(width))
                            .max_w(px(width))
                            .max_h(px(height))
                            .p_3()
                            .rounded_lg()
                            .border_1()
                            .border_color(rgb(ACCENT))
                            .bg(rgb(WHITE))
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(MUTED))
                                    .child(format!("OVERLAY · {anchor} · ≤ {width}×{height}")),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(rgb(INK))
                                    .child(title),
                            )
                            .when_some(command, |card, command| {
                                card.child(
                                    div()
                                        .id(format!("plugin-overlay-run-{plugin_id}-{view_id}"))
                                        .role(Role::Button)
                                        .aria_label("Run the plugin overlay command".to_owned())
                                        .focusable()
                                        .focus_visible(|style| style.border_color(rgb(ACCENT)))
                                        .px_2()
                                        .py_1()
                                        .rounded_md()
                                        .border_1()
                                        .border_color(rgb(LINE))
                                        .text_xs()
                                        .text_color(rgb(ACCENT))
                                        .cursor_pointer()
                                        .when(busy, |button| button.opacity(0.6))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.invoke_plugin_view(
                                                plugin_id.clone(),
                                                command.clone(),
                                                Some(view_id.clone()),
                                                cx,
                                            );
                                        }))
                                        .child("Run"),
                                )
                            })
                    })),
            );
        }

        if let Some(notice) = self.plugin_notice_for(session_id).cloned() {
            section = section.child(self.plugin_notice_element(&notice, cx));
        }

        section.into_any_element()
    }

    fn plugin_problem(&self, label: &str, detail: &str) -> gpui::AnyElement {
        div()
            .p_2()
            .rounded_md()
            .bg(rgb(0xfff4e5))
            .text_xs()
            .text_color(rgb(0x7a4b1e))
            .child(format!("{label}: {}", sanitize_untrusted(detail)))
            .into_any_element()
    }

    fn plugin_notice_element(
        &self,
        notice: &PluginNotice,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let (label, detail, background, ink) = match &notice.state {
            PluginNoticeState::Succeeded { provenance, value } => (
                format!("{} · {provenance}", notice.command),
                sanitize_untrusted(
                    &serde_json::to_string(value).unwrap_or_else(|_| "<unprintable>".into()),
                ),
                0xeef7f0,
                0x2f6b45,
            ),
            PluginNoticeState::Denied { kind, message } => (
                format!("Denied · {kind}"),
                format!("{} · {}", notice.command, sanitize_untrusted(message)),
                0xffefeb,
                0xa14b32,
            ),
            PluginNoticeState::LocalError { kind, message } => (
                format!("Unavailable · {kind}"),
                sanitize_untrusted(message),
                0xfff4e5,
                0x7a4b1e,
            ),
        };
        div()
            .id("plugin-invoke-notice")
            .role(Role::Alert)
            .aria_label(format!(
                "Plugin result for {} via {}: {label}",
                notice.plugin_id,
                notice.view_id.as_deref().unwrap_or(notice.command.as_str())
            ))
            .flex()
            .flex_wrap()
            .items_center()
            .justify_between()
            .gap_2()
            .p_3()
            .rounded_md()
            .bg(rgb(background))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .min_w_0()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgb(ink))
                            .child(label),
                    )
                    .child(div().text_xs().text_color(rgb(ink)).child(detail)),
            )
            .child(
                div()
                    .id("plugin-notice-dismiss")
                    .role(Role::Button)
                    .aria_label("Dismiss the plugin result")
                    .focusable()
                    .focus_visible(|style| style.border_color(rgb(ACCENT)))
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(ink))
                    .text_xs()
                    .text_color(rgb(ink))
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, _, cx| this.clear_plugin_notice(cx)))
                    .child("Dismiss"),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ukk_host_client::{PluginCommand, PluginStatusView};

    fn view(view_id: &str, slot: &str) -> PluginView {
        PluginView {
            plugin_id: "dev.ukk.sample.session-action".into(),
            view_id: view_id.into(),
            slot: slot.into(),
            title: view_id.into(),
            command: Some("sample.session.ping".into()),
            overlay: None,
        }
    }

    fn snapshot() -> PluginSnapshot {
        PluginSnapshot {
            capability: "separate-plugin-control".into(),
            control_enabled: false,
            control_problem: None,
            registry_path: "/tmp/registry.json".into(),
            registry_loaded: true,
            registry_present: true,
            registry_error: None,
            plugins: vec![PluginStatusView {
                plugin_id: "dev.ukk.sample.session-action".into(),
                trust: "first-party".into(),
                state: "ready".into(),
                installed: true,
                enabled_by_registry: true,
                command_count: 1,
                view_count: 3,
                queued_events: 0,
                dropped_events: 0,
                failure: None,
            }],
            session_actions: vec![view("sample.session.action", "session.action")],
            session_tabs: vec![view("sample.session.tab", "session.tab")],
            session_overlays: vec![view("sample.session.overlay", "session.overlay")],
            commands: vec![PluginCommand {
                plugin_id: "dev.ukk.sample.session-action".into(),
                command_id: "sample.session.ping".into(),
                title: "Sample: ping this session".into(),
                scope: "session".into(),
                requires: Vec::new(),
            }],
        }
    }

    #[test]
    fn contributions_map_to_distinct_session_surfaces() {
        let snapshot = snapshot();
        assert_eq!(snapshot.session_actions[0].slot, "session.action");
        assert_eq!(snapshot.session_tabs[0].slot, "session.tab");
        assert_eq!(snapshot.session_overlays[0].slot, "session.overlay");
        assert_eq!(
            snapshot.session_actions[0].command.as_deref(),
            Some("sample.session.ping")
        );
        // Commands are menu metadata, not an implicit fourth view.
        assert_eq!(snapshot.commands.len(), 1);
    }

    #[test]
    fn plugin_tab_selection_falls_back_to_first_declared_tab() {
        let snapshot = snapshot();
        assert_eq!(
            active_plugin_tab(&snapshot, Some("sample.session.tab"))
                .unwrap()
                .view_id,
            "sample.session.tab"
        );
        assert_eq!(
            active_plugin_tab(&snapshot, Some("missing"))
                .unwrap()
                .view_id,
            "sample.session.tab"
        );
        assert_eq!(
            active_plugin_tab(&snapshot, None).unwrap().view_id,
            "sample.session.tab"
        );
    }

    #[test]
    fn overlay_display_is_clamped_to_the_session_column() {
        let overlay = PluginOverlay {
            anchor: "top-right".into(),
            max_width: 1080,
            max_height: 1080,
        };
        assert_eq!(overlay_display_size(&overlay), (420.0, 320.0));
        let tiny = PluginOverlay {
            anchor: "center".into(),
            max_width: 40,
            max_height: 40,
        };
        assert_eq!(overlay_display_size(&tiny), (120.0, 120.0));
    }

    #[test]
    fn typed_denial_states_are_preserved_and_sanitized() {
        let error = HostError::Remote(serde_json::json!({
            "code": -32065,
            "message": "plugin `x` is not ready (state: disabled)\n\u{1b}[31m",
            "data": {"kind": "not_enabled"}
        }));
        let (kind, message) = plugin_error_kind(&error);
        assert_eq!(kind, "not_enabled");
        assert!(message.contains("not ready"));
        assert!(!message.contains('\n'));
        assert!(!message.contains('\u{1b}'));
    }

    #[test]
    fn helper_output_is_bounded_and_stripped() {
        let text = format!("{}\u{1b}[2J\u{202e}", "x".repeat(600));
        let sanitized = sanitize_untrusted(&text);
        assert!(!sanitized.contains('\u{1b}'));
        assert!(!sanitized.contains('\u{202e}'));
        assert!(sanitized.chars().count() <= 401);
    }
}
