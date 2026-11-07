//! Keyboard-first Pane and Tab navigation.
//!
//! Every shortcut calls the same `PaneAction` path as the pointer controls:
//! an authenticated, revision-checked Host mutation. Keys never bypass local
//! authorization, CAS, or Agent process boundaries.

use super::*;
use gpui::{actions, KeyBinding};

actions!(
    ukk_workspace,
    [
        FocusNextPane,
        FocusPrevPane,
        FocusNextTab,
        FocusPrevTab,
        FocusNextStop,
        FocusPrevStop,
        AddTab,
        CloseTab,
        ReorderTabLeft,
        ReorderTabRight,
        MoveTabToPreviousPane,
        MoveTabToNextPane,
        SplitPaneRight,
        SplitPaneDown,
        ClosePane,
        ShrinkSplit,
        GrowSplit,
        ReloadLayout,
    ]
);

pub(super) const KEY_CONTEXT: &str = "Workspace";

/// Logical shortcuts. Bindings and the in-app help table share this list so a
/// documented shortcut can never silently diverge from the registered keymap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum Shortcut {
    FocusNextPane,
    FocusPrevPane,
    FocusNextTab,
    FocusPrevTab,
    FocusNextStop,
    FocusPrevStop,
    AddTab,
    CloseTab,
    ReorderTabLeft,
    ReorderTabRight,
    MoveTabToPreviousPane,
    MoveTabToNextPane,
    SplitPaneRight,
    SplitPaneDown,
    ClosePane,
    ShrinkSplit,
    GrowSplit,
    ReloadLayout,
}

#[cfg(target_os = "macos")]
mod platform {
    pub const ADD_TAB: &str = "cmd-t";
    pub const CLOSE_TAB: &str = "cmd-w";
    pub const SPLIT_RIGHT: &str = "cmd-d";
    pub const SPLIT_DOWN: &str = "cmd-shift-d";
    pub const CLOSE_PANE: &str = "cmd-shift-w";
    pub const RELOAD: &str = "cmd-shift-r";

    pub const ADD_TAB_HELP: &str = "⌘T";
    pub const CLOSE_TAB_HELP: &str = "⌘W";
    pub const SPLIT_RIGHT_HELP: &str = "⌘D";
    pub const SPLIT_DOWN_HELP: &str = "⌘⇧D";
    pub const CLOSE_PANE_HELP: &str = "⌘⇧W";
    pub const RELOAD_HELP: &str = "⌘⇧R";
}

#[cfg(not(target_os = "macos"))]
mod platform {
    pub const ADD_TAB: &str = "ctrl-t";
    pub const CLOSE_TAB: &str = "ctrl-w";
    pub const SPLIT_RIGHT: &str = "ctrl-d";
    pub const SPLIT_DOWN: &str = "ctrl-shift-d";
    pub const CLOSE_PANE: &str = "ctrl-shift-w";
    pub const RELOAD: &str = "ctrl-shift-r";

    pub const ADD_TAB_HELP: &str = "Ctrl+T";
    pub const CLOSE_TAB_HELP: &str = "Ctrl+W";
    pub const SPLIT_RIGHT_HELP: &str = "Ctrl+D";
    pub const SPLIT_DOWN_HELP: &str = "Ctrl+Shift+D";
    pub const CLOSE_PANE_HELP: &str = "Ctrl+Shift+W";
    pub const RELOAD_HELP: &str = "Ctrl+Shift+R";
}

impl Shortcut {
    pub(super) const ALL: [Shortcut; 18] = [
        Shortcut::FocusNextPane,
        Shortcut::FocusPrevPane,
        Shortcut::FocusNextTab,
        Shortcut::FocusPrevTab,
        Shortcut::FocusNextStop,
        Shortcut::FocusPrevStop,
        Shortcut::AddTab,
        Shortcut::CloseTab,
        Shortcut::ReorderTabLeft,
        Shortcut::ReorderTabRight,
        Shortcut::MoveTabToPreviousPane,
        Shortcut::MoveTabToNextPane,
        Shortcut::SplitPaneRight,
        Shortcut::SplitPaneDown,
        Shortcut::ClosePane,
        Shortcut::ShrinkSplit,
        Shortcut::GrowSplit,
        Shortcut::ReloadLayout,
    ];

    /// Registered keystrokes. More than one alternative is allowed.
    pub(super) fn keystrokes(self) -> &'static [&'static str] {
        match self {
            Shortcut::FocusNextPane => &["ctrl-tab"],
            Shortcut::FocusPrevPane => &["ctrl-shift-tab"],
            Shortcut::FocusNextTab => &["ctrl-pagedown"],
            Shortcut::FocusPrevTab => &["ctrl-pageup"],
            Shortcut::FocusNextStop => &["tab"],
            Shortcut::FocusPrevStop => &["shift-tab"],
            Shortcut::AddTab => &[platform::ADD_TAB],
            Shortcut::CloseTab => &[platform::CLOSE_TAB],
            Shortcut::ReorderTabLeft => &["alt-left"],
            Shortcut::ReorderTabRight => &["alt-right"],
            Shortcut::MoveTabToPreviousPane => &["alt-shift-left"],
            Shortcut::MoveTabToNextPane => &["alt-shift-right"],
            Shortcut::SplitPaneRight => &[platform::SPLIT_RIGHT],
            Shortcut::SplitPaneDown => &[platform::SPLIT_DOWN],
            Shortcut::ClosePane => &[platform::CLOSE_PANE],
            Shortcut::ShrinkSplit => &["ctrl-alt-left", "ctrl-alt-up"],
            Shortcut::GrowSplit => &["ctrl-alt-right", "ctrl-alt-down"],
            Shortcut::ReloadLayout => &[platform::RELOAD],
        }
    }

    /// Full GPUI action name, used by the keymap mapping test.
    #[cfg(test)]
    pub(super) fn action_name(self) -> &'static str {
        match self {
            Shortcut::FocusNextPane => "ukk_workspace::FocusNextPane",
            Shortcut::FocusPrevPane => "ukk_workspace::FocusPrevPane",
            Shortcut::FocusNextTab => "ukk_workspace::FocusNextTab",
            Shortcut::FocusPrevTab => "ukk_workspace::FocusPrevTab",
            Shortcut::FocusNextStop => "ukk_workspace::FocusNextStop",
            Shortcut::FocusPrevStop => "ukk_workspace::FocusPrevStop",
            Shortcut::AddTab => "ukk_workspace::AddTab",
            Shortcut::CloseTab => "ukk_workspace::CloseTab",
            Shortcut::ReorderTabLeft => "ukk_workspace::ReorderTabLeft",
            Shortcut::ReorderTabRight => "ukk_workspace::ReorderTabRight",
            Shortcut::MoveTabToPreviousPane => "ukk_workspace::MoveTabToPreviousPane",
            Shortcut::MoveTabToNextPane => "ukk_workspace::MoveTabToNextPane",
            Shortcut::SplitPaneRight => "ukk_workspace::SplitPaneRight",
            Shortcut::SplitPaneDown => "ukk_workspace::SplitPaneDown",
            Shortcut::ClosePane => "ukk_workspace::ClosePane",
            Shortcut::ShrinkSplit => "ukk_workspace::ShrinkSplit",
            Shortcut::GrowSplit => "ukk_workspace::GrowSplit",
            Shortcut::ReloadLayout => "ukk_workspace::ReloadLayout",
        }
    }

    pub(super) fn help_keys(self) -> &'static str {
        match self {
            Shortcut::FocusNextPane => "Ctrl+Tab",
            Shortcut::FocusPrevPane => "Ctrl+Shift+Tab",
            Shortcut::FocusNextTab => "Ctrl+PageDown",
            Shortcut::FocusPrevTab => "Ctrl+PageUp",
            Shortcut::FocusNextStop => "Tab",
            Shortcut::FocusPrevStop => "Shift+Tab",
            Shortcut::AddTab => platform::ADD_TAB_HELP,
            Shortcut::CloseTab => platform::CLOSE_TAB_HELP,
            Shortcut::ReorderTabLeft => "Alt+←",
            Shortcut::ReorderTabRight => "Alt+→",
            Shortcut::MoveTabToPreviousPane => "Alt+Shift+←",
            Shortcut::MoveTabToNextPane => "Alt+Shift+→",
            Shortcut::SplitPaneRight => platform::SPLIT_RIGHT_HELP,
            Shortcut::SplitPaneDown => platform::SPLIT_DOWN_HELP,
            Shortcut::ClosePane => platform::CLOSE_PANE_HELP,
            Shortcut::ShrinkSplit => "Ctrl+Alt+←/↑",
            Shortcut::GrowSplit => "Ctrl+Alt+→/↓",
            Shortcut::ReloadLayout => platform::RELOAD_HELP,
        }
    }

    pub(super) fn description(self) -> &'static str {
        match self {
            Shortcut::FocusNextPane => "Focus next Pane",
            Shortcut::FocusPrevPane => "Focus previous Pane",
            Shortcut::FocusNextTab => "Select next Tab",
            Shortcut::FocusPrevTab => "Select previous Tab",
            Shortcut::FocusNextStop => "Move keyboard focus forward",
            Shortcut::FocusPrevStop => "Move keyboard focus backward",
            Shortcut::AddTab => "Add a Tab to the focused Pane",
            Shortcut::CloseTab => "Close the active Tab",
            Shortcut::ReorderTabLeft => "Move active Tab left",
            Shortcut::ReorderTabRight => "Move active Tab right",
            Shortcut::MoveTabToPreviousPane => "Move active Tab to previous Pane",
            Shortcut::MoveTabToNextPane => "Move active Tab to next Pane",
            Shortcut::SplitPaneRight => "Split focused Pane horizontally",
            Shortcut::SplitPaneDown => "Split focused Pane vertically",
            Shortcut::ClosePane => "Close focused Pane",
            Shortcut::ShrinkSplit => "Shrink the enclosing split",
            Shortcut::GrowSplit => "Grow the enclosing split",
            Shortcut::ReloadLayout => "Reload layout after a conflict",
        }
    }
}

pub(super) struct ShortcutHelp {
    pub keys: &'static str,
    pub description: &'static str,
}

pub(super) fn shortcut_help() -> Vec<ShortcutHelp> {
    Shortcut::ALL
        .iter()
        .map(|shortcut| ShortcutHelp {
            keys: shortcut.help_keys(),
            description: shortcut.description(),
        })
        .collect()
}

fn binding_for(shortcut: Shortcut, keystrokes: &'static str) -> KeyBinding {
    let context = Some(KEY_CONTEXT);
    match shortcut {
        Shortcut::FocusNextPane => KeyBinding::new(keystrokes, FocusNextPane, context),
        Shortcut::FocusPrevPane => KeyBinding::new(keystrokes, FocusPrevPane, context),
        Shortcut::FocusNextTab => KeyBinding::new(keystrokes, FocusNextTab, context),
        Shortcut::FocusPrevTab => KeyBinding::new(keystrokes, FocusPrevTab, context),
        Shortcut::FocusNextStop => KeyBinding::new(keystrokes, FocusNextStop, context),
        Shortcut::FocusPrevStop => KeyBinding::new(keystrokes, FocusPrevStop, context),
        Shortcut::AddTab => KeyBinding::new(keystrokes, AddTab, context),
        Shortcut::CloseTab => KeyBinding::new(keystrokes, CloseTab, context),
        Shortcut::ReorderTabLeft => KeyBinding::new(keystrokes, ReorderTabLeft, context),
        Shortcut::ReorderTabRight => KeyBinding::new(keystrokes, ReorderTabRight, context),
        Shortcut::MoveTabToPreviousPane => {
            KeyBinding::new(keystrokes, MoveTabToPreviousPane, context)
        }
        Shortcut::MoveTabToNextPane => KeyBinding::new(keystrokes, MoveTabToNextPane, context),
        Shortcut::SplitPaneRight => KeyBinding::new(keystrokes, SplitPaneRight, context),
        Shortcut::SplitPaneDown => KeyBinding::new(keystrokes, SplitPaneDown, context),
        Shortcut::ClosePane => KeyBinding::new(keystrokes, ClosePane, context),
        Shortcut::ShrinkSplit => KeyBinding::new(keystrokes, ShrinkSplit, context),
        Shortcut::GrowSplit => KeyBinding::new(keystrokes, GrowSplit, context),
        Shortcut::ReloadLayout => KeyBinding::new(keystrokes, ReloadLayout, context),
    }
}

pub(crate) fn bindings() -> Vec<KeyBinding> {
    Shortcut::ALL
        .iter()
        .flat_map(|shortcut| {
            shortcut
                .keystrokes()
                .iter()
                .map(move |keystrokes| binding_for(*shortcut, keystrokes))
        })
        .collect()
}

/// Generate one GPUI action listener per shortcut. Keeping these generated
/// from the same enum makes an unhandled registered shortcut impossible.
macro_rules! shortcut_listeners {
    ($($method:ident: $action:ident => $shortcut:ident),* $(,)?) => {
        impl Workspace {
            $(
                pub(super) fn $method(
                    &mut self,
                    _: &$action,
                    window: &mut Window,
                    cx: &mut Context<Self>,
                ) {
                    self.run_shortcut(Shortcut::$shortcut, window, cx);
                }
            )*
        }
    };
}

shortcut_listeners! {
    on_focus_next_pane: FocusNextPane => FocusNextPane,
    on_focus_prev_pane: FocusPrevPane => FocusPrevPane,
    on_focus_next_tab: FocusNextTab => FocusNextTab,
    on_focus_prev_tab: FocusPrevTab => FocusPrevTab,
    on_focus_next_stop: FocusNextStop => FocusNextStop,
    on_focus_prev_stop: FocusPrevStop => FocusPrevStop,
    on_add_tab: AddTab => AddTab,
    on_close_tab: CloseTab => CloseTab,
    on_reorder_tab_left: ReorderTabLeft => ReorderTabLeft,
    on_reorder_tab_right: ReorderTabRight => ReorderTabRight,
    on_move_tab_previous_pane: MoveTabToPreviousPane => MoveTabToPreviousPane,
    on_move_tab_next_pane: MoveTabToNextPane => MoveTabToNextPane,
    on_split_pane_right: SplitPaneRight => SplitPaneRight,
    on_split_pane_down: SplitPaneDown => SplitPaneDown,
    on_close_pane: ClosePane => ClosePane,
    on_shrink_split: ShrinkSplit => ShrinkSplit,
    on_grow_split: GrowSplit => GrowSplit,
    on_reload_layout: ReloadLayout => ReloadLayout,
}

impl Workspace {
    pub(super) fn run_shortcut(
        &mut self,
        shortcut: Shortcut,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !matches!(shortcut, Shortcut::FocusNextStop | Shortcut::FocusPrevStop) {
            // A keyboard command supersedes any uncommitted pointer preview.
            self.cancel_split_drag(cx);
        }
        match shortcut {
            Shortcut::FocusNextPane => self.cycle_focused_pane(1, cx),
            Shortcut::FocusPrevPane => self.cycle_focused_pane(-1, cx),
            Shortcut::FocusNextTab => self.cycle_focused_tab(1, cx),
            Shortcut::FocusPrevTab => self.cycle_focused_tab(-1, cx),
            Shortcut::FocusNextStop => window.focus_next(cx),
            Shortcut::FocusPrevStop => window.focus_prev(cx),
            Shortcut::AddTab => self.add_tab_to_focused_pane(cx),
            Shortcut::CloseTab => self.close_focused_tab(cx),
            Shortcut::ReorderTabLeft => self.reorder_focused_tab(-1, cx),
            Shortcut::ReorderTabRight => self.reorder_focused_tab(1, cx),
            Shortcut::MoveTabToPreviousPane => self.move_focused_tab_to_adjacent_pane(-1, cx),
            Shortcut::MoveTabToNextPane => self.move_focused_tab_to_adjacent_pane(1, cx),
            Shortcut::SplitPaneRight => self.split_focused_pane(SplitAxis::Horizontal, cx),
            Shortcut::SplitPaneDown => self.split_focused_pane(SplitAxis::Vertical, cx),
            Shortcut::ClosePane => self.close_focused_pane(cx),
            Shortcut::ShrinkSplit => self.resize_enclosing_split(-1, cx),
            Shortcut::GrowSplit => self.resize_enclosing_split(1, cx),
            Shortcut::ReloadLayout => self.refresh(cx),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::Keystroke;
    use std::collections::HashSet;

    #[test]
    fn every_shortcut_has_a_registered_binding_in_the_workspace_context() {
        let bindings = bindings();
        let registered: HashSet<&str> = bindings
            .iter()
            .map(|binding| binding.action().name())
            .collect();
        for shortcut in Shortcut::ALL {
            assert!(
                registered.contains(shortcut.action_name()),
                "{} must be bound",
                shortcut.action_name()
            );
        }
        assert_eq!(bindings.len(), 20);
    }

    #[test]
    fn bindings_are_unique_keystroke_sequences() {
        let mut seen = HashSet::new();
        for binding in bindings() {
            let sequence = format!("{:?}", binding.keystrokes());
            assert!(
                seen.insert(sequence.clone()),
                "duplicate shortcut registration for {sequence}"
            );
        }
    }

    #[test]
    fn every_registered_keystroke_parses() {
        for shortcut in Shortcut::ALL {
            for keystrokes in shortcut.keystrokes() {
                for keystroke in keystrokes.split(' ') {
                    Keystroke::parse(keystroke)
                        .unwrap_or_else(|error| panic!("{keystrokes}: {error}"));
                }
            }
        }
    }

    #[test]
    fn platform_primary_modifier_matches_the_documented_help() {
        #[cfg(target_os = "macos")]
        assert!(Shortcut::AddTab.keystrokes()[0].starts_with("cmd-"));
        #[cfg(not(target_os = "macos"))]
        assert!(Shortcut::AddTab.keystrokes()[0].starts_with("ctrl-"));
        assert_eq!(Shortcut::AddTab.help_keys(), platform::ADD_TAB_HELP);
    }

    #[test]
    fn shortcut_help_covers_every_shortcut_once() {
        let help = shortcut_help();
        assert_eq!(help.len(), Shortcut::ALL.len());
        let descriptions: HashSet<&str> = help.iter().map(|entry| entry.description).collect();
        assert_eq!(descriptions.len(), help.len());
    }
}
