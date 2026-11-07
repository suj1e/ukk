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
mod agent_detail;
mod focus_store;
mod keymap;
mod navigation;
mod pane_focus;
mod panes;
mod render;
mod resize;
mod sidebar;
mod splits;
mod state;
mod tabs;
mod toolbar;

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
    retry_after_reload: Option<PaneAction>,
    /// Action whose Host response is pending, used to offer an explicit retry.
    in_flight_action: Option<PaneAction>,
    conflict: Option<ConflictNotice>,
    online: bool,
    message: Option<String>,
    host_epoch: String,
    host_revision: u64,
    workspace_epoch: String,
    workspace_revision: i64,
    workspace_sync_error: Option<String>,
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
            conflict: None,
            online: false,
            message: None,
            host_epoch: String::new(),
            host_revision: 0,
            workspace_epoch: String::new(),
            workspace_revision: -1,
            workspace_sync_error: None,
        };
        view.watch_updates(cx);
        view.watch_workspace(cx);
        view.refresh(cx);
        view.load_local_focus(cx);
        view
    }
}

/// Registered once at application startup.
pub(crate) fn key_bindings() -> Vec<gpui::KeyBinding> {
    keymap::bindings()
}
