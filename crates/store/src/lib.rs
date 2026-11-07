//! Durable workspace metadata for ukk. The authoritative terminal lifecycle
//! remains in herdr-core. This crate *never* launches, stops, or kills agents.
//!
//! SQLite with foreign keys, a schema version and optimistic layout revisions.
//! Opening the DB is not tied to a GPUI window, and all writes are transactions.

use rusqlite::{params, Connection, OptionalExtension};
use std::env;
use std::error::Error;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use ukk_domain::{
    new_id, AgentRun, DomainSnapshot, LayoutError, PaneContent, PaneNode, Session, SessionLayout,
    Space,
};

#[derive(Debug)]
pub enum StoreError {
    Sql(rusqlite::Error),
    Json(serde_json::Error),
    Io(std::io::Error),
    Layout(LayoutError),
    Invalid(String),
    NotFound(String),
    Conflict,
    UnsupportedVersion(i64),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sql(e) => write!(f, "SQLite: {e}"),
            Self::Json(e) => write!(f, "layout JSON: {e}"),
            Self::Io(e) => write!(f, "filesystem: {e}"),
            Self::Layout(e) => write!(f, "invalid pane layout: {e}"),
            Self::Invalid(s) => write!(f, "invalid input: {s}"),
            Self::NotFound(s) => write!(f, "not found: {s}"),
            Self::Conflict => write!(f, "layout revision conflict; reload before retrying"),
            Self::UnsupportedVersion(v) => write!(f, "database schema version {v} is newer than supported"),
        }
    }
}

impl Error for StoreError {}

impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> Self { Self::Sql(e) }
}
impl From<serde_json::Error> for StoreError {
    fn from(e: serde_json::Error) -> Self { Self::Json(e) }
}
impl From<std::io::Error> for StoreError {
    fn from(e: std::io::Error) -> Self { Self::Io(e) }
}
impl From<LayoutError> for StoreError {
    fn from(e: LayoutError) -> Self { Self::Layout(e) }
}

pub type Result<T> = std::result::Result<T, StoreError>;

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn nonempty(value: &str, field: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > 200 {
        return Err(StoreError::Invalid(format!("{field} must contain 1–200 characters")));
    }
    Ok(value.to_owned())
}

/// Local-only configuration. The folder is created by the Host process, not
/// by the Desktop UI, so closing the window cannot remove persisted metadata.
pub fn default_database_path() -> Result<PathBuf> {
    if let Some(path) = env::var_os("UKK_DB_PATH") {
        return Ok(PathBuf::from(path));
    }
    #[cfg(windows)]
    {
        let local = env::var_os("LOCALAPPDATA")
            .ok_or_else(|| StoreError::Invalid("LOCALAPPDATA is not set".into()))?;
        Ok(PathBuf::from(local).join("ukk").join("workspace.sqlite3"))
    }
    #[cfg(not(windows))]
    {
        let home = env::var_os("HOME")
            .ok_or_else(|| StoreError::Invalid("HOME is not set".into()))?;
        Ok(PathBuf::from(home)
            .join(".ukk")
            .join("data")
            .join("workspace.sqlite3"))
    }
}

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open_default() -> Result<Self> {
        Self::open(&default_database_path()?)
    }

    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        // On Unix, ensure SQLite creates the DB with 0600 permissions rather
        // than depending on a permissive process umask. Do not truncate an
        // existing file. Directory permissions are the deployment owner's job
        // for explicitly configured UKK_DB_PATH.
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            fs::OpenOptions::new()
                .create(true)
                .append(true)
                .mode(0o600)
                .open(path)?;
        }
        let conn = Connection::open(path)?;
        conn.busy_timeout(Duration::from_secs(3))?;
        conn.execute_batch("PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL;")?;
        let mut store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    pub fn open_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        let mut store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&mut self) -> Result<()> {
        let version: i64 = self.conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version > 1 {
            return Err(StoreError::UnsupportedVersion(version));
        }
        if version == 1 {
            return Ok(());
        }
        let tx = self.conn.transaction()?;
        tx.execute_batch(
            r#"
            CREATE TABLE spaces (
                id TEXT PRIMARY KEY NOT NULL,
                title TEXT NOT NULL,
                project_root TEXT NOT NULL,
                created_ms INTEGER NOT NULL
            );
            CREATE TABLE sessions (
                id TEXT PRIMARY KEY NOT NULL,
                space_id TEXT NOT NULL REFERENCES spaces(id) ON DELETE RESTRICT,
                title TEXT NOT NULL,
                archived INTEGER NOT NULL DEFAULT 0 CHECK (archived IN (0, 1)),
                created_ms INTEGER NOT NULL,
                updated_ms INTEGER NOT NULL
            );
            CREATE INDEX sessions_by_space ON sessions(space_id, created_ms DESC);
            CREATE TABLE agent_runs (
                id TEXT PRIMARY KEY NOT NULL,
                session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                core_id TEXT NOT NULL,
                terminal_address TEXT NOT NULL,
                parent_run_id TEXT REFERENCES agent_runs(id) ON DELETE SET NULL,
                adapter TEXT NOT NULL,
                created_ms INTEGER NOT NULL,
                UNIQUE(core_id, terminal_address)
            );
            CREATE INDEX agents_by_session ON agent_runs(session_id);
            CREATE TABLE session_layouts (
                session_id TEXT PRIMARY KEY NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                revision INTEGER NOT NULL CHECK (revision >= 0),
                content_json TEXT NOT NULL
            );
            PRAGMA user_version = 1;
            "#,
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn create_space(&mut self, title: &str, project_root: &str) -> Result<Space> {
        let title = nonempty(title, "Space title")?;
        let project_root = nonempty(project_root, "project root")?;
        let space = Space {
            id: new_id(),
            title,
            project_root,
            created_ms: now_ms(),
        };
        self.conn.execute(
            "INSERT INTO spaces(id,title,project_root,created_ms) VALUES(?1,?2,?3,?4)",
            params![space.id, space.title, space.project_root, space.created_ms],
        )?;
        Ok(space)
    }

    pub fn create_session(&mut self, space_id: &str, title: &str) -> Result<Session> {
        let title = nonempty(title, "Session title")?;
        let space_id = nonempty(space_id, "Space ID")?;
        let exists: Option<String> = self.conn.query_row(
            "SELECT id FROM spaces WHERE id=?1", [&space_id], |row| row.get(0),
        ).optional()?;
        if exists.is_none() {
            return Err(StoreError::NotFound(format!("space {space_id}")));
        }
        let created = now_ms();
        let session = Session {
            id: new_id(),
            space_id,
            title,
            archived: false,
            created_ms: created,
            updated_ms: created,
        };
        let layout = SessionLayout::new(session.id.clone());
        let encoded = serde_json::to_string(&layout.root)?;
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO sessions(id,space_id,title,archived,created_ms,updated_ms) VALUES(?1,?2,?3,0,?4,?5)",
            params![session.id, session.space_id, session.title, session.created_ms, session.updated_ms],
        )?;
        tx.execute(
            "INSERT INTO session_layouts(session_id,revision,content_json) VALUES(?1,0,?2)",
            params![session.id, encoded],
        )?;
        tx.commit()?;
        Ok(session)
    }

    /// An explicit mapping to an *existing* runtime terminal. This operation
    /// never launches the agent or alters the PTY lifetime. Do not infer a
    /// product Session from a terminal without a user-defined association.
    pub fn bind_agent(
        &mut self,
        session_id: &str,
        core_id: &str,
        terminal_address: &str,
        adapter: &str,
        parent_run_id: Option<&str>,
    ) -> Result<AgentRun> {
        let session_id = nonempty(session_id, "Session ID")?;
        let core_id = nonempty(core_id, "herdr-core ID")?;
        let terminal_address = nonempty(terminal_address, "terminal address")?;
        let adapter = nonempty(adapter, "adapter")?;
        let session: Option<String> = self.conn.query_row(
            "SELECT id FROM sessions WHERE id=?1", [&session_id], |row| row.get(0),
        ).optional()?;
        if session.is_none() {
            return Err(StoreError::NotFound(format!("session {session_id}")));
        }
        if let Some(parent) = parent_run_id {
            let parent_session: Option<String> = self.conn.query_row(
                "SELECT session_id FROM agent_runs WHERE id=?1", [parent], |row| row.get(0),
            ).optional()?;
            if parent_session.as_deref() != Some(&session_id) {
                return Err(StoreError::Invalid("parent AgentRun must belong to the same Session".into()));
            }
        }
        let run = AgentRun {
            id: new_id(),
            session_id,
            core_id,
            terminal_address,
            parent_run_id: parent_run_id.map(str::to_owned),
            adapter,
            created_ms: now_ms(),
        };
        self.conn.execute(
            "INSERT INTO agent_runs(id,session_id,core_id,terminal_address,parent_run_id,adapter,created_ms) VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![run.id, run.session_id, run.core_id, run.terminal_address, run.parent_run_id, run.adapter, run.created_ms],
        )?;
        Ok(run)
    }

    pub fn archive_session(&mut self, session_id: &str) -> Result<()> {
        let count = self.conn.execute(
            "UPDATE sessions SET archived=1, updated_ms=?2 WHERE id=?1",
            params![session_id, now_ms()],
        )?;
        if count == 0 {
            return Err(StoreError::NotFound(format!("session {session_id}")));
        }
        // Intentionally no interaction with herdr-core terminals.
        Ok(())
    }

    pub fn get_layout(&self, session_id: &str) -> Result<SessionLayout> {
        let row: Option<(i64, String)> = self.conn.query_row(
            "SELECT revision,content_json FROM session_layouts WHERE session_id=?1",
            [session_id], |row| Ok((row.get(0)?, row.get(1)?)),
        ).optional()?;
        let (revision, encoded) = row.ok_or_else(|| StoreError::NotFound(format!("layout for {session_id}")))?;
        let root: PaneNode = serde_json::from_str(&encoded)?;
        root.validate()?;
        Ok(SessionLayout {
            session_id: session_id.to_owned(),
            revision,
            root,
        })
    }

    /// Atomic compare-and-swap. Saving a new view does not touch AgentRun.
    pub fn set_layout(
        &mut self,
        session_id: &str,
        expected_revision: i64,
        root: PaneNode,
    ) -> Result<SessionLayout> {
        root.validate()?;
        for (_, content) in root.leaves() {
            if let PaneContent::Agent { agent_run_id } | PaneContent::Terminal { agent_run_id } = content {
                let matching_session: Option<String> = self.conn.query_row(
                    "SELECT session_id FROM agent_runs WHERE id=?1",
                    [agent_run_id],
                    |row| row.get(0),
                ).optional()?;
                if matching_session.as_deref() != Some(session_id) {
                    return Err(StoreError::Invalid("pane's AgentRun must belong to this Session".into()));
                }
            }
        }
        let encoded = serde_json::to_string(&root)?;
        let revision = expected_revision.checked_add(1)
            .ok_or(StoreError::Invalid("layout revision overflow".into()))?;
        let changed = self.conn.execute(
            "UPDATE session_layouts SET revision=?1,content_json=?2 WHERE session_id=?3 AND revision=?4",
            params![revision, encoded, session_id, expected_revision],
        )?;
        if changed == 0 {
            return match self.get_layout(session_id) {
                Err(StoreError::NotFound(_)) => Err(StoreError::NotFound(format!("layout for {session_id}"))),
                _ => Err(StoreError::Conflict),
            };
        }
        Ok(SessionLayout { session_id: session_id.to_owned(), revision, root })
    }

    pub fn snapshot(&self) -> Result<DomainSnapshot> {
        let mut spaces = Vec::new();
        {
            let mut stmt = self.conn.prepare(
                "SELECT id,title,project_root,created_ms FROM spaces ORDER BY created_ms,id",
            )?;
            let entries = stmt.query_map([], |row| Ok(Space {
                id: row.get(0)?, title: row.get(1)?, project_root: row.get(2)?,
                created_ms: row.get(3)?,
            }))?;
            for entry in entries { spaces.push(entry?); }
        }
        let mut sessions = Vec::new();
        {
            let mut stmt = self.conn.prepare(
                "SELECT id,space_id,title,archived,created_ms,updated_ms FROM sessions ORDER BY created_ms,id",
            )?;
            let entries = stmt.query_map([], |row| Ok(Session {
                id: row.get(0)?, space_id: row.get(1)?, title: row.get(2)?,
                archived: row.get::<_, i64>(3)? != 0,
                created_ms: row.get(4)?, updated_ms: row.get(5)?,
            }))?;
            for entry in entries { sessions.push(entry?); }
        }
        let mut agent_runs = Vec::new();
        {
            let mut stmt = self.conn.prepare(
                "SELECT id,session_id,core_id,terminal_address,parent_run_id,adapter,created_ms FROM agent_runs ORDER BY created_ms,id",
            )?;
            let entries = stmt.query_map([], |row| Ok(AgentRun {
                id: row.get(0)?, session_id: row.get(1)?, core_id: row.get(2)?,
                terminal_address: row.get(3)?, parent_run_id: row.get(4)?,
                adapter: row.get(5)?, created_ms: row.get(6)?,
            }))?;
            for entry in entries { agent_runs.push(entry?); }
        }
        let mut layouts = Vec::new();
        {
            let mut stmt = self.conn.prepare(
                "SELECT session_id,revision,content_json FROM session_layouts ORDER BY session_id",
            )?;
            let entries = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?, row.get::<_, String>(2)?))
            })?;
            for entry in entries {
                let (session_id, revision, json) = entry?;
                let root: PaneNode = serde_json::from_str(&json)?;
                root.validate()?;
                layouts.push(SessionLayout { session_id, revision, root });
            }
        }
        Ok(DomainSnapshot { spaces, sessions, agent_runs, layouts })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ukk_domain::{PaneContent, SplitAxis};

    #[test]
    fn session_and_layout_persist_across_reopen() {
        let dir = env::temp_dir().join(format!("ukk-store-{}", new_id()));
        let path = dir.join("workspace.db");
        let space_id;
        let session_id;
        let leaf_id;
        {
            let mut store = Store::open(&path).unwrap();
            let space = store.create_space("ukk", "/work/ukk").unwrap();
            space_id = space.id;
            let session = store.create_session(&space_id, "Implement sync").unwrap();
            session_id = session.id;
            let layout = store.get_layout(&session_id).unwrap();
            leaf_id = layout.root.id().to_owned();
            let split = layout.root.split_leaf(&leaf_id, SplitAxis::Horizontal, 0.55, PaneContent::Diff).unwrap();
            store.set_layout(&session_id, 0, split).unwrap();
        }
        {
            let store = Store::open(&path).unwrap();
            let snap = store.snapshot().unwrap();
            assert_eq!(snap.spaces[0].id, space_id);
            assert_eq!(snap.sessions[0].id, session_id);
            assert_eq!(snap.layouts[0].revision, 1);
            assert_eq!(snap.layouts[0].root.leaves().len(), 2);
            assert_eq!(snap.layouts[0].root.leaves()[0].0, leaf_id);
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn closing_pane_or_archiving_session_does_not_remove_agent() {
        let mut store = Store::open_memory().unwrap();
        let space = store.create_space("project", "/work").unwrap();
        let session = store.create_session(&space.id, "parallel agents").unwrap();
        let agent = store.bind_agent(&session.id, "hc-core-1", "t9", "codex", None).unwrap();
        let layout = store.get_layout(&session.id).unwrap();
        let two = layout.root.split_leaf(
            layout.root.id(), SplitAxis::Vertical, 0.5,
            PaneContent::Agent { agent_run_id: agent.id.clone() },
        ).unwrap();
        let saved = store.set_layout(&session.id, 0, two).unwrap();
        let pane_to_close = saved.root.leaves()[1].0.to_owned();
        let after = saved.root.close_leaf(&pane_to_close).unwrap();
        store.set_layout(&session.id, 1, after).unwrap();
        store.archive_session(&session.id).unwrap();
        let snapshot = store.snapshot().unwrap();
        assert!(snapshot.sessions[0].archived);
        assert_eq!(snapshot.agent_runs[0].id, agent.id);
        assert_eq!(snapshot.agent_runs[0].terminal_address, "t9");
    }

    #[test]
    fn optimistic_layout_revision_prevents_stale_writer() {
        let mut store = Store::open_memory().unwrap();
        let space = store.create_space("x", "/x").unwrap();
        let session = store.create_session(&space.id, "session").unwrap();
        let layout = store.get_layout(&session.id).unwrap();
        store.set_layout(&session.id, 0, layout.root.clone()).unwrap();
        assert!(matches!(
            store.set_layout(&session.id, 0, layout.root),
            Err(StoreError::Conflict)
        ));
    }

    #[test]
    fn forbids_link_to_agent_in_different_session() {
        let mut store = Store::open_memory().unwrap();
        let space = store.create_space("x", "/x").unwrap();
        let one = store.create_session(&space.id, "one").unwrap();
        let two = store.create_session(&space.id, "two").unwrap();
        let agent = store.bind_agent(&one.id, "hc-core-a", "t1", "codex", None).unwrap();
        let root = PaneNode::leaf(PaneContent::Agent { agent_run_id: agent.id });
        assert!(matches!(store.set_layout(&two.id, 0, root), Err(StoreError::Invalid(_))));
    }

    #[test]
    fn binds_parent_only_within_same_session() {
        let mut store = Store::open_memory().unwrap();
        let space = store.create_space("x", "/x").unwrap();
        let one = store.create_session(&space.id, "one").unwrap();
        let two = store.create_session(&space.id, "two").unwrap();
        let parent = store.bind_agent(&one.id, "hc-core-a", "t1", "codex", None).unwrap();
        assert!(matches!(
            store.bind_agent(&two.id, "hc-core-a", "t2", "claude", Some(&parent.id)),
            Err(StoreError::Invalid(_))
        ));
    }
}
