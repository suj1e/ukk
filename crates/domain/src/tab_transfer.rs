//! Atomic cross-Pane transfers. This module preserves tab IDs and all
//! AgentRun associations without mutating process state or shared tab focus.
use crate::tabs::{default_title, rewrite_target};
use crate::{LayoutError, PaneNode, PaneTab, new_id};

impl PaneNode {
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
        fn find_group<'a>(node: &'a PaneNode, group: &str) -> Option<&'a [PaneTab]> {
            match node {
                PaneNode::Tabs { id, tabs } if id == group => Some(tabs),
                PaneNode::Split { first, second, .. } => {
                    find_group(first, group).or_else(|| find_group(second, group))
                }
                _ => None,
            }
        }
        let moved = find_group(self, source_group_id)
            .ok_or(LayoutError::PaneNotFound)?
            .iter()
            .find(|tab| tab.id == tab_id)
            .cloned()
            .ok_or(LayoutError::TabNotFound)?;
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PaneContent;

    #[test]
    fn moving_tabs_between_split_panes_preserves_ids_and_agent_identity() {
        let agent = PaneNode::leaf(PaneContent::Agent {
            agent_run_id: "agent-stays-running".into(),
        });
        let source = agent
            .add_tab(agent.id(), "Files".into(), PaneContent::Files)
            .unwrap();
        let source_id = source.id().to_owned();
        let moved_id = source.leaves()[1].0.to_owned();
        let tree = source
            .split_leaf(source.id(), crate::SplitAxis::Horizontal, 0.6, PaneContent::Diff)
            .unwrap();
        let dest_id = tree.leaves()[2].0.to_owned();
        let new_tree = tree.move_tab(&source_id, &moved_id, &dest_id).unwrap();
        new_tree.validate().unwrap();
        assert_eq!(new_tree.leaves().len(), 3);
        assert_eq!(new_tree.leaves()[0].0, agent.leaves()[0].0);
        assert_eq!(new_tree.leaves()[2].0, moved_id);
        assert!(matches!(
            new_tree.leaves()[0].1,
            PaneContent::Agent { agent_run_id } if agent_run_id == "agent-stays-running"
        ));
        assert!(matches!(&new_tree, PaneNode::Split { ratio, .. } if *ratio == 0.6));
        assert_eq!(tree.leaves().len(), 3, "source tree must remain immutable");
    }

    #[test]
    fn cross_pane_move_appends_to_existing_tabs_and_rejects_bad_targets() {
        let first = PaneNode::leaf(PaneContent::Activity);
        let source = first.add_tab(first.id(), "Files".into(), PaneContent::Files).unwrap();
        let source_id = source.id().to_owned();
        let moving_id = source.leaves()[1].0.to_owned();
        let destination = PaneNode::leaf(PaneContent::Diff);
        let destination = destination
            .add_tab(destination.id(), "Activity".into(), PaneContent::Activity)
            .unwrap();
        let target_id = destination.id().to_owned();
        let tree = PaneNode::Split {
            id: new_id(),
            axis: crate::SplitAxis::Vertical,
            ratio: 0.5,
            first: Box::new(source),
            second: Box::new(destination),
        };
        let transferred = tree.move_tab(&source_id, &moving_id, &target_id).unwrap();
        assert_eq!(transferred.leaves().len(), 4);
        assert_eq!(transferred.leaves()[3].0, moving_id);
        assert_eq!(tree.move_tab(&source_id, &moving_id, &source_id).unwrap_err(),
            LayoutError::SameTabGroup);
        assert_eq!(tree.move_tab(&source_id, "not-a-tab", &target_id).unwrap_err(),
            LayoutError::TabNotFound);
        assert_eq!(tree.move_tab(&source_id, &moving_id, "missing").unwrap_err(),
            LayoutError::PaneNotFound);
        assert_eq!(tree, serde_json::from_value::<PaneNode>(
            serde_json::to_value(&tree).unwrap()
        ).unwrap());
    }

    #[test]
    fn moving_from_single_tab_group_is_rejected_without_emptying_pane() {
        let root = PaneNode::Tabs {
            id: new_id(),
            tabs: vec![PaneTab::new("Only".into(), PaneContent::Activity)],
        };
        let split = root
            .split_leaf(root.id(), crate::SplitAxis::Vertical, 0.5, PaneContent::Files)
            .unwrap();
        let dest = split.leaves()[1].0.to_owned();
        let tab_id = root.leaves()[0].0.to_owned();
        assert_eq!(split.move_tab(root.id(), &tab_id, &dest).unwrap_err(), LayoutError::LastPane);
        assert_eq!(split.leaves().len(), 2);
    }
}
