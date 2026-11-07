//! Immutable tab membership edits, independent of window focus and agents.
use crate::{new_id, LayoutError, PaneContent, PaneNode, PaneTab};

fn default_title(content: &PaneContent) -> &'static str {
    match content {
        PaneContent::Activity => "Activity",
        PaneContent::Files => "Files",
        PaneContent::Diff => "Diff",
        PaneContent::Agent { .. } => "Agent",
        PaneContent::Terminal { .. } => "Terminal",
    }
}

/// Only the group containing the target is changed. Other Pane IDs, Session
/// bindings and split ratios are retained. Never mutate herdr-core processes.
fn rewrite_target(
    node: &PaneNode,
    target: &str,
    edit: &impl Fn(&PaneNode) -> Result<PaneNode, LayoutError>,
) -> Result<Option<PaneNode>, LayoutError> {
    if node.id() == target {
        return Ok(Some(edit(node)?));
    }
    if let PaneNode::Split {
        id,
        axis,
        ratio,
        first,
        second,
    } = node
    {
        if let Some(updated) = rewrite_target(first, target, edit)? {
            return Ok(Some(PaneNode::Split {
                id: id.clone(),
                axis: *axis,
                ratio: *ratio,
                first: Box::new(updated),
                second: second.clone(),
            }));
        }
        if let Some(updated) = rewrite_target(second, target, edit)? {
            return Ok(Some(PaneNode::Split {
                id: id.clone(),
                axis: *axis,
                ratio: *ratio,
                first: first.clone(),
                second: Box::new(updated),
            }));
        }
    }
    Ok(None)
}

impl PaneNode {
    /// Converts an existing Leaf to a tab group without changing its original
    /// view ID, or appends to an existing group in durable tab order.
    pub fn add_tab(
        &self,
        pane_id: &str,
        title: String,
        content: PaneContent,
    ) -> Result<Self, LayoutError> {
        let updated = rewrite_target(self, pane_id, &|node| {
            let new_tab = PaneTab::new(title.clone(), content.clone());
            match node {
                PaneNode::Leaf {
                    id,
                    content: original,
                } => Ok(PaneNode::Tabs {
                    id: new_id(),
                    tabs: vec![
                        PaneTab {
                            id: id.clone(),
                            title: default_title(original).to_owned(),
                            content: original.clone(),
                        },
                        new_tab,
                    ],
                }),
                PaneNode::Tabs { id, tabs } => {
                    let mut tabs = tabs.clone();
                    tabs.push(new_tab);
                    Ok(PaneNode::Tabs {
                        id: id.clone(),
                        tabs,
                    })
                }
                PaneNode::Split { .. } => Err(LayoutError::NotTabGroup),
            }
        })?
        .ok_or(LayoutError::PaneNotFound)?;
        updated.validate()?;
        Ok(updated)
    }

    /// Closing a tab changes only the layout. The last visible tab cannot be
    /// closed; groups with one remaining tab collapse to a legacy Leaf.
    pub fn close_tab(&self, group_id: &str, tab_id: &str) -> Result<Self, LayoutError> {
        let updated = rewrite_target(self, group_id, &|node| {
            let PaneNode::Tabs { id, tabs } = node else {
                return Err(LayoutError::NotTabGroup);
            };
            let position = tabs
                .iter()
                .position(|t| t.id == tab_id)
                .ok_or(LayoutError::TabNotFound)?;
            if tabs.len() <= 1 {
                return Err(LayoutError::LastPane);
            }
            let mut remaining = tabs.clone();
            remaining.remove(position);
            if remaining.len() == 1 {
                let last = remaining.remove(0);
                Ok(PaneNode::Leaf {
                    id: last.id,
                    content: last.content,
                })
            } else {
                Ok(PaneNode::Tabs {
                    id: id.clone(),
                    tabs: remaining,
                })
            }
        })?
        .ok_or(LayoutError::PaneNotFound)?;
        updated.validate()?;
        Ok(updated)
    }

    /// Move one view from a tab group into another Pane in a single
    /// immutable layout edit. The moved tab's ID and AgentRun reference are
    /// retained, so this operation never starts/stops an Agent or PTY.
    ///
    /// The source group must retain a visible Pane: moving one tab out of a
    /// two-tab group collapses the source to its remaining legacy Leaf. A
    /// single-tab source is rejected rather than leaving an empty Pane.
    pub fn move_tab(
        &self,
        source_group_id: &str,
        tab_id: &str,
        destination_pane_id: &str,
    ) -> Result<Self, LayoutError> {
        if source_group_id == destination_pane_id {
            return Err(LayoutError::SameTabGroup);
        }
        self.validate()?;
        let mut moved: Option<PaneTab> = None;
        let without_source = rewrite_target(self, source_group_id, &|node| {
            let PaneNode::Tabs { id, tabs } = node else {
                return Err(LayoutError::NotTabGroup);
            };
            if tabs.len() <= 1 {
                return Err(LayoutError::LastPane);
            }
            let index = tabs
                .iter()
                .position(|tab| tab.id == tab_id)
                .ok_or(LayoutError::TabNotFound)?;
            let mut remaining = tabs.clone();
            // Write the moved entry into a RefCell-free local lookup after
            // reconstructing the immutable source. The source is never
            // persisted independently of the destination.
            remaining.remove(index);
            if remaining.len() == 1 {
                let only = remaining.remove(0);
                Ok(PaneNode::Leaf {
                    id: only.id,
                    content: only.content,
                })
            } else {
                Ok(PaneNode::Tabs {
                    id: id.clone(),
                    tabs: remaining,
                })
            }
        })?
        .ok_or(LayoutError::PaneNotFound)?;

        // Extract from original source (rather than storing mutable state
        // in the edit closure). This makes operation atomic and retry-safe.
        fn find_group<'a>(node: &'a PaneNode, id: &str) -> Option<&'a [PaneTab]> {
            match node {
                PaneNode::Tabs { id: group_id, tabs } if group_id == id => Some(tabs),
                PaneNode::Split { first, second, .. } => {
                    find_group(first, id).or_else(|| find_group(second, id))
                }
                _ => None,
            }
        }
        if let Some(group) = find_group(self, source_group_id) {
            moved = group.iter().find(|tab| tab.id == tab_id).cloned();
        }
        let moved = moved.ok_or(LayoutError::TabNotFound)?;

        let updated = rewrite_target(&without_source, destination_pane_id, &|node| {
            match node {
                PaneNode::Leaf { id, content } => Ok(PaneNode::Tabs {
                    id: new_id(),
                    tabs: vec![
                        PaneTab {
                            id: id.clone(),
                            title: default_title(content).to_owned(),
                            content: content.clone(),
                        },
                        moved.clone(),
                    ],
                }),
                PaneNode::Tabs { id, tabs } => {
                    let mut tabs = tabs.clone();
                    tabs.push(moved.clone());
                    Ok(PaneNode::Tabs {
                        id: id.clone(),
                        tabs,
                    })
                }
                PaneNode::Split { .. } => Err(LayoutError::NotTabGroup),
            }
        })?
        .ok_or(LayoutError::PaneNotFound)?;
        updated.validate()?;
        Ok(updated)
    }

    /// A single-step ordering change, persisted with the Session layout CAS.
    /// Active tab is deliberately not part of this transaction.
    pub fn reorder_tab(
        &self,
        group_id: &str,
        tab_id: &str,
        direction: i32,
    ) -> Result<Self, LayoutError> {
        if !matches!(direction, -1 | 1) {
            return Err(LayoutError::InvalidTabOrder);
        }
        let updated = rewrite_target(self, group_id, &|node| {
            let PaneNode::Tabs { id, tabs } = node else {
                return Err(LayoutError::NotTabGroup);
            };
            let index = tabs
                .iter()
                .position(|t| t.id == tab_id)
                .ok_or(LayoutError::TabNotFound)?;
            let new_index = index as i32 + direction;
            if new_index < 0 || new_index as usize >= tabs.len() {
                return Err(LayoutError::InvalidTabOrder);
            }
            let mut reordered = tabs.clone();
            reordered.swap(index, new_index as usize);
            Ok(PaneNode::Tabs {
                id: id.clone(),
                tabs: reordered,
            })
        })?
        .ok_or(LayoutError::PaneNotFound)?;
        updated.validate()?;
        Ok(updated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_leaf_to_tabs_preserves_existing_view_identity_and_json() {
        let root = PaneNode::Leaf {
            id: "original".into(),
            content: PaneContent::Files,
        };
        let tabs = root
            .add_tab("original", "Diff".into(), PaneContent::Diff)
            .unwrap();
        let PaneNode::Tabs { id, tabs: entries } = &tabs else {
            panic!("expected Tabs");
        };
        assert_ne!(id, "original");
        assert_eq!(entries[0].id, "original");
        assert_eq!(entries[0].title, "Files");
        assert_eq!(entries[1].title, "Diff");
        let encoded = serde_json::to_string(&tabs).unwrap();
        assert_eq!(serde_json::from_str::<PaneNode>(&encoded).unwrap(), tabs);
        assert_eq!(
            serde_json::from_str::<PaneNode>(
                r#"{"kind":"leaf","id":"original","content":{"type":"files"}}"#
            )
            .unwrap(),
            root
        );
    }

    #[test]
    fn reorder_close_and_last_tab_semantics() {
        let root = PaneNode::leaf(PaneContent::Activity);
        let group = root
            .add_tab(root.id(), "Files".into(), PaneContent::Files)
            .unwrap();
        let PaneNode::Tabs { id, tabs } = &group else {
            panic!()
        };
        let group_id = id.clone();
        let tab_id = tabs[1].id.clone();
        let reordered = group.reorder_tab(&group_id, &tab_id, -1).unwrap();
        assert_eq!(reordered.leaves()[0].0, tab_id);
        assert_eq!(
            reordered.reorder_tab(&group_id, &tab_id, -1).unwrap_err(),
            LayoutError::InvalidTabOrder
        );
        let collapsed = reordered.close_tab(&group_id, &tab_id).unwrap();
        assert_eq!(collapsed, root);
        assert_eq!(
            collapsed.close_leaf(root.id()).unwrap_err(),
            LayoutError::LastPane
        );
    }

    #[test]
    fn split_tabs_and_preserve_agent_identity() {
        let agent = PaneNode::leaf(PaneContent::Agent {
            agent_run_id: "run-1".into(),
        });
        let tabs = agent
            .add_tab(agent.id(), "Diff".into(), PaneContent::Diff)
            .unwrap();
        let split = tabs
            .split_leaf(
                tabs.id(),
                crate::SplitAxis::Vertical,
                0.5,
                PaneContent::Files,
            )
            .unwrap();
        assert_eq!(split.leaves().len(), 3);
        assert!(
            matches!(split.leaves()[0].1, PaneContent::Agent { agent_run_id } if agent_run_id == "run-1")
        );
        let second = split.leaves()[2].0.to_owned();
        assert_eq!(split.close_leaf(&second).unwrap(), tabs);
    }

    #[test]
    fn rejects_invalid_tab_groups_and_duplicate_ids() {
        let bad = PaneNode::Tabs {
            id: "group".into(),
            tabs: vec![],
        };
        assert_eq!(bad.validate().unwrap_err(), LayoutError::EmptyTabs);
        let one = PaneTab::new("First".into(), PaneContent::Activity);
        let dup = PaneNode::Tabs {
            id: one.id.clone(),
            tabs: vec![one],
        };
        assert_eq!(dup.validate().unwrap_err(), LayoutError::DuplicateOrEmptyId);
        let root = PaneNode::leaf(PaneContent::Files);
        assert_eq!(
            root.add_tab(root.id(), " ".into(), PaneContent::Diff)
                .unwrap_err(),
            LayoutError::InvalidTabTitle
        );
    }

    #[test]
    fn global_sixteen_surface_limit_includes_hidden_tabs() {
        let mut root = PaneNode::leaf(PaneContent::Activity);
        for _ in 0..15 {
            let id = root.id().to_owned();
            root = root
                .add_tab(&id, "Activity".into(), PaneContent::Activity)
                .unwrap();
        }
        assert_eq!(root.leaves().len(), 16);
        let group = root.id().to_owned();
        assert_eq!(
            root.add_tab(&group, "Too many".into(), PaneContent::Diff)
                .unwrap_err(),
            LayoutError::TooManyPanes
        );
    }
}
