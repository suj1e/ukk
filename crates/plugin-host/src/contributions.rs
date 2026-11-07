//! Read-only contribution descriptors consumed by native UI adapters.
//!
//! This is the seam a GPUI (or SwiftUI) layer renders. It contains no GPUI
//! types, no processes and no mutation: the UI reads descriptors from here and
//! sends user intent back through [`crate::PluginHost::invoke`], which enforces
//! ownership and grants.
use ukk_plugin_api::{
    CommandScope, ContributionSlot, OverlayDescriptor, PluginId, UiViewContribution,
};

/// One view contributed by an installed, enabled plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewDescriptor {
    plugin_id: PluginId,
    view_id: String,
    slot: ContributionSlot,
    title: String,
    command: Option<String>,
    overlay: Option<OverlayDescriptor>,
}

impl ViewDescriptor {
    pub(crate) fn from_view(plugin_id: &PluginId, view: &UiViewContribution) -> Self {
        Self {
            plugin_id: plugin_id.clone(),
            view_id: view.id().to_owned(),
            slot: view.slot(),
            title: view.title().to_owned(),
            command: view.command().map(str::to_owned),
            overlay: view.overlay(),
        }
    }

    pub fn plugin_id(&self) -> &PluginId {
        &self.plugin_id
    }

    pub fn view_id(&self) -> &str {
        &self.view_id
    }

    pub fn slot(&self) -> ContributionSlot {
        self.slot
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    /// Command to invoke when the user activates this view, if the plugin
    /// declared one.
    pub fn command(&self) -> Option<&str> {
        self.command.as_deref()
    }

    /// Bounded geometry for `session.overlay` views.
    pub fn overlay(&self) -> Option<OverlayDescriptor> {
        self.overlay
    }
}

/// One declared command offered by an enabled plugin, for a command palette.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandDescriptor {
    plugin_id: PluginId,
    command_id: String,
    title: String,
    scope: CommandScope,
    requires: Vec<String>,
}

impl CommandDescriptor {
    pub(crate) fn new(
        plugin_id: &PluginId,
        command_id: &str,
        title: &str,
        scope: CommandScope,
        requires: Vec<String>,
    ) -> Self {
        Self {
            plugin_id: plugin_id.clone(),
            command_id: command_id.to_owned(),
            title: title.to_owned(),
            scope,
            requires,
        }
    }

    pub fn plugin_id(&self) -> &PluginId {
        &self.plugin_id
    }

    pub fn command_id(&self) -> &str {
        &self.command_id
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn scope(&self) -> CommandScope {
        self.scope
    }

    /// Capabilities the user must grant before the command can run.
    pub fn requires(&self) -> &[String] {
        &self.requires
    }
}

/// Everything enabled plugins contribute to one client platform.
///
/// Only plugins in the `Ready` state appear: a disabled or failed plugin
/// contributes nothing and receives no events.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContributionSnapshot {
    pub session_actions: Vec<ViewDescriptor>,
    pub session_tabs: Vec<ViewDescriptor>,
    pub session_overlays: Vec<ViewDescriptor>,
    pub context_panels: Vec<ViewDescriptor>,
    pub settings_sections: Vec<ViewDescriptor>,
    /// Commands from the same ready plugins, for palettes and menus.
    pub commands: Vec<CommandDescriptor>,
}

impl ContributionSnapshot {
    pub fn is_empty(&self) -> bool {
        self.session_actions.is_empty()
            && self.session_tabs.is_empty()
            && self.session_overlays.is_empty()
            && self.context_panels.is_empty()
            && self.settings_sections.is_empty()
            && self.commands.is_empty()
    }
}
