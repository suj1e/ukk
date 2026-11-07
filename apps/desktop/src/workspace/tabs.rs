//! Native tab strip. Active tab selection belongs to each Desktop window;
//! membership/order are durable Session layout metadata owned by the Host.
use super::*;

pub(super) fn find_tab_group<'a>(node: &'a PaneNode, group_id: &str) -> Option<&'a [PaneTab]> {
    match node {
        PaneNode::Tabs { id, tabs } if id == group_id => Some(tabs),
        PaneNode::Split { first, second, .. } => {
            find_tab_group(first, group_id).or_else(|| find_tab_group(second, group_id))
        }
        _ => None,
    }
}

impl Workspace {
    /// The UI may receive drag payloads after a Session switch; never let
    /// stale drag events edit another Session. Host still enforces local
    /// authorization and layout revision on every actual mutation.
    pub(super) fn accept_dragged_tab(
        &mut self,
        payload: &DraggedPaneTab,
        destination: &str,
        cx: &mut Context<Self>,
    ) {
        if self.mutation_busy
            || self.selected_session.as_deref() != Some(payload.session_id.as_str())
            || payload.source_group_id == destination
        {
            return;
        }
        self.pane_action(
            PaneAction::TabMove {
                source_group_id: payload.source_group_id.clone(),
                destination_pane_id: destination.to_owned(),
                tab_id: payload.tab_id.clone(),
            },
            cx,
        );
    }

    fn selected_tab_id<'a>(&self, group_id: &str, tabs: &'a [PaneTab]) -> &'a str {
        let key = (
            self.selected_session.as_deref().unwrap_or("").to_owned(),
            group_id.to_owned(),
        );
        let focused = self.tab_focus.get(&key).map(String::as_str);
        tabs.iter()
            .find(|tab| Some(tab.id.as_str()) == focused)
            .or_else(|| tabs.first())
            .map(|tab| tab.id.as_str())
            .unwrap_or("")
    }

    pub(super) fn tab_group(
        &self,
        group_id: &str,
        tabs: &[PaneTab],
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let active_id = self.selected_tab_id(group_id, tabs);
        let Some(active) = tabs.iter().find(|tab| tab.id == active_id) else {
            return div().child("Invalid empty tab group").into_any_element();
        };
        let active_index = tabs.iter().position(|tab| tab.id == active_id).unwrap_or(0);
        let active_id = active.id.clone();
        let current_session = self.selected_session.clone().unwrap_or_default();
        let group_id_owned = group_id.to_owned();

        let tab_strip = div()
            .id(format!("tab-strip-{group_id}"))
            .flex()
            .items_center()
            .gap_1()
            .min_w_0()
            .overflow_x_scroll()
            .children(tabs.iter().map(|tab| {
                let selected = tab.id == active_id;
                let tab_id = tab.id.clone();
                let group = group_id_owned.clone();
                let session = current_session.clone();
                div()
                    .id(format!("tab-{group}-{tab_id}"))
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .bg(rgb(if selected { WHITE } else { SURFACE }))
                    .text_xs()
                    .text_color(rgb(if selected { ACCENT } else { MUTED }))
                    .cursor_pointer()
                    .hover(|style| style.bg(rgb(WHITE)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        // Do not let an old click from a different Session
                        // steal focus after switching Workspace context.
                        if this.selected_session.as_deref() == Some(&session) {
                            this.tab_focus
                                .insert((session.clone(), group.clone()), tab_id.clone());
                            cx.notify();
                        }
                    }))
                    .on_drag(
                        DraggedPaneTab {
                            session_id: current_session.clone(),
                            source_group_id: group_id_owned.clone(),
                            tab_id: tab.id.clone(),
                        },
                        |_, _, _, cx| cx.new(|_| gpui::Empty),
                    )
                    .child(tab.title.clone())
            }));

        let controls = div()
            .flex()
            .items_center()
            .gap_1()
            .when(active_index > 0, |row| {
                row.child(self.pane_button(
                    cx,
                    format!("{group_id}-tab-left"),
                    "←",
                    PaneAction::TabReorder {
                        group_id: group_id_owned.clone(),
                        tab_id: active_id.clone(),
                        direction: -1,
                    },
                ))
            })
            .when(active_index + 1 < tabs.len(), |row| {
                row.child(self.pane_button(
                    cx,
                    format!("{group_id}-tab-right"),
                    "→",
                    PaneAction::TabReorder {
                        group_id: group_id_owned.clone(),
                        tab_id: active_id.clone(),
                        direction: 1,
                    },
                ))
            })
            .child(self.pane_button(
                cx,
                format!("{group_id}-tab-close"),
                "Close tab",
                PaneAction::TabClose {
                    group_id: group_id_owned.clone(),
                    tab_id: active_id.clone(),
                },
            ))
            .child(self.pane_button(
                cx,
                format!("{group_id}-tab-add-files"),
                "+ Files",
                PaneAction::TabAdd {
                    pane_id: group_id_owned.clone(),
                    title: "Files".into(),
                    content: PaneContent::Files,
                },
            ))
            .child(self.pane_button(
                cx,
                format!("{group_id}-tab-add-diff"),
                "+ Diff",
                PaneAction::TabAdd {
                    pane_id: group_id_owned.clone(),
                    title: "Diff".into(),
                    content: PaneContent::Diff,
                },
            ));

        // Only the active surface is rendered, but all tab membership remains
        // in the validated SQLite tree. This transient leaf is never stored.
        let surface = PaneNode::Leaf {
            id: group_id_owned.clone(),
            content: active.content.clone(),
        };
        let destination = group_id_owned;
        div()
            .id(format!("drop-target-tabs-{group_id}"))
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .bg(rgb(SURFACE))
                    .child(tab_strip)
                    .child(controls),
            )
            .child(self.session_pane(&surface, cx))
            .on_drop::<DraggedPaneTab>(cx.listener(move |this, payload: &DraggedPaneTab, _, cx| {
                this.accept_dragged_tab(payload, &destination, cx);
            }))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_tab_lookup_ignores_unrelated_split_branches() {
        let root = PaneNode::leaf(PaneContent::Activity);
        let tabs = root
            .add_tab(root.id(), "Files".into(), PaneContent::Files)
            .unwrap();
        let group_id = tabs.id().to_owned();
        let split = tabs
            .split_leaf(tabs.id(), SplitAxis::Vertical, 0.5, PaneContent::Diff)
            .unwrap();
        assert_eq!(find_tab_group(&split, &group_id).unwrap().len(), 2);
        assert!(find_tab_group(&split, "missing").is_none());
    }
}
