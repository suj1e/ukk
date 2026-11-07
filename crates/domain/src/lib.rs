//! ukk's persistent *product* identities, separate from herdr-core PTYs.
//! None of these types owns or terminates a process. Closing a Pane only
//! changes a layout tree; AgentRun remains mapped to a herdr-core terminal.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fmt;
use uuid::Uuid;

pub fn new_id() -> String {
    Uuid::new_v4().to_string()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Space {
    pub id: String,
    pub title: String,
    pub project_root: String,
    pub created_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub space_id: String,
    pub title: String,
    pub archived: bool,
    pub created_ms: i64,
    pub updated_ms: i64,
}

/// Stable product binding to an existing herdr-core process. A terminal
/// address is unique *only within a particular herdr-core identity*.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRun {
    pub id: String,
    pub session_id: String,
    pub core_id: String,
    pub terminal_address: String,
    pub parent_run_id: Option<String>,
    pub adapter: String,
    pub created_ms: i64,
}

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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PaneNode {
    Leaf { id: String, content: PaneContent },
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
        Self::Leaf { id: new_id(), content }
    }

    pub fn id(&self) -> &str {
        match self {
            Self::Leaf { id, .. } | Self::Split { id, .. } => id,
        }
    }

    pub fn leaves(&self) -> Vec<(&str, &PaneContent)> {
        fn collect<'a>(node: &'a PaneNode, target: &mut Vec<(&'a str, &'a PaneContent)>) {
            match node {
                PaneNode::Leaf { id, content } => target.push((id, content)),
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
                    if *leaves > 16 {
                        return Err(LayoutError::TooManyPanes);
                    }
                    if let PaneContent::Agent { agent_run_id }
                    | PaneContent::Terminal { agent_run_id } = content
                    {
                        if agent_run_id.trim().is_empty() {
                            return Err(LayoutError::EmptyAgentId);
                        }
                    }
                }
                PaneNode::Split { ratio, first, second, .. } => {
                    if !ratio.is_finite() || !(0.15..=0.85).contains(ratio) {
                        return Err(LayoutError::InvalidRatio);
                    }
                    visit(first, depth + 1, ids, leaves)?;
                    visit(second, depth + 1, ids, leaves)?;
                }
            }
            Ok(())
        }
        visit(self, 0, &mut HashSet::new(), &mut 0)
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
                PaneNode::Leaf { id, .. } if id == target => Some(PaneNode::Split {
                    id: new_id(),
                    axis,
                    ratio,
                    first: Box::new(node.clone()),
                    second: Box::new(PaneNode::leaf(content.clone())),
                }),
                PaneNode::Leaf { .. } => None,
                PaneNode::Split { id, axis: old_axis, ratio: old_ratio, first, second } => {
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
        let updated = insert(self, target, axis, ratio, &content).ok_or(LayoutError::PaneNotFound)?;
        updated.validate()?;
        Ok(updated)
    }

    /// Close a view and collapse its parent split. The final leaf is retained.
    pub fn close_leaf(&self, target: &str) -> Result<Self, LayoutError> {
        fn remove(node: &PaneNode, target: &str) -> Option<PaneNode> {
            match node {
                PaneNode::Leaf { .. } => None,
                PaneNode::Split { id, axis, ratio, first, second } => {
                    if matches!(&**first, PaneNode::Leaf { id, .. } if id == target) {
                        return Some((**second).clone());
                    }
                    if matches!(&**second, PaneNode::Leaf { id, .. } if id == target) {
                        return Some((**first).clone());
                    }
                    if let Some(updated) = remove(first, target) {
                        return Some(PaneNode::Split {
                            id: id.clone(), axis: *axis, ratio: *ratio,
                            first: Box::new(updated), second: second.clone(),
                        });
                    }
                    remove(second, target).map(|updated| PaneNode::Split {
                        id: id.clone(), axis: *axis, ratio: *ratio,
                        first: first.clone(), second: Box::new(updated),
                    })
                }
            }
        }
        if matches!(self, PaneNode::Leaf { id, .. } if id == target) {
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DomainSnapshot {
    pub spaces: Vec<Space>,
    pub sessions: Vec<Session>,
    pub agent_runs: Vec<AgentRun>,
    pub layouts: Vec<SessionLayout>,
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
        let original = PaneNode::leaf(PaneContent::Agent { agent_run_id: "a1".into() });
        let original_id = original.id().to_owned();
        let split = original
            .split_leaf(&original_id, SplitAxis::Horizontal, 0.5, PaneContent::Terminal { agent_run_id: "a1".into() })
            .unwrap();
        assert_eq!(split.leaves().len(), 2);
        let second_id = split.leaves()[1].0.to_owned();
        let collapsed = split.close_leaf(&second_id).unwrap();
        assert_eq!(collapsed, original);
        assert_eq!(collapsed.leaves()[0].0, original_id);
        assert_eq!(original.leaves().len(), 1);
    }

    #[test]
    fn refuses_to_close_only_pane() {
        let only = PaneNode::leaf(PaneContent::Activity);
        assert_eq!(only.close_leaf(only.id()).unwrap_err(), LayoutError::LastPane);
    }

    #[test]
    fn rejects_invalid_sizes_and_targets() {
        let root = PaneNode::leaf(PaneContent::Files);
        assert_eq!(root.split_leaf("missing", SplitAxis::Vertical, 0.4, PaneContent::Diff).unwrap_err(), LayoutError::PaneNotFound);
        assert_eq!(root.split_leaf(root.id(), SplitAxis::Vertical, 1.0, PaneContent::Diff).unwrap_err(), LayoutError::InvalidRatio);
        assert_eq!(root.split_leaf(root.id(), SplitAxis::Vertical, f32::NAN, PaneContent::Diff).unwrap_err(), LayoutError::InvalidRatio);
    }

    #[test]
    fn rejects_duplicate_identifiers() {
        let root = PaneNode::Leaf { id: "same".into(), content: PaneContent::Files };
        let invalid = PaneNode::Split {
            id: "same".into(),
            axis: SplitAxis::Horizontal,
            ratio: 0.5,
            first: Box::new(root),
            second: Box::new(PaneNode::leaf(PaneContent::Diff)),
        };
        assert_eq!(invalid.validate().unwrap_err(), LayoutError::DuplicateOrEmptyId);
    }

    #[test]
    fn layout_json_roundtrip_preserves_split_order_and_ids() {
        let root = PaneNode::leaf(PaneContent::Activity);
        let two = root.split_leaf(root.id(), SplitAxis::Vertical, 0.4, PaneContent::Diff).unwrap();
        let encoded = serde_json::to_string(&two).unwrap();
        let decoded: PaneNode = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, two);
    }
}
