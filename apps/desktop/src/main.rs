//! ukk Desktop entrypoint. UI state and views live under workspace/.
mod build_info;
mod theme;
mod workspace;

use gpui::{prelude::*, px, size, App, Bounds, WindowBounds, WindowOptions};
use gpui_platform::application;
use workspace::Workspace;

fn main() {
    if let Some(argument) = std::env::args().nth(1) {
        if matches!(argument.as_str(), "version" | "--version" | "-V") {
            println!("{}", build_info::version_line());
            return;
        }
    }

    application().run(|cx: &mut App| {
        cx.bind_keys(workspace::key_bindings());
        let bounds = Bounds::centered(None, size(px(1280.0), px(790.0)), cx);
        if let Err(error) = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| Workspace::new(window, cx)),
        ) {
            eprintln!("Unable to open ukk Desktop window: {error}");
            return;
        }
        cx.activate(true);
    });
}
