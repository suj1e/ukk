//! Per-device, restart-persistent focus that never enters shared layout state.
//!
//! The selected Pane and Tab are presentation state. They live in a
//! Desktop-local JSON file under the user profile, never in SQLite, the Host
//! workspace protocol or herdr-core. A remote layout edit is reconciled here
//! by dropping stale identifiers and choosing a visible fallback.

use super::*;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use ukk_domain::{DomainSnapshot, PaneNode};

pub(super) const SETTINGS_VERSION: u32 = 1;
const SETTINGS_ENV: &str = "UKK_DESKTOP_SETTINGS";
const MAX_SETTINGS_BYTES: u64 = 1024 * 1024;
const FILE_NAME: &str = "desktop-focus.json";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct SessionFocus {
    /// Pane or tab-group ID inside this Session's layout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane: Option<String>,
    /// Per-tab-group active Tab. Group IDs are durable layout nodes.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tabs: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct LocalFocus {
    #[serde(default = "current_version")]
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_session: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub sessions: BTreeMap<String, SessionFocus>,
}

fn current_version() -> u32 {
    SETTINGS_VERSION
}

impl Default for LocalFocus {
    fn default() -> Self {
        Self {
            version: SETTINGS_VERSION,
            selected_session: None,
            sessions: BTreeMap::new(),
        }
    }
}

/// One visible Pane in visual tree order. A `Leaf` has no tabs; a `Tabs`
/// group carries its ordered member IDs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PaneEntry {
    pub id: String,
    pub tabs: Vec<String>,
}

pub(super) fn pane_entries(node: &PaneNode) -> Vec<PaneEntry> {
    let mut entries = Vec::new();
    collect_panes(node, &mut entries);
    entries
}

fn collect_panes(node: &PaneNode, entries: &mut Vec<PaneEntry>) {
    match node {
        PaneNode::Leaf { id, .. } => entries.push(PaneEntry {
            id: id.clone(),
            tabs: Vec::new(),
        }),
        PaneNode::Tabs { id, tabs } => entries.push(PaneEntry {
            id: id.clone(),
            tabs: tabs.iter().map(|tab| tab.id.clone()).collect(),
        }),
        PaneNode::Split { first, second, .. } => {
            collect_panes(first, entries);
            collect_panes(second, entries);
        }
    }
}

pub(super) fn pane_order(node: &PaneNode) -> Vec<String> {
    pane_entries(node)
        .into_iter()
        .map(|entry| entry.id)
        .collect()
}

/// Choose the neighbour that should receive focus after `removed` disappears.
/// Prefer the next visible item, then the previous one, then the first item.
pub(super) fn recovery_target(
    previous: &[String],
    removed: &str,
    next: &[String],
) -> Option<String> {
    if next.is_empty() {
        return None;
    }
    if let Some(index) = previous.iter().position(|id| id == removed) {
        for candidate in previous[index + 1..]
            .iter()
            .chain(previous[..index].iter().rev())
        {
            if next.contains(candidate) {
                return Some(candidate.clone());
            }
        }
    }
    next.first().cloned()
}

impl LocalFocus {
    pub(super) fn session(&self, session_id: &str) -> Option<&SessionFocus> {
        self.sessions.get(session_id)
    }

    fn session_mut(&mut self, session_id: &str) -> &mut SessionFocus {
        self.sessions.entry(session_id.to_owned()).or_default()
    }

    pub(super) fn pane(&self, session_id: &str) -> Option<&str> {
        self.session(session_id)?.pane.as_deref()
    }

    pub(super) fn tab(&self, session_id: &str, group_id: &str) -> Option<&str> {
        self.session(session_id)?
            .tabs
            .get(group_id)
            .map(String::as_str)
    }

    pub(super) fn set_pane(&mut self, session_id: &str, pane_id: &str) {
        self.session_mut(session_id).pane = Some(pane_id.to_owned());
    }

    pub(super) fn set_tab(&mut self, session_id: &str, group_id: &str, tab_id: &str) {
        self.session_mut(session_id)
            .tabs
            .insert(group_id.to_owned(), tab_id.to_owned());
    }

    pub(super) fn set_selected_session(&mut self, session_id: Option<&str>) {
        self.selected_session = session_id.map(str::to_owned);
    }

    /// Drop focus identifiers that the current shared layout no longer
    /// contains. A missing focused Pane falls back to the first visible Pane;
    /// callers may apply neighbour recovery first for a better transition.
    pub(super) fn reconcile(&mut self, domain: &DomainSnapshot) -> bool {
        let mut changed = false;
        if let Some(selected) = &self.selected_session {
            if !domain
                .sessions
                .iter()
                .any(|session| &session.id == selected)
            {
                self.selected_session = None;
                changed = true;
            }
        }

        let session_ids: HashSet<&str> = domain
            .sessions
            .iter()
            .map(|session| session.id.as_str())
            .collect();
        let before = self.sessions.len();
        self.sessions
            .retain(|session_id, _| session_ids.contains(session_id.as_str()));
        changed |= self.sessions.len() != before;

        for layout in &domain.layouts {
            let Some(focus) = self.sessions.get_mut(&layout.session_id) else {
                continue;
            };
            let entries = pane_entries(&layout.root);
            if let Some(pane) = focus.pane.clone() {
                if !entries.iter().any(|entry| entry.id == pane) {
                    focus.pane = entries.first().map(|entry| entry.id.clone());
                    changed = true;
                }
            }
            let before = focus.tabs.len();
            focus.tabs.retain(|group_id, tab_id| {
                entries
                    .iter()
                    .find(|entry| &entry.id == group_id)
                    .is_some_and(|entry| entry.tabs.iter().any(|tab| tab == tab_id))
            });
            changed |= focus.tabs.len() != before;
        }
        changed
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Platform {
    MacOs,
    Windows,
    Linux,
}

fn current_platform() -> Platform {
    if cfg!(target_os = "macos") {
        Platform::MacOs
    } else if cfg!(windows) {
        Platform::Windows
    } else {
        Platform::Linux
    }
}

/// A user-profile path, or the explicit `UKK_DESKTOP_SETTINGS` override used
/// for isolated test runs. No shared database and no Host capability involved.
pub(super) fn settings_path() -> Option<PathBuf> {
    if let Some(custom) = std::env::var_os(SETTINGS_ENV) {
        if !custom.is_empty() {
            return Some(PathBuf::from(custom));
        }
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let app_data = std::env::var_os("APPDATA").map(PathBuf::from);
    let xdg = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from);
    settings_path_for(
        current_platform(),
        home.as_deref(),
        app_data.as_deref(),
        xdg.as_deref(),
    )
}

pub(super) fn settings_path_for(
    platform: Platform,
    home: Option<&Path>,
    app_data: Option<&Path>,
    xdg_config: Option<&Path>,
) -> Option<PathBuf> {
    match platform {
        Platform::MacOs => {
            home.map(|home| home.join("Library/Application Support/ukk").join(FILE_NAME))
        }
        Platform::Windows => app_data
            .map(|base| base.join("ukk").join(FILE_NAME))
            .or_else(|| home.map(|home| home.join(".config/ukk").join(FILE_NAME))),
        Platform::Linux => xdg_config
            .map(|base| base.join("ukk").join(FILE_NAME))
            .or_else(|| home.map(|home| home.join(".config/ukk").join(FILE_NAME))),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum LoadOutcome {
    Missing,
    Loaded(LocalFocus),
    /// Written by a newer Desktop. Refuse to overwrite it.
    Unsupported(u32),
}

pub(super) fn load_from(path: &Path) -> Result<LoadOutcome, String> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(LoadOutcome::Missing)
        }
        Err(error) => return Err(format!("cannot read focus settings: {error}")),
    };
    if metadata.len() > MAX_SETTINGS_BYTES {
        return Err("focus settings file is unexpectedly large".into());
    }
    let raw =
        fs::read_to_string(path).map_err(|error| format!("cannot read focus settings: {error}"))?;
    let focus: LocalFocus = serde_json::from_str(&raw)
        .map_err(|error| format!("focus settings are not valid JSON: {error}"))?;
    if focus.version > SETTINGS_VERSION {
        return Ok(LoadOutcome::Unsupported(focus.version));
    }
    Ok(LoadOutcome::Loaded(focus))
}

static SAVE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Write atomically so a crash mid-write cannot leave corrupt preferences.
pub(super) fn save_to(path: &Path, focus: &LocalFocus) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "focus settings path has no parent directory".to_owned())?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("cannot create focus settings directory: {error}"))?;
    let encoded = serde_json::to_vec_pretty(focus)
        .map_err(|error| format!("cannot encode focus settings: {error}"))?;
    if encoded.len() as u64 > MAX_SETTINGS_BYTES {
        return Err("focus settings would exceed the size limit".into());
    }
    let unique = SAVE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let temporary = parent.join(format!(".{FILE_NAME}.{}.{unique}.tmp", std::process::id()));
    let write_result = (|| -> std::io::Result<()> {
        let mut file = fs::File::create(&temporary)?;
        file.write_all(&encoded)?;
        file.sync_all()
    })();
    if let Err(error) = write_result {
        let _ = fs::remove_file(&temporary);
        return Err(format!("cannot write focus settings: {error}"));
    }
    match fs::rename(&temporary, path) {
        Ok(()) => Ok(()),
        Err(first_error) => {
            // Windows rename does not replace an existing destination.
            if path.exists() {
                fs::remove_file(path)
                    .map_err(|error| format!("cannot replace focus settings: {error}"))?;
                fs::rename(&temporary, path)
                    .map_err(|error| format!("cannot replace focus settings: {error}"))
            } else {
                let _ = fs::remove_file(&temporary);
                Err(format!("cannot write focus settings: {first_error}"))
            }
        }
    }
}

impl Workspace {
    /// Read persisted focus on a background executor. The UI starts with a
    /// safe default and adopts the file only after it has been validated.
    pub(super) fn load_local_focus(&mut self, cx: &mut Context<Self>) {
        let Some(path) = settings_path() else {
            self.focus_loaded = true;
            self.focus_save_enabled = false;
            self.focus_save_error =
                Some("No user profile directory is available; focus will not persist.".into());
            cx.notify();
            return;
        };
        cx.spawn(async move |this, cx| {
            let (path, result) = cx
                .background_executor()
                .spawn(async move {
                    let loaded = load_from(&path);
                    (path, loaded)
                })
                .await;
            this.update(cx, |view, cx| {
                view.focus_loaded = true;
                view.focus_path = Some(path);
                match result {
                    Ok(LoadOutcome::Loaded(focus)) => {
                        view.local_focus = focus;
                        view.focus_save_enabled = true;
                        view.adopt_loaded_focus();
                    }
                    Ok(LoadOutcome::Missing) => {
                        view.focus_save_enabled = true;
                    }
                    Ok(LoadOutcome::Unsupported(version)) => {
                        view.focus_save_enabled = false;
                        view.focus_save_error = Some(format!(
                            "Focus settings were written by a newer Desktop (v{version}); this device will not overwrite them."
                        ));
                    }
                    Err(error) => {
                        view.focus_save_enabled = true;
                        view.focus_save_error = Some(error);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Reconcile loaded focus with the layout that arrived independently from
    /// the Host. A removed Pane cannot keep or steal focus.
    fn adopt_loaded_focus(&mut self) {
        if self.focus_touched {
            // A user action happened before the read completed; it wins.
            return;
        }
        self.local_focus.reconcile(&self.domain);
        if self.selected_session.is_none() && self.selected.is_none() {
            if let Some(session_id) = self.local_focus.selected_session.clone() {
                if self
                    .domain
                    .sessions
                    .iter()
                    .any(|session| session.id == session_id)
                {
                    self.selected_session = Some(session_id);
                }
            }
        }
    }

    /// Persist asynchronously; generation numbers stop a slow older write
    /// from reporting an error after a newer successful one.
    pub(super) fn persist_local_focus(&mut self, cx: &mut Context<Self>) {
        if !self.focus_loaded || !self.focus_save_enabled {
            return;
        }
        if self.focus_save_in_flight {
            // Coalesce rapid focus changes; the finishing writer re-reads the
            // latest state, so an older snapshot can never land last.
            self.focus_save_pending = true;
            return;
        }
        let Some(path) = self.focus_path.clone() else {
            return;
        };
        self.focus_save_in_flight = true;
        self.focus_save_generation = self.focus_save_generation.wrapping_add(1);
        let generation = self.focus_save_generation;
        let focus = self.local_focus.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { save_to(&path, &focus) })
                .await;
            this.update(cx, |view, cx| {
                view.focus_save_in_flight = false;
                if view.focus_save_generation == generation {
                    view.focus_save_error = result.err();
                }
                if view.focus_save_pending {
                    view.focus_save_pending = false;
                    view.persist_local_focus(cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// A conflict banner plus a local persistence error may both be present.
    pub(super) fn local_focus_status(&self) -> Option<String> {
        self.focus_save_error.clone()
    }

    pub(super) fn focus_persisted_path(&self) -> Option<&std::path::Path> {
        self.focus_path.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ukk_domain::{PaneContent, Session, SessionLayout, Space, SplitAxis};

    fn space() -> Space {
        Space {
            id: "space-1".into(),
            title: "Space".into(),
            project_root: "/tmp".into(),
            created_ms: 0,
        }
    }

    fn session(id: &str) -> Session {
        Session {
            id: id.into(),
            space_id: "space-1".into(),
            title: "Session".into(),
            archived: false,
            created_ms: 0,
            updated_ms: 0,
        }
    }

    fn tabbed(activity: PaneContent) -> PaneNode {
        let root = PaneNode::leaf(activity);
        root.add_tab(root.id(), "Files".into(), PaneContent::Files)
            .unwrap()
    }

    fn domain_with(root: PaneNode) -> DomainSnapshot {
        DomainSnapshot {
            spaces: vec![space()],
            sessions: vec![session("session-1")],
            agent_runs: Vec::new(),
            layouts: vec![SessionLayout {
                session_id: "session-1".into(),
                revision: 4,
                root,
            }],
        }
    }

    fn temp_path(name: &str) -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "ukk-focus-{name}-{}-{unique}/desktop-focus.json",
            std::process::id()
        ))
    }

    #[test]
    fn persistence_round_trip_preserves_pane_and_tab_focus() {
        let path = temp_path("round-trip");
        let mut focus = LocalFocus::default();
        focus.set_selected_session(Some("session-1"));
        focus.set_pane("session-1", "group-1");
        focus.set_tab("session-1", "group-1", "tab-2");
        save_to(&path, &focus).unwrap();
        let loaded = load_from(&path).unwrap();
        assert_eq!(loaded, LoadOutcome::Loaded(focus));
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn persistence_is_local_and_never_carries_host_mutation_fields() {
        let mut focus = LocalFocus::default();
        focus.set_pane("session-1", "pane-1");
        let encoded = serde_json::to_string(&focus).unwrap();
        assert!(!encoded.contains("expected_revision"));
        assert!(!encoded.contains("auth"));
        assert!(!encoded.contains("session_id\":\""));
        assert!(encoded.contains("\"pane\":\"pane-1\""));
    }

    #[test]
    fn unreadable_version_is_reported_instead_of_overwritten() {
        let path = temp_path("version");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, r#"{"version":99,"sessions":{}}"#).unwrap();
        assert_eq!(
            load_from(&path).unwrap(),
            LoadOutcome::Unsupported(99),
            "a newer schema must not be silently replaced"
        );
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn reconcile_drops_removed_sessions_groups_and_tabs() {
        let group = tabbed(PaneContent::Activity);
        let group_id = group.id().to_owned();
        let first_tab = group.leaves()[0].0.to_owned();
        let mut focus = LocalFocus::default();
        focus.set_selected_session(Some("session-1"));
        focus.set_pane("session-1", &group_id);
        focus.set_tab("session-1", &group_id, &first_tab);
        focus.set_pane("deleted-session", "pane");
        focus.set_tab("session-1", "deleted-group", "tab");

        let changed = focus.reconcile(&domain_with(group));
        assert!(changed);
        assert_eq!(focus.selected_session.as_deref(), Some("session-1"));
        assert_eq!(focus.pane("session-1"), Some(group_id.as_str()));
        assert_eq!(focus.tab("session-1", &group_id), Some(first_tab.as_str()));
        assert!(focus.session("session-1").unwrap().tabs.len() == 1);
        assert!(focus.session("deleted-session").is_none());
    }

    #[test]
    fn stale_focused_pane_falls_back_to_a_visible_pane() {
        let root = PaneNode::leaf(PaneContent::Activity);
        let first = root.id().to_owned();
        let split = root
            .split_leaf(&first, SplitAxis::Horizontal, 0.5, PaneContent::Files)
            .unwrap();
        let mut focus = LocalFocus::default();
        focus.set_pane("session-1", "removed-pane");
        assert!(focus.reconcile(&domain_with(split)));
        assert_eq!(focus.pane("session-1"), Some(first.as_str()));
    }

    #[test]
    fn recovery_prefers_next_neighbour_then_previous_then_first() {
        let previous = vec!["a".to_owned(), "b".to_owned(), "c".to_owned()];
        let next = vec!["a".to_owned(), "c".to_owned()];
        assert_eq!(recovery_target(&previous, "b", &next).as_deref(), Some("c"));
        assert_eq!(recovery_target(&previous, "c", &next).as_deref(), Some("a"));
        assert_eq!(
            recovery_target(&["missing".to_owned()], "missing", &next).as_deref(),
            Some("a")
        );
        assert_eq!(recovery_target(&previous, "a", &[]), None);
    }

    #[test]
    fn pane_order_is_visual_depth_first() {
        let first = PaneNode::leaf(PaneContent::Activity);
        let first_id = first.id().to_owned();
        let split = first
            .split_leaf(&first_id, SplitAxis::Vertical, 0.4, PaneContent::Diff)
            .unwrap();
        let PaneNode::Split { second, .. } = &split else {
            panic!("expected split");
        };
        assert_eq!(pane_order(&split), vec![first_id, second.id().to_owned()]);
    }

    #[test]
    fn settings_paths_are_under_the_user_profile_on_every_platform() {
        let home = Path::new("/home/user");
        let app_data = Path::new("/appdata");
        let xdg = Path::new("/xdg");
        assert_eq!(
            settings_path_for(Platform::MacOs, Some(home), None, None).unwrap(),
            PathBuf::from("/home/user/Library/Application Support/ukk/desktop-focus.json")
        );
        assert_eq!(
            settings_path_for(Platform::Windows, Some(home), Some(app_data), None).unwrap(),
            PathBuf::from("/appdata/ukk/desktop-focus.json")
        );
        assert_eq!(
            settings_path_for(Platform::Linux, Some(home), None, Some(xdg)).unwrap(),
            PathBuf::from("/xdg/ukk/desktop-focus.json")
        );
        assert_eq!(
            settings_path_for(Platform::Linux, Some(home), None, None).unwrap(),
            PathBuf::from("/home/user/.config/ukk/desktop-focus.json")
        );
        assert_eq!(settings_path_for(Platform::MacOs, None, None, None), None);
    }
}
