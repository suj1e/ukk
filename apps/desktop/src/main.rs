//! ukk Desktop entrypoint. UI state and views live under workspace/.
mod theme;
mod workspace;

use gpui::{prelude::*, px, size, App, Bounds, WindowBounds, WindowOptions};
use gpui_platform::application;
use workspace::Workspace;

fn main() {
    application().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1280.0), px(790.0)), cx);
        if let Err(error) = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_, cx| cx.new(Workspace::new),
        ) {
            eprintln!("Unable to open ukk Desktop window: {error}");
            return;
        }
        cx.activate(true);
    });
}
