//! Native ukk GPUI workspace: state orchestration, native panes, and Agent views.
//! herdr-core processes and SQLite data are owned outside this window.

use crate::theme::*;
use gpui::{div, prelude::*, px, rgb, Context, Render, Role, Window};
use serde_json::{json, Value};
use std::time::Duration;
use ukk_domain::{
    new_id, DomainSnapshot, PaneContent, PaneNode, PaneTab, Session, SessionLayout, SplitAxis,
};
use ukk_host_client::{HostClient, HostSnapshot, LiveHostState, Terminal, WorkspaceUpdate};

mod actions;
mod agent_bind;
mod agent_detail;
mod focus_store;
mod keymap;
mod navigation;
mod omp;
mod pane_focus;
mod panes;
mod plugins;
mod render;
mod repository;
mod repository_load;
mod repository_view;
mod resize;
mod sidebar;
mod spaces;
mod splits;
mod state;
mod tabs;
mod terminal;
mod terminal_buffer;
mod toolbar;
mod unsupported_surface;

#[derive(Clone, Copy, PartialEq, Eq)]
enum DetailTab {
    Overview,
    Runtime,
}

/// A drag gesture belongs to one split node, never an Agent process.
#[derive(Clone)]
struct DraggedSplitHandle {
    split_id: String,
}

/// Drag payload carries only local Session and view identifiers. It must
/// never grant process control or move a Tab across different Sessions.
#[derive(Clone)]
struct DraggedPaneTab {
    session_id: String,
    source_group_id: String,
    tab_id: String,
}

struct SplitDragPreview {
    session_id: String,
    split_id: String,
    ratio: f32,
}

/// A stale SQLite CAS rejection. Retrying is an explicit user decision; the
/// Desktop never rebases and re-sends a layout write silently.
#[derive(Clone)]
struct ConflictNotice {
    message: String,
    /// Reapplied once after an explicit reload, never automatically.
    action: Option<PaneAction>,
    /// Session the failed intent belonged to. Retry must never translate the
    /// same IDs into a different Session's layout.
    session_id: Option<String>,
}

/// What a successful mutation should change in local presentation state.
/// IDs still come from the Host response, never from optimistic guessing.
#[derive(Clone, Copy, PartialEq, Eq)]
enum MutationIntent {
    None,
    CreatedSession,
    CreatedSpace,
    RenamedSpace,
    BoundAgentRun,
}

#[derive(Clone)]
enum PaneAction {
    Split {
        pane_id: String,
        axis: SplitAxis,
        content: PaneContent,
    },
    Close {
        pane_id: String,
    },
    Resize {
        split_id: String,
        ratio: f32,
    },
    TabAdd {
        pane_id: String,
        title: String,
        content: PaneContent,
    },
    TabClose {
        group_id: String,
        tab_id: String,
    },
    TabReorder {
        group_id: String,
        tab_id: String,
        direction: i32,
    },
    TabMove {
        source_group_id: String,
        destination_pane_id: String,
        tab_id: String,
    },
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
    split_drag_preview: Option<SplitDragPreview>,
    /// Per-device focus, persisted under the user profile. Membership and
    /// ordering remain shared Host layout; focus is never uploaded.
    local_focus: focus_store::LocalFocus,
    focused_root: gpui::FocusHandle,
    focus_path: Option<std::path::PathBuf>,
    focus_loaded: bool,
    focus_save_enabled: bool,
    focus_save_error: Option<String>,
    focus_save_generation: u64,
    focus_save_in_flight: bool,
    focus_save_pending: bool,
    /// User changed focus before the settings file finished loading.
    focus_touched: bool,
    pending_tab_focus: Option<pane_focus::PendingFocus>,
    pending_pane_focus: Option<String>,
    /// Explicit retry: applied after one successful reload, never in a loop.
    retry_after_reload: Option<(String, PaneAction)>,
    /// Action whose Host response is pending, used to offer an explicit retry.
    in_flight_action: Option<PaneAction>,
    /// Session owning `in_flight_action` (focus can switch before the reply).
    in_flight_session: Option<String>,
    /// A real Host workspace snapshot has been applied at least once.
    workspace_snapshot_loaded: bool,
    conflict: Option<ConflictNotice>,
    online: bool,
    /// Live herdr-core identity from the Host snapshot. Unknown is not a
    /// reason to guess; binding stays disabled until it is reported.
    core_id: Option<String>,
    message: Option<String>,
    host_epoch: String,
    host_revision: u64,
    workspace_epoch: String,
    workspace_revision: i64,
    workspace_sync_error: Option<String>,
    /// Read-only Files/Diff/Activity inspection for the selected Session.
    repository: repository::RepositorySurfaces,
    /// Host plugin broker projection. This contains descriptors only, never
    /// helper output or a capability token.
    plugin_snapshot: ukk_host_client::PluginSnapshot,
    plugin_snapshot_error: Option<String>,
    plugin_refreshing: bool,
    plugin_busy: bool,
    plugin_notice: Option<plugins::PluginNotice>,
    /// Explicit read-only terminal observers, keyed by pane or detail view.
    terminal_feeds: terminal::TerminalFeeds,
    /// Explicit one-line terminal input. Local UI state only; never a
    /// keystroke stream and never cleared without a verified Host reply.
    terminal_input: terminal::TerminalInputState,
    terminal_input_focus: gpui::FocusHandle,
    /// One read-only `runtime.status` request at a time.
    terminal_capability_in_flight: bool,
    /// Selected `session.tab` view id. Selection is local and never invokes.
    plugin_active_tab: Option<String>,
    /// Host-owned omp RPC conversation. Local UI state only; no layout or
    /// domain schema migration in this slice.
    omp: omp::OmpPanel,
    omp_input_focus: gpui::FocusHandle,
    /// Explicit Space creation/rename form. Text is local until submit.
    space_editor: spaces::SpaceEditor,
    space_title_focus: gpui::FocusHandle,
    space_root_focus: gpui::FocusHandle,
    /// Explicit AgentRun binding confirmation. Closed by default; never
    /// opened by terminal selection or a workspace refresh.
    agent_bind: agent_bind::AgentBindEditor,
    agent_bind_adapter_focus: gpui::FocusHandle,
    agent_bind_notice: Option<String>,
}

impl Workspace {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focused_root = cx.focus_handle();
        window.focus(&focused_root, cx);
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
            split_drag_preview: None,
            local_focus: focus_store::LocalFocus::default(),
            focused_root,
            focus_path: None,
            focus_loaded: false,
            focus_save_enabled: false,
            focus_save_error: None,
            focus_save_generation: 0,
            focus_save_in_flight: false,
            focus_save_pending: false,
            focus_touched: false,
            pending_tab_focus: None,
            pending_pane_focus: None,
            retry_after_reload: None,
            in_flight_action: None,
            in_flight_session: None,
            workspace_snapshot_loaded: false,
            conflict: None,
            online: false,
            core_id: None,
            message: None,
            host_epoch: String::new(),
            host_revision: 0,
            workspace_epoch: String::new(),
            workspace_revision: -1,
            workspace_sync_error: None,
            repository: repository::RepositorySurfaces::default(),
            plugin_snapshot: ukk_host_client::PluginSnapshot::default(),
            plugin_snapshot_error: None,
            plugin_refreshing: false,
            plugin_busy: false,
            plugin_notice: None,
            terminal_feeds: terminal::TerminalFeeds::default(),
            terminal_input: terminal::TerminalInputState::default(),
            terminal_input_focus: cx.focus_handle(),
            terminal_capability_in_flight: false,
            plugin_active_tab: None,
            omp: omp::OmpPanel::default(),
            omp_input_focus: cx.focus_handle(),
            space_editor: spaces::SpaceEditor::default(),
            space_title_focus: cx.focus_handle(),
            space_root_focus: cx.focus_handle(),
            agent_bind: agent_bind::AgentBindEditor::default(),
            agent_bind_adapter_focus: cx.focus_handle(),
            agent_bind_notice: None,
        };
        view.watch_updates(cx);
        view.watch_workspace(cx);
        view.watch_plugins(cx);
        view.watch_terminals(cx);
        view.refresh(cx);
        view.load_local_focus(cx);
        view
    }
}

/// Registered once at application startup.
pub(crate) fn key_bindings() -> Vec<gpui::KeyBinding> {
    keymap::bindings()
}
