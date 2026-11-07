//! Native splitter drag behavior. Persist once per completed gesture; never
//! make SQLite writes on every mouse move or couple Pane state to AgentRun.
use super::*;

const MIN_SPLIT_RATIO: f32 = 0.15;
const MAX_SPLIT_RATIO: f32 = 0.85;

/// Keep the mouse math pure and testable. Zero-size/unusable bounds cannot
/// propagate NaNs into a validated persistent PaneNode.
fn ratio_from_offset(offset: f32, size: f32) -> Option<f32> {
    if !size.is_finite() || size <= 0.0 || !offset.is_finite() {
        return None;
    }
    Some((offset / size).clamp(MIN_SPLIT_RATIO, MAX_SPLIT_RATIO))
}

fn drag_ratio(
    bounds: gpui::Bounds<gpui::Pixels>,
    position: gpui::Point<gpui::Pixels>,
    axis: SplitAxis,
) -> Option<f32> {
    let (offset, size) = match axis {
        SplitAxis::Horizontal => (
            position.x - bounds.left(),
            bounds.right() - bounds.left(),
        ),
        SplitAxis::Vertical => (
            position.y - bounds.top(),
            bounds.bottom() - bounds.top(),
        ),
    };
    // The GPUI pixel ratio is dimensionless f32.
    if size <= px(0.0) {
        return None;
    }
    let raw = offset / size;
    ratio_from_offset(raw, 1.0)
}

impl Workspace {
    /// Use a local preview during dragging; the durable tree and its
    /// revision remain untouched until the pointer is released.
    pub(super) fn preview_split_drag(
        &mut self,
        target: &str,
        axis: SplitAxis,
        event: &gpui::DragMoveEvent<DraggedSplitHandle>,
        cx: &mut Context<Self>,
    ) {
        let Some(payload) = event.dragged_item().downcast_ref::<DraggedSplitHandle>() else {
            return;
        };
        if payload.split_id != target || self.mutation_busy {
            return;
        }
        let Some(session_id) = self.selected_session.clone() else {
            return;
        };
        let Some(ratio) = drag_ratio(event.bounds, event.event.position, axis) else {
            return;
        };
        self.split_drag_preview = Some(SplitDragPreview {
            session_id,
            split_id: target.to_owned(),
            ratio,
        });
        cx.notify();
    }

    pub(super) fn displayed_split_ratio(&self, split_id: &str, stored: f32) -> f32 {
        match &self.split_drag_preview {
            Some(preview)
                if preview.split_id == split_id
                    && self.selected_session.as_deref() == Some(preview.session_id.as_str()) =>
            {
                preview.ratio
            }
            _ => stored,
        }
    }

    /// A real Host command is sent only after the gesture ends; Host uses a
    /// compare-and-swap revision and will reject stale simultaneous editors.
    pub(super) fn finish_split_drag(&mut self, split_id: &str, cx: &mut Context<Self>) {
        let Some(preview) = self.split_drag_preview.as_ref() else {
            return;
        };
        if preview.split_id != split_id {
            return;
        }
        let preview = self.split_drag_preview.take().expect("checked above");
        if !self.mutation_busy
            && self.selected_session.as_deref() == Some(&preview.session_id)
        {
            self.pane_action(
                PaneAction::Resize {
                    split_id: preview.split_id,
                    ratio: preview.ratio,
                },
                cx,
            );
        }
        cx.notify();
    }

    pub(super) fn cancel_split_drag(&mut self, cx: &mut Context<Self>) {
        if self.split_drag_preview.take().is_some() {
            cx.notify();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ratio_from_offset;

    #[test]
    fn cursor_ratio_is_clamped_to_domain_constraints() {
        assert_eq!(ratio_from_offset(-200.0, 200.0), Some(0.15));
        assert_eq!(ratio_from_offset(50.0, 200.0), Some(0.25));
        assert_eq!(ratio_from_offset(1000.0, 200.0), Some(0.85));
    }

    #[test]
    fn invalid_drag_geometry_is_ignored() {
        assert_eq!(ratio_from_offset(5.0, 0.0), None);
        assert_eq!(ratio_from_offset(5.0, f32::NAN), None);
        assert_eq!(ratio_from_offset(f32::INFINITY, 10.0), None);
    }
}
