//! Draggable native horizontal/vertical split containers.
use super::*;

impl Workspace {
    /// A narrow native drag handle between split children; the drag payload
    /// contains only a Pane split ID, never a terminal or Agent identifier.
    fn split_resize_handle(
        &self,
        split_id: &str,
        axis: SplitAxis,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let id_for_up = split_id.to_owned();
        let id_for_out = split_id.to_owned();
        let handle = div()
            .id(format!("split-drag-handle-{split_id}"))
            .flex_shrink_0()
            .bg(rgb(LINE))
            .hover(|style| style.bg(rgb(ACCENT)))
            .block_mouse_except_scroll()
            .on_drag(
                DraggedSplitHandle {
                    split_id: split_id.to_owned(),
                },
                |_, _, _, cx| cx.new(|_| gpui::Empty),
            )
            .on_mouse_up(
                gpui::MouseButton::Left,
                cx.listener(move |this, _, _, cx| this.finish_split_drag(&id_for_up, cx)),
            )
            .on_mouse_up_out(
                gpui::MouseButton::Left,
                cx.listener(move |this, _, _, cx| this.finish_split_drag(&id_for_out, cx)),
            );
        match axis {
            SplitAxis::Horizontal => handle
                .w(px(8.))
                .h_full()
                .cursor_col_resize()
                .into_any_element(),
            SplitAxis::Vertical => handle
                .h(px(8.))
                .w_full()
                .cursor_row_resize()
                .into_any_element(),
        }
    }

    /// Render a native split without giving Pane or UI events ownership of
    /// a herdr-core process. Ratio preview lives in resize.rs.
    pub(super) fn split_container(
        &self,
        id: &str,
        axis: SplitAxis,
        ratio: f32,
        first: &PaneNode,
        second: &PaneNode,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let shown_ratio = self.displayed_split_ratio(id, ratio);
        let smaller = (shown_ratio - 0.1).max(0.15);
        let larger = (shown_ratio + 0.1).min(0.85);
        let controls = div()
            .flex()
            .justify_end()
            .items_center()
            .gap_2()
            .mb_2()
            .child(div().text_xs().text_color(rgb(MUTED)).child(format!(
                "{}% / {}%",
                (shown_ratio * 100.0).round() as i32,
                ((1.0 - shown_ratio) * 100.0).round() as i32
            )))
            .child(self.pane_button(
                cx,
                format!("{id}-smaller"),
                "−",
                PaneAction::Resize {
                    split_id: id.to_owned(),
                    ratio: smaller,
                },
            ))
            .child(self.pane_button(
                cx,
                format!("{id}-larger"),
                "+",
                PaneAction::Resize {
                    split_id: id.to_owned(),
                    ratio: larger,
                },
            ));
        let first_view = div()
            .flex()
            .flex_shrink_1()
            .min_w_0()
            .min_h_0()
            .flex_basis(gpui::DefiniteLength::Fraction(shown_ratio))
            .child(self.session_pane(first, cx));
        let second_view = div()
            .flex()
            .flex_shrink_1()
            .min_w_0()
            .min_h_0()
            .flex_basis(gpui::DefiniteLength::Fraction(1.0 - shown_ratio))
            .child(self.session_pane(second, cx));
        let split_id_for_move = id.to_owned();
        let split_id_for_drop = id.to_owned();
        let split_axis = axis;
        let columns = div()
            .id(format!("split-content-{id}"))
            .flex()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .gap_1()
            .on_drag_move::<DraggedSplitHandle>(cx.listener(
                move |this, event: &gpui::DragMoveEvent<DraggedSplitHandle>, _, cx| {
                    this.preview_split_drag(&split_id_for_move, split_axis, event, cx);
                },
            ))
            .on_drop::<DraggedSplitHandle>(cx.listener(
                move |this, payload: &DraggedSplitHandle, _, cx| {
                    if payload.split_id == split_id_for_drop {
                        this.finish_split_drag(&split_id_for_drop, cx);
                    }
                },
            ));
        let columns = if axis == SplitAxis::Vertical {
            columns.flex_col()
        } else {
            columns.flex_row()
        };
        let handle = self.split_resize_handle(id, axis, cx);
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .child(controls)
            .child(columns.child(first_view).child(handle).child(second_view))
            .into_any_element()
    }
}
