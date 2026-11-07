//! Keyboard-initiated Pane/Tab actions.
//!
//! Every command becomes the same authorized `PaneAction` as pointer
//! controls: Host authorization, local CAS and Agent process boundaries are
//! never bypassed.

use super::focus_store::{pane_entries, PaneEntry};
use super::pane_focus::{
    adjacent_content, adjusted_split_ratio, enclosing_split, resolve_focus, PendingFocus,
};
use super::*;

fn content_for(entry: &PaneEntry, layout: &SessionLayout, active_tab: Option<&str>) -> PaneContent {
    match entry.tabs.is_empty() {
        true => match find_node(&layout.root, &entry.id) {
            Some(PaneNode::Leaf { content, .. }) => content.clone(),
            _ => PaneContent::Activity,
        },
        false => entry
            .tabs
            .iter()
            .find(|tab| Some(tab.as_str()) == active_tab)
            .or_else(|| entry.tabs.first())
            .and_then(|tab| find_tab(&layout.root, tab))
            .map(|tab| tab.content.clone())
            .unwrap_or(PaneContent::Activity),
    }
}

fn find_node<'a>(node: &'a PaneNode, pane_id: &str) -> Option<&'a PaneNode> {
    match node {
        PaneNode::Split { first, second, .. } => {
            find_node(first, pane_id).or_else(|| find_node(second, pane_id))
        }
        _ => (node.id() == pane_id).then_some(node),
    }
}

fn find_tab<'a>(node: &'a PaneNode, tab_id: &str) -> Option<&'a PaneTab> {
    match node {
        PaneNode::Tabs { tabs, .. } => tabs.iter().find(|tab| tab.id == tab_id),
        PaneNode::Split { first, second, .. } => {
            find_tab(first, tab_id).or_else(|| find_tab(second, tab_id))
        }
        PaneNode::Leaf { .. } => None,
    }
}

impl Workspace {
    fn focused_layout(&self) -> Option<(String, SessionLayout)> {
        let session_id = self.selected_session.clone()?;
        let layout = self
            .domain
            .layouts
            .iter()
            .find(|layout| layout.session_id == session_id)?
            .clone();
        Some((session_id, layout))
    }

    pub(super) fn focused_entry(&self) -> Option<PaneEntry> {
        let (session_id, layout) = self.focused_layout()?;
        let entries = pane_entries(&layout.root);
        let focused = self.local_focus.pane(&session_id).map(str::to_owned);
        entries
            .iter()
            .find(|entry| Some(entry.id.as_str()) == focused.as_deref())
            .cloned()
            .or_else(|| entries.into_iter().next())
    }

    /// Locally remembered active Tab ID for a group, without validating it
    /// against the current tree. Rendering and actions go through
    /// `active_tab_id`, which falls back to the first visible Tab.
    fn focused_tab(&self, group_id: &str) -> Option<String> {
        let session_id = self.selected_session.as_deref()?;
        self.local_focus
            .tab(session_id, group_id)
            .map(str::to_owned)
    }

    /// Used by rendering to draw a visible keyboard focus ring.
    pub(super) fn is_focused_pane(&self, pane_id: &str) -> bool {
        self.selected_session.is_some()
            && self
                .focused_entry()
                .is_some_and(|entry| entry.id == pane_id)
    }

    pub(super) fn active_tab_id<'a>(&self, group_id: &str, tabs: &'a [PaneTab]) -> Option<&'a str> {
        let session_id = self.selected_session.as_deref()?;
        let focused = self.local_focus.tab(session_id, group_id);
        tabs.iter()
            .find(|tab| Some(tab.id.as_str()) == focused)
            .or_else(|| tabs.first())
            .map(|tab| tab.id.as_str())
    }

    fn focus_pane(&mut self, pane_id: &str, cx: &mut Context<Self>) {
        let Some(session_id) = self.selected_session.clone() else {
            return;
        };
        self.focus_touched = true;
        self.local_focus.set_pane(&session_id, pane_id);
        self.persist_local_focus(cx);
        cx.notify();
    }

    /// Pointer focus for a Pane. This is local presentation state only and
    /// can never change another Desktop's focus.
    pub(super) fn focus_pane_click(&mut self, pane_id: &str, cx: &mut Context<Self>) {
        if self.selected_session.is_none() {
            return;
        }
        if self
            .focused_entry()
            .is_some_and(|entry| entry.id == pane_id)
        {
            return;
        }
        self.focus_pane(pane_id, cx);
    }

    pub(super) fn focus_tab(&mut self, group_id: &str, tab_id: &str, cx: &mut Context<Self>) {
        let Some(session_id) = self.selected_session.clone() else {
            return;
        };
        self.focus_touched = true;
        self.local_focus.set_pane(&session_id, group_id);
        self.local_focus.set_tab(&session_id, group_id, tab_id);
        self.persist_local_focus(cx);
        cx.notify();
    }

    pub(super) fn cycle_focused_pane(&mut self, direction: isize, cx: &mut Context<Self>) {
        let Some((_, layout)) = self.focused_layout() else {
            return;
        };
        let entries = pane_entries(&layout.root);
        if entries.is_empty() {
            return;
        }
        let current = self
            .focused_entry()
            .map(|entry| entry.id)
            .unwrap_or_default();
        let index = entries
            .iter()
            .position(|entry| entry.id == current)
            .unwrap_or(0);
        let next = (index as isize + direction).rem_euclid(entries.len() as isize) as usize;
        self.focus_pane(&entries[next].id, cx);
    }

    pub(super) fn cycle_focused_tab(&mut self, direction: isize, cx: &mut Context<Self>) {
        let Some(entry) = self.focused_entry() else {
            return;
        };
        if entry.tabs.len() < 2 {
            return;
        }
        let current = self
            .focused_tab(&entry.id)
            .filter(|tab| entry.tabs.iter().any(|candidate| candidate == tab))
            .unwrap_or_else(|| entry.tabs[0].clone());
        let index = entry
            .tabs
            .iter()
            .position(|tab| *tab == current)
            .unwrap_or(0);
        let next = (index as isize + direction).rem_euclid(entry.tabs.len() as isize) as usize;
        self.focus_tab(&entry.id, &entry.tabs[next], cx);
    }

    pub(super) fn add_tab_to_focused_pane(&mut self, cx: &mut Context<Self>) {
        if self.mutation_busy {
            return;
        }
        let Some(entry) = self.focused_entry() else {
            return;
        };
        self.pending_tab_focus = Some(PendingFocus::LastInPane {
            pane_id: entry.id.clone(),
        });
        self.pane_action(
            PaneAction::TabAdd {
                pane_id: entry.id,
                title: "Activity".into(),
                content: PaneContent::Activity,
            },
            cx,
        );
    }

    pub(super) fn close_focused_tab(&mut self, cx: &mut Context<Self>) {
        if self.mutation_busy {
            return;
        }
        let Some(entry) = self.focused_entry() else {
            return;
        };
        if entry.tabs.len() < 2 {
            return; // The last visible surface is never closed.
        }
        let active = match self.focused_tab(&entry.id) {
            Some(tab) if entry.tabs.iter().any(|candidate| candidate == &tab) => tab,
            _ => entry.tabs[0].clone(),
        };
        let index = entry
            .tabs
            .iter()
            .position(|tab| tab == &active)
            .unwrap_or(0);
        let neighbour = entry
            .tabs
            .get(index + 1)
            .or_else(|| {
                index
                    .checked_sub(1)
                    .and_then(|previous| entry.tabs.get(previous))
            })
            .cloned();
        if let Some(tab_id) = neighbour {
            self.pending_tab_focus = Some(PendingFocus::Tab {
                group_id: entry.id.clone(),
                tab_id,
            });
        }
        self.pane_action(
            PaneAction::TabClose {
                group_id: entry.id,
                tab_id: active,
            },
            cx,
        );
    }

    pub(super) fn reorder_focused_tab(&mut self, direction: i32, cx: &mut Context<Self>) {
        if self.mutation_busy {
            return;
        }
        let Some(entry) = self.focused_entry() else {
            return;
        };
        if entry.tabs.len() < 2 {
            return;
        }
        let active = match self.focused_tab(&entry.id) {
            Some(tab) if entry.tabs.iter().any(|candidate| candidate == &tab) => tab,
            _ => entry.tabs[0].clone(),
        };
        let index = entry
            .tabs
            .iter()
            .position(|tab| tab == &active)
            .unwrap_or(0);
        if (direction < 0 && index == 0) || (direction > 0 && index + 1 == entry.tabs.len()) {
            return;
        }
        self.pending_tab_focus = Some(PendingFocus::Tab {
            group_id: entry.id.clone(),
            tab_id: active.clone(),
        });
        self.pane_action(
            PaneAction::TabReorder {
                group_id: entry.id,
                tab_id: active,
                direction,
            },
            cx,
        );
    }

    pub(super) fn move_focused_tab_to_adjacent_pane(
        &mut self,
        direction: isize,
        cx: &mut Context<Self>,
    ) {
        if self.mutation_busy {
            return;
        }
        let Some((_, layout)) = self.focused_layout() else {
            return;
        };
        let entries = pane_entries(&layout.root);
        let Some(entry) = self.focused_entry() else {
            return;
        };
        if entry.tabs.len() < 2 {
            // Moving the only Tab would leave an empty Pane.
            return;
        }
        let active = match self.focused_tab(&entry.id) {
            Some(tab) if entry.tabs.iter().any(|candidate| candidate == &tab) => tab,
            _ => entry.tabs[0].clone(),
        };
        let index = entries
            .iter()
            .position(|candidate| candidate.id == entry.id)
            .unwrap_or(0);
        let target = index as isize + direction;
        if target < 0 || target as usize >= entries.len() {
            return;
        }
        let destination = entries[target as usize].id.clone();
        self.pending_tab_focus = Some(PendingFocus::MovedTab {
            tab_id: active.clone(),
        });
        self.pane_action(
            PaneAction::TabMove {
                source_group_id: entry.id,
                destination_pane_id: destination,
                tab_id: active,
            },
            cx,
        );
    }

    pub(super) fn split_focused_pane(&mut self, axis: SplitAxis, cx: &mut Context<Self>) {
        if self.mutation_busy {
            return;
        }
        let Some((_, layout)) = self.focused_layout() else {
            return;
        };
        let Some(entry) = self.focused_entry() else {
            return;
        };
        let active = self
            .focused_tab(&entry.id)
            .filter(|tab| entry.tabs.iter().any(|candidate| candidate == tab))
            .or_else(|| entry.tabs.first().cloned());
        let content = content_for(&entry, &layout, active.as_deref());
        self.pane_action(
            PaneAction::Split {
                pane_id: entry.id,
                axis,
                content: adjacent_content(&content),
            },
            cx,
        );
    }

    pub(super) fn close_focused_pane(&mut self, cx: &mut Context<Self>) {
        if self.mutation_busy {
            return;
        }
        let Some((_, layout)) = self.focused_layout() else {
            return;
        };
        let entries = pane_entries(&layout.root);
        if entries.len() < 2 {
            return; // The last visible Pane is never closed.
        }
        let Some(entry) = self.focused_entry() else {
            return;
        };
        let index = entries
            .iter()
            .position(|candidate| candidate.id == entry.id)
            .unwrap_or(0);
        self.pending_pane_focus = entries
            .get(index + 1)
            .or_else(|| {
                index
                    .checked_sub(1)
                    .and_then(|previous| entries.get(previous))
            })
            .map(|target| target.id.clone());
        self.pane_action(PaneAction::Close { pane_id: entry.id }, cx);
    }

    pub(super) fn resize_enclosing_split(&mut self, direction: i32, cx: &mut Context<Self>) {
        if self.mutation_busy {
            return;
        }
        let Some((_, layout)) = self.focused_layout() else {
            return;
        };
        let Some(entry) = self.focused_entry() else {
            return;
        };
        let Some(PaneNode::Split { id, ratio, .. }) = enclosing_split(&layout.root, &entry.id)
        else {
            return;
        };
        let adjusted = adjusted_split_ratio(*ratio, direction);
        if (adjusted - ratio).abs() < f32::EPSILON {
            return;
        }
        self.pane_action(
            PaneAction::Resize {
                split_id: id.clone(),
                ratio: adjusted,
            },
            cx,
        );
    }

    /// Apply a deferred focus intent after a Host snapshot replaced the tree.
    /// Returns whether local focus changed and therefore needs persisting.
    pub(super) fn apply_pending_focus(&mut self) -> bool {
        let Some((session_id, layout)) = self.focused_layout() else {
            self.pending_tab_focus = None;
            self.pending_pane_focus = None;
            return false;
        };
        let entries = pane_entries(&layout.root);
        let mut changed = false;
        if let Some(pane_id) = self.pending_pane_focus.take() {
            if entries.iter().any(|entry| entry.id == pane_id) {
                self.local_focus.set_pane(&session_id, &pane_id);
                changed = true;
            }
        }
        if let Some(pending) = self.pending_tab_focus.take() {
            if let Some(resolved) = resolve_focus(&entries, &pending) {
                self.local_focus.set_pane(&session_id, &resolved.pane_id);
                if let Some(tab_id) = resolved.tab_id {
                    self.local_focus
                        .set_tab(&session_id, &resolved.pane_id, &tab_id);
                }
                changed = true;
            }
        }
        changed
    }
}
