//! Native ukk GPUI workspace: state orchestration, native panes, and Agent views.
//! herdr-core processes and SQLite data are owned outside this window.

use crate::theme::*;
use gpui::{div, prelude::*, px, rgb, Context, Render, Window};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::Duration;
use ukk_domain::{new_id, DomainSnapshot, PaneContent, PaneNode, PaneTab, Session, SplitAxis};
use ukk_host_client::{HostClient, HostSnapshot, LiveHostState, Terminal, WorkspaceUpdate};

mod actions;
mod agent_detail;
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
    /// Per-window active tabs; membership/order are shared via SQLite.
    tab_focus: HashMap<(String, String), String>,
    online: bool,
    message: Option<String>,
    host_epoch: String,
    host_revision: u64,
    workspace_epoch: String,
    workspace_revision: i64,
    workspace_sync_error: Option<String>,
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
            split_drag_preview: None,
            tab_focus: HashMap::new(),
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
        view
    }
}
