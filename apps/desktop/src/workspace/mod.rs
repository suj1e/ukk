//! Native ukk GPUI workspace: state orchestration, native panes, and Agent views.
//! herdr-core processes and SQLite data are owned outside this window.

use crate::theme::*;
use gpui::{div, prelude::*, px, rgb, Context, Render, Window};
use serde_json::{json, Value};
use std::time::Duration;
use ukk_domain::{new_id, DomainSnapshot, PaneContent, PaneNode, Session, SplitAxis};
use ukk_host_client::{HostClient, HostSnapshot, LiveHostState, Terminal};

mod actions;
mod agent_detail;
mod panes;
mod render;
mod sidebar;
mod state;
mod toolbar;

#[derive(Clone, Copy, PartialEq, Eq)]
enum DetailTab {
    Overview,
    Runtime,
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
}
