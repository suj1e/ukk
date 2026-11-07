//! Pure Pane/Tab focus targets shared by rendering and keyboard actions.
//!
//! These helpers never touch GPUI entities, herdr-core, SQLite or the Host.

use super::focus_store::PaneEntry;
use super::*;

const SPLIT_STEP: f32 = 0.05;

/// Respect the same bounds validated by `ukk-domain`.
const MIN_SPLIT_RATIO: f32 = 0.15;
const MAX_SPLIT_RATIO: f32 = 0.85;

/// Focus target resolved after a Host snapshot replaces the layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum PendingFocus {
    /// Neighbouring Tab after a close, or the active Tab after a reorder.
    Tab { group_id: String, tab_id: String },
    /// Newly appended Tab when the previous Pane shape is unknown.
    LastInPane { pane_id: String },
    /// Active Tab that moved to another Pane.
    MovedTab { tab_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ResolvedFocus {
    pub pane_id: String,
    pub tab_id: Option<String>,
}

/// Resolve a deferred focus intent against the *new* layout. Missing targets
/// return `None`, so stale intents never focus an unrelated Pane.
pub(super) fn resolve_focus(
    entries: &[PaneEntry],
    pending: &PendingFocus,
) -> Option<ResolvedFocus> {
    match pending {
        PendingFocus::Tab { group_id, tab_id } => {
            if let Some(entry) = entries
                .iter()
                .find(|entry| &entry.id == group_id && entry.tabs.iter().any(|tab| tab == tab_id))
            {
                return Some(ResolvedFocus {
                    pane_id: entry.id.clone(),
                    tab_id: Some(tab_id.clone()),
                });
            }
            // Closing one of two Tabs collapses the group into a Leaf whose
            // ID is the surviving Tab ID.
            entries
                .iter()
                .find(|entry| &entry.id == tab_id)
                .map(|entry| ResolvedFocus {
                    pane_id: entry.id.clone(),
                    tab_id: None,
                })
        }
        PendingFocus::LastInPane { pane_id } => {
            let entry = entries.iter().find(|entry| {
                &entry.id == pane_id || entry.tabs.iter().any(|tab| tab == pane_id)
            })?;
            Some(ResolvedFocus {
                pane_id: entry.id.clone(),
                tab_id: entry.tabs.last().cloned(),
            })
        }
        PendingFocus::MovedTab { tab_id } => entries
            .iter()
            .find(|entry| entry.tabs.iter().any(|tab| tab == tab_id))
            .map(|entry| ResolvedFocus {
                pane_id: entry.id.clone(),
                tab_id: Some(tab_id.clone()),
            }),
    }
}

/// Find the deepest `Split` node that contains `target`.
pub(super) fn enclosing_split<'a>(node: &'a PaneNode, target: &str) -> Option<&'a PaneNode> {
    fn contains(node: &PaneNode, target: &str) -> bool {
        match node {
            PaneNode::Split { first, second, .. } => {
                contains(first, target) || contains(second, target)
            }
            _ => node.id() == target,
        }
    }

    let PaneNode::Split { first, second, .. } = node else {
        return None;
    };
    let child = if contains(first, target) {
        first.as_ref()
    } else if contains(second, target) {
        second.as_ref()
    } else {
        return None;
    };
    enclosing_split(child, target).or(Some(node))
}

pub(super) fn adjusted_split_ratio(ratio: f32, direction: i32) -> f32 {
    let delta = if direction < 0 {
        -SPLIT_STEP
    } else {
        SPLIT_STEP
    };
    (ratio + delta).clamp(MIN_SPLIT_RATIO, MAX_SPLIT_RATIO)
}

/// Next content used by both the pointer split buttons and keyboard splits.
pub(super) fn adjacent_content(content: &PaneContent) -> PaneContent {
    match content {
        PaneContent::Agent { agent_run_id } => PaneContent::Terminal {
            agent_run_id: agent_run_id.clone(),
        },
        PaneContent::Diff => PaneContent::Files,
        PaneContent::Files => PaneContent::Activity,
        PaneContent::Terminal { .. } | PaneContent::Activity => PaneContent::Diff,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ukk_domain::PaneContent;

    fn entry(id: &str, tabs: &[&str]) -> PaneEntry {
        PaneEntry {
            id: id.into(),
            tabs: tabs.iter().map(|tab| (*tab).to_owned()).collect(),
        }
    }

    #[test]
    fn tab_focus_survives_reorder_and_collapsed_groups() {
        let entries = vec![entry("group-1", &["tab-b", "tab-a"])];
        assert_eq!(
            resolve_focus(
                &entries,
                &PendingFocus::Tab {
                    group_id: "group-1".into(),
                    tab_id: "tab-a".into()
                }
            ),
            Some(ResolvedFocus {
                pane_id: "group-1".into(),
                tab_id: Some("tab-a".into())
            })
        );

        let collapsed = vec![entry("tab-a", &[])];
        assert_eq!(
            resolve_focus(
                &collapsed,
                &PendingFocus::Tab {
                    group_id: "deleted-group".into(),
                    tab_id: "tab-a".into()
                }
            ),
            Some(ResolvedFocus {
                pane_id: "tab-a".into(),
                tab_id: None
            })
        );
    }

    #[test]
    fn stale_pending_focus_does_not_steal_another_pane() {
        let entries = vec![entry("group-1", &["tab-a"])];
        assert_eq!(
            resolve_focus(
                &entries,
                &PendingFocus::Tab {
                    group_id: "gone".into(),
                    tab_id: "gone-tab".into()
                }
            ),
            None
        );
        assert_eq!(
            resolve_focus(
                &entries,
                &PendingFocus::MovedTab {
                    tab_id: "gone-tab".into()
                }
            ),
            None
        );
    }

    #[test]
    fn moved_tab_focus_lands_in_the_destination_group() {
        let entries = vec![
            entry("source", &["keep"]),
            entry("dest", &["keep2", "moved"]),
        ];
        assert_eq!(
            resolve_focus(
                &entries,
                &PendingFocus::MovedTab {
                    tab_id: "moved".into()
                }
            ),
            Some(ResolvedFocus {
                pane_id: "dest".into(),
                tab_id: Some("moved".into())
            })
        );
    }

    #[test]
    fn added_tab_focus_uses_the_last_member_of_a_converted_leaf() {
        let entries = vec![entry("old-leaf-as-tab", &["old-leaf-as-tab", "new-tab"])];
        assert_eq!(
            resolve_focus(
                &entries,
                &PendingFocus::LastInPane {
                    pane_id: "old-leaf-as-tab".into()
                }
            ),
            Some(ResolvedFocus {
                pane_id: "old-leaf-as-tab".into(),
                tab_id: Some("new-tab".into())
            })
        );
    }

    #[test]
    fn enclosing_split_returns_the_deepest_ancestor() {
        let root = PaneNode::leaf(PaneContent::Activity);
        let first = root.id().to_owned();
        let split = root
            .split_leaf(&first, SplitAxis::Horizontal, 0.5, PaneContent::Files)
            .unwrap();
        let PaneNode::Split { second, .. } = &split else {
            panic!("expected split");
        };
        let target = second.id().to_owned();
        let nested = split
            .split_leaf(&target, SplitAxis::Vertical, 0.4, PaneContent::Diff)
            .unwrap();
        let inner = enclosing_split(&nested, &target).expect("enclosing split");
        assert_ne!(inner.id(), nested.id(), "must be the new innermost split");
        assert!(matches!(
            inner,
            PaneNode::Split { first, .. } if first.id() == target
        ));
        assert_eq!(enclosing_split(&nested, "missing"), None);
        assert_eq!(
            enclosing_split(&PaneNode::leaf(PaneContent::Files), "x"),
            None
        );
    }

    #[test]
    fn split_resize_steps_are_clamped_to_domain_bounds() {
        assert_eq!(adjusted_split_ratio(0.5, 1), 0.55);
        assert_eq!(adjusted_split_ratio(0.5, -1), 0.45);
        assert_eq!(adjusted_split_ratio(0.83, 1), 0.85);
        assert_eq!(adjusted_split_ratio(0.16, -1), 0.15);
        assert_eq!(adjusted_split_ratio(0.85, 1), 0.85);
    }

    #[test]
    fn adjacent_content_never_invents_agent_identity() {
        let agent = PaneContent::Agent {
            agent_run_id: "run-1".into(),
        };
        assert_eq!(
            adjacent_content(&agent),
            PaneContent::Terminal {
                agent_run_id: "run-1".into()
            }
        );
        assert_eq!(adjacent_content(&PaneContent::Files), PaneContent::Activity);
    }
}
