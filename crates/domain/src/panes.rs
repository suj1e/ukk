//! Validated immutable pane operations. UI layout never owns Agent processes.
use crate::new_id;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fmt;

/// Presentation-only node: never use its ID as an Agent or PTY identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PaneContent {
    Activity,
    Agent { agent_run_id: String },
    Terminal { agent_run_id: String },
    Diff,
    Files,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SplitAxis {
    Horizontal,
    Vertical,
}

/// A durable surface entry; selecting it is client-local UI state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneTab {
    pub id: String,
    pub title: String,
    pub content: PaneContent,
}

impl PaneTab {
    pub fn new(title: String, content: PaneContent) -> Self {
        Self {
            id: new_id(),
            title,
            content,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PaneNode {
    Leaf {
        id: String,
        content: PaneContent,
    },
    Tabs {
        id: String,
        /// Ordered tab entries. Active/focused tab intentionally not stored.
        tabs: Vec<PaneTab>,
    },
    Split {
        id: String,
        axis: SplitAxis,
        /// First pane's share of available width/height (0.15–0.85).
        ratio: f32,
        first: Box<PaneNode>,
        second: Box<PaneNode>,
    },
}

impl PaneNode {
    pub fn leaf(content: PaneContent) -> Self {
        Self::Leaf {
            id: new_id(),
            content,
        }
    }

    pub fn id(&self) -> &str {
        match self {
            Self::Leaf { id, .. } | Self::Tabs { id, .. } | Self::Split { id, .. } => id,
        }
    }

    pub fn leaves(&self) -> Vec<(&str, &PaneContent)> {
        fn collect<'a>(node: &'a PaneNode, target: &mut Vec<(&'a str, &'a PaneContent)>) {
            match node {
                PaneNode::Leaf { id, content } => target.push((id, content)),
                PaneNode::Tabs { tabs, .. } => {
                    for tab in tabs {
                        target.push((&tab.id, &tab.content));
                    }
                }
                PaneNode::Split { first, second, .. } => {
                    collect(first, target);
                    collect(second, target);
                }
            }
        }
        let mut result = Vec::new();
        collect(self, &mut result);
        result
    }

    pub fn validate(&self) -> Result<(), LayoutError> {
        fn validate_content(content: &PaneContent) -> Result<(), LayoutError> {
            if let PaneContent::Agent { agent_run_id } | PaneContent::Terminal { agent_run_id } =
                content
            {
                if agent_run_id.trim().is_empty() {
                    return Err(LayoutError::EmptyAgentId);
                }
            }
            Ok(())
        }
        fn visit(
            node: &PaneNode,
            depth: usize,
            ids: &mut HashSet<String>,
            leaves: &mut usize,
        ) -> Result<(), LayoutError> {
            if depth > 8 {
                return Err(LayoutError::TooDeep);
            }
            if node.id().trim().is_empty() || !ids.insert(node.id().to_owned()) {
                return Err(LayoutError::DuplicateOrEmptyId);
            }
            match node {
                PaneNode::Leaf { content, .. } => {
                    *leaves += 1;
                    validate_content(content)?;
                    if *leaves > 16 {
                        return Err(LayoutError::TooManyPanes);
                    }
                }
                PaneNode::Tabs { tabs, .. } => {
                    if tabs.is_empty() {
                        return Err(LayoutError::EmptyTabs);
                    }
                    for tab in tabs {
                        if tab.title.trim().is_empty() || tab.title.chars().count() > 64 {
                            return Err(LayoutError::InvalidTabTitle);
                        }
                        if tab.id.trim().is_empty() || !ids.insert(tab.id.clone()) {
                            return Err(LayoutError::DuplicateOrEmptyId);
                        }
                        validate_content(&tab.content)?;
                        *leaves += 1;
                        if *leaves > 16 {
                            return Err(LayoutError::TooManyPanes);
                        }
                    }
                }
                PaneNode::Split {
                    ratio,
                    first,
                    second,
                    ..
                } => {
                    if !ratio.is_finite() || !(0.15..=0.85).contains(ratio) {
                        return Err(LayoutError::InvalidRatio);
                    }
                    visit(first, depth + 1, ids, leaves)?;
                    visit(second, depth + 1, ids, leaves)?;
                }
            }
            Ok(())
        }
        let mut ids = HashSet::new();
        let mut leaf_count = 0usize;
        visit(self, 0, &mut ids, &mut leaf_count)
    }

    /// Return a new tree with a split, leaving the original unchanged on error.
    /// A split adds a second *view*, never spawns/terminates an Agent.
    pub fn split_leaf(
        &self,
        target: &str,
        axis: SplitAxis,
        ratio: f32,
        content: PaneContent,
    ) -> Result<Self, LayoutError> {
        if !ratio.is_finite() || !(0.15..=0.85).contains(&ratio) {
            return Err(LayoutError::InvalidRatio);
        }
        fn insert(
            node: &PaneNode,
            target: &str,
            axis: SplitAxis,
            ratio: f32,
            content: &PaneContent,
        ) -> Option<PaneNode> {
            match node {
                PaneNode::Leaf { id, .. } | PaneNode::Tabs { id, .. } if id == target => {
                    Some(PaneNode::Split {
                        id: new_id(),
                        axis,
                        ratio,
                        first: Box::new(node.clone()),
                        second: Box::new(PaneNode::leaf(content.clone())),
                    })
                }
                PaneNode::Leaf { .. } | PaneNode::Tabs { .. } => None,
                PaneNode::Split {
                    id,
                    axis: old_axis,
                    ratio: old_ratio,
                    first,
                    second,
                } => {
                    if let Some(updated) = insert(first, target, axis, ratio, content) {
                        return Some(PaneNode::Split {
                            id: id.clone(),
                            axis: *old_axis,
                            ratio: *old_ratio,
                            first: Box::new(updated),
                            second: second.clone(),
                        });
                    }
                    insert(second, target, axis, ratio, content).map(|updated| PaneNode::Split {
                        id: id.clone(),
                        axis: *old_axis,
                        ratio: *old_ratio,
                        first: first.clone(),
                        second: Box::new(updated),
                    })
                }
            }
        }
        let updated =
            insert(self, target, axis, ratio, &content).ok_or(LayoutError::PaneNotFound)?;
        updated.validate()?;
        Ok(updated)
    }

    /// Adjust the split identified by its own ID, without touching the child
    /// Pane IDs, the displayed AgentRun, or the underlying terminal.
    pub fn resize_split(&self, target: &str, ratio: f32) -> Result<Self, LayoutError> {
        if !ratio.is_finite() || !(0.15..=0.85).contains(&ratio) {
            return Err(LayoutError::InvalidRatio);
        }
        fn replace(node: &PaneNode, target: &str, ratio: f32) -> Option<PaneNode> {
            match node {
                PaneNode::Leaf { .. } | PaneNode::Tabs { .. } => None,
                PaneNode::Split {
                    id,
                    axis,
                    ratio: old_ratio,
                    first,
                    second,
                } => {
                    if id == target {
                        return Some(PaneNode::Split {
                            id: id.clone(),
                            axis: *axis,
                            ratio,
                            first: first.clone(),
                            second: second.clone(),
                        });
                    }
                    if let Some(updated) = replace(first, target, ratio) {
                        return Some(PaneNode::Split {
                            id: id.clone(),
                            axis: *axis,
                            ratio: *old_ratio,
                            first: Box::new(updated),
                            second: second.clone(),
                        });
                    }
                    replace(second, target, ratio).map(|updated| PaneNode::Split {
                        id: id.clone(),
                        axis: *axis,
                        ratio: *old_ratio,
                        first: first.clone(),
                        second: Box::new(updated),
                    })
                }
            }
        }
        let next = replace(self, target, ratio).ok_or(LayoutError::PaneNotFound)?;
        next.validate()?;
        Ok(next)
    }

    /// Close a view and collapse its parent split. The final leaf is retained.
    pub fn close_leaf(&self, target: &str) -> Result<Self, LayoutError> {
        fn remove(node: &PaneNode, target: &str) -> Option<PaneNode> {
            match node {
                PaneNode::Leaf { .. } | PaneNode::Tabs { .. } => None,
                PaneNode::Split {
                    id,
                    axis,
                    ratio,
                    first,
                    second,
                } => {
                    if matches!(&**first, PaneNode::Leaf { id, .. } | PaneNode::Tabs { id, .. } if id == target)
                    {
                        return Some((**second).clone());
                    }
                    if matches!(&**second, PaneNode::Leaf { id, .. } | PaneNode::Tabs { id, .. } if id == target)
                    {
                        return Some((**first).clone());
                    }
                    if let Some(updated) = remove(first, target) {
                        return Some(PaneNode::Split {
                            id: id.clone(),
                            axis: *axis,
                            ratio: *ratio,
                            first: Box::new(updated),
                            second: second.clone(),
                        });
                    }
                    remove(second, target).map(|updated| PaneNode::Split {
                        id: id.clone(),
                        axis: *axis,
                        ratio: *ratio,
                        first: first.clone(),
                        second: Box::new(updated),
                    })
                }
            }
        }
        if matches!(self, PaneNode::Leaf { id, .. } | PaneNode::Tabs { id, .. } if id == target) {
            return Err(LayoutError::LastPane);
        }
        let updated = remove(self, target).ok_or(LayoutError::PaneNotFound)?;
        updated.validate()?;
        Ok(updated)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionLayout {
    pub session_id: String,
    pub revision: i64,
    pub root: PaneNode,
}

impl SessionLayout {
    pub fn new(session_id: String) -> Self {
        Self {
            session_id,
            revision: 0,
            root: PaneNode::leaf(PaneContent::Activity),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutError {
    TooDeep,
    TooManyPanes,
    InvalidRatio,
    DuplicateOrEmptyId,
    EmptyAgentId,
    PaneNotFound,
    LastPane,
    EmptyTabs,
    InvalidTabTitle,
    NotTabGroup,
    TabNotFound,
    InvalidTabOrder,
}

impl fmt::Display for LayoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for LayoutError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splitting_and_closing_never_changes_agent_identity() {
        let original = PaneNode::leaf(PaneContent::Agent {
            agent_run_id: "a1".into(),
        });
        let original_id = original.id().to_owned();
        let split = original
            .split_leaf(
                &original_id,
                SplitAxis::Horizontal,
                0.5,
                PaneContent::Terminal {
                    agent_run_id: "a1".into(),
                },
            )
            .unwrap();
        assert_eq!(split.leaves().len(), 2);
        let second_id = split.leaves()[1].0.to_owned();
        let collapsed = split.close_leaf(&second_id).unwrap();
        assert_eq!(collapsed, original);
        assert_eq!(collapsed.leaves()[0].0, original_id);
        assert_eq!(original.leaves().len(), 1);
    }

    #[test]
    fn resize_preserves_child_identity_and_rejects_invalid_targets() {
        let first = PaneNode::leaf(PaneContent::Activity);
        let split = first
            .split_leaf(first.id(), SplitAxis::Horizontal, 0.5, PaneContent::Diff)
            .unwrap();
        let ids_before: Vec<String> = split
            .leaves()
            .iter()
            .map(|(id, _)| (*id).to_owned())
            .collect();
        let resized = split.resize_split(split.id(), 0.65).unwrap();
        let ids_after: Vec<String> = resized
            .leaves()
            .iter()
            .map(|(id, _)| (*id).to_owned())
            .collect();
        assert_eq!(ids_before, ids_after);
        assert!(
            matches!(&resized, PaneNode::Split { ratio, .. } if (*ratio - 0.65).abs() < f32::EPSILON)
        );
        assert_eq!(
            split.resize_split("missing", 0.6).unwrap_err(),
            LayoutError::PaneNotFound
        );
        assert_eq!(
            split.resize_split(split.id(), f32::NAN).unwrap_err(),
            LayoutError::InvalidRatio
        );
    }

    #[test]
    fn refuses_to_close_only_pane() {
        let only = PaneNode::leaf(PaneContent::Activity);
        assert_eq!(
            only.close_leaf(only.id()).unwrap_err(),
            LayoutError::LastPane
        );
    }

    #[test]
    fn rejects_invalid_sizes_and_targets() {
        let root = PaneNode::leaf(PaneContent::Files);
        assert_eq!(
            root.split_leaf("missing", SplitAxis::Vertical, 0.4, PaneContent::Diff)
                .unwrap_err(),
            LayoutError::PaneNotFound
        );
        assert_eq!(
            root.split_leaf(root.id(), SplitAxis::Vertical, 1.0, PaneContent::Diff)
                .unwrap_err(),
            LayoutError::InvalidRatio
        );
        assert_eq!(
            root.split_leaf(root.id(), SplitAxis::Vertical, f32::NAN, PaneContent::Diff)
                .unwrap_err(),
            LayoutError::InvalidRatio
        );
    }

    #[test]
    fn rejects_duplicate_identifiers() {
        let root = PaneNode::Leaf {
            id: "same".into(),
            content: PaneContent::Files,
        };
        let invalid = PaneNode::Split {
            id: "same".into(),
            axis: SplitAxis::Horizontal,
            ratio: 0.5,
            first: Box::new(root),
            second: Box::new(PaneNode::leaf(PaneContent::Diff)),
        };
        assert_eq!(
            invalid.validate().unwrap_err(),
            LayoutError::DuplicateOrEmptyId
        );
    }

    #[test]
    fn layout_json_roundtrip_preserves_split_order_and_ids() {
        let root = PaneNode::leaf(PaneContent::Activity);
        let two = root
            .split_leaf(root.id(), SplitAxis::Vertical, 0.4, PaneContent::Diff)
            .unwrap();
        let encoded = serde_json::to_string(&two).unwrap();
        let decoded: PaneNode = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, two);
    }
}
