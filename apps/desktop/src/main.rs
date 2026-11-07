//! ukk Desktop — an experimental native GPUI shell.
//!
//! Unlike herdr-app, this is a new UI. The first screen displays only the
//! actual terminals exposed by a local ukk Host; no synthetic Agent activity,
//! fake chats, or invented providers. Agent runtime lives outside this window.

use gpui::{
    div, prelude::*, px, rgb, size, App, Bounds, Context, Window, WindowBounds, WindowOptions,
};
use gpui_platform::application;
use std::time::Duration;
use ukk_host_client::{HostClient, HostSnapshot, LiveHostState, Terminal};

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

struct Workspace {
    snapshot: HostSnapshot,
    selected: Option<String>,
    tab: DetailTab,
    loading: bool,
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
            tab: DetailTab::Overview,
            loading: false,
            online: false,
            message: None,
            host_epoch: String::new(),
            host_revision: 0,
        };
        view.watch_updates(cx);
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
            self.selected = live.snapshot.terminals.first().map(|t| t.address.clone());
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
                .spawn(async move { HostClient::from_env().host_snapshot() })
                .await;
            this.update(cx, |view, cx| {
                view.loading = false;
                match result {
                    Ok(live) => view.apply_host_state(live),
                    Err(error) => {
                        view.online = false;
                        view.message = Some(error.to_string());
                        // Keep last known metadata, but visibly label it stale.
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

    fn detail(&self, cx: &mut Context<Self>) -> impl IntoElement {
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

        body
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
