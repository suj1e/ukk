//! Single native root render, composed from independent panes and navigation.
//!
//! The root owns application-level keyboard focus and all shortcut listeners.
//! Layout adapts to the real window size so narrow windows stay usable.

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WindowLayout {
    /// Sidebar, content and runtime inspector.
    Full,
    /// Sidebar and content; the inspector would squeeze the workspace.
    Compact,
    /// Narrow sidebar and content only.
    Narrow,
}

pub(super) fn layout_for_width(width: f32) -> WindowLayout {
    if width >= 1080.0 {
        WindowLayout::Full
    } else if width >= 820.0 {
        WindowLayout::Compact
    } else {
        WindowLayout::Narrow
    }
}

pub(super) fn sidebar_width(layout: WindowLayout) -> f32 {
    match layout {
        WindowLayout::Full => 260.0,
        WindowLayout::Compact => 220.0,
        WindowLayout::Narrow => 176.0,
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let layout = layout_for_width(window.viewport_size().width.into());
        div()
            .id("workspace-root")
            .role(Role::Application)
            .aria_label("ukk Agent Workspace")
            .track_focus(&self.focused_root)
            .key_context(keymap::KEY_CONTEXT)
            .on_action(cx.listener(Self::on_focus_next_pane))
            .on_action(cx.listener(Self::on_focus_prev_pane))
            .on_action(cx.listener(Self::on_focus_next_tab))
            .on_action(cx.listener(Self::on_focus_prev_tab))
            .on_action(cx.listener(Self::on_focus_next_stop))
            .on_action(cx.listener(Self::on_focus_prev_stop))
            .on_action(cx.listener(Self::on_add_tab))
            .on_action(cx.listener(Self::on_close_tab))
            .on_action(cx.listener(Self::on_reorder_tab_left))
            .on_action(cx.listener(Self::on_reorder_tab_right))
            .on_action(cx.listener(Self::on_move_tab_previous_pane))
            .on_action(cx.listener(Self::on_move_tab_next_pane))
            .on_action(cx.listener(Self::on_split_pane_right))
            .on_action(cx.listener(Self::on_split_pane_down))
            .on_action(cx.listener(Self::on_close_pane))
            .on_action(cx.listener(Self::on_shrink_split))
            .on_action(cx.listener(Self::on_grow_split))
            .on_action(cx.listener(Self::on_reload_layout))
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(BG))
            .text_color(rgb(INK))
            .child(self.topbar(layout != WindowLayout::Narrow, cx))
            .when_some(self.status_banner(cx), |root, banner| root.child(banner))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .child(self.sidebar(sidebar_width(layout), cx))
                    .child(self.detail(cx))
                    .when(layout == WindowLayout::Full, |row| {
                        row.child(self.inspector())
                    }),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_layout_keeps_a_readable_content_column() {
        assert_eq!(layout_for_width(1280.0), WindowLayout::Full);
        assert_eq!(layout_for_width(1080.0), WindowLayout::Full);
        assert_eq!(layout_for_width(900.0), WindowLayout::Compact);
        assert_eq!(layout_for_width(820.0), WindowLayout::Compact);
        assert_eq!(layout_for_width(640.0), WindowLayout::Narrow);
        assert_eq!(layout_for_width(320.0), WindowLayout::Narrow);
    }

    #[test]
    fn every_layout_leaves_room_for_session_content() {
        for width in [1440.0_f32, 1024.0, 800.0] {
            let sidebar = sidebar_width(layout_for_width(width));
            assert!(
                width - sidebar >= 320.0,
                "content column collapsed at {width}px with a {sidebar}px sidebar"
            );
        }
    }
}
