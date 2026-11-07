//! Database open, permissions and versioned migrations.
use super::*;
use std::{
    env, fs,
    path::{Path, PathBuf},
    time::Duration,
};
use ukk_protocol::Limits;

/// Current schema. v1 is workspace metadata, v2 adds the durable workspace
/// revision, v3 adds the S1 event log, device trust and idempotency tables.
const SCHEMA_VERSION: i64 = 3;

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
        let home =
            env::var_os("HOME").ok_or_else(|| StoreError::Invalid("HOME is not set".into()))?;
        Ok(PathBuf::from(home)
            .join(".ukk")
            .join("data")
            .join("workspace.sqlite3"))
    }
}

impl Store {
    pub fn open_default() -> Result<Self> {
        let path = default_database_path()?;
        #[cfg(unix)]
        if env::var_os("UKK_DB_PATH").is_none() {
            use std::os::unix::fs::PermissionsExt;
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
                fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
            }
        }
        Self::open(&path)
    }

    pub fn open(path: &Path) -> Result<Self> {
        Self::open_with_limits(path, Limits::default())
    }

    /// Opens a database with explicit protocol bounds. The same limits govern
    /// event payload size, retained history and idempotency capacity, so the
    /// durable adapter cannot be configured more loosely than the Host.
    pub fn open_with_limits(path: &Path, limits: Limits) -> Result<Self> {
        limits.validate().map_err(StoreError::Limit)?;
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
        let mut store = Self { conn, limits };
        store.migrate()?;
        Ok(store)
    }

    pub fn open_memory() -> Result<Self> {
        Self::open_memory_with_limits(Limits::default())
    }

    pub fn open_memory_with_limits(limits: Limits) -> Result<Self> {
        limits.validate().map_err(StoreError::Limit)?;
        let conn = Connection::open_in_memory()?;
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        let mut store = Self { conn, limits };
        store.migrate()?;
        Ok(store)
    }

    #[must_use]
    pub fn limits(&self) -> &Limits {
        &self.limits
    }

    fn migrate(&mut self) -> Result<()> {
        // Most opens are reads on an up-to-date database: no writer lock.
        let version: i64 = self
            .conn
            .pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version > SCHEMA_VERSION {
            return Err(StoreError::UnsupportedVersion(version));
        }
        if version == SCHEMA_VERSION {
            return Ok(());
        }
        // Upgrade under one IMMEDIATE transaction. A second process waiting
        // to migrate rechecks the version *after* acquiring the writer lock,
        // so it never tries to recreate tables already installed by another.
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let version: i64 = tx.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version > SCHEMA_VERSION {
            return Err(StoreError::UnsupportedVersion(version));
        }
        if version == SCHEMA_VERSION {
            tx.commit()?;
            return Ok(());
        }
        if version == 0 {
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
        }
        // v2: a durable, monotonic revision for Spaces, Sessions, AgentRuns
        // and persisted pane layouts. Triggers catch writes from *every*
        // connection, including an independent CLI process.
        if version < 2 {
            tx.execute_batch(
                "CREATE TABLE workspace_revision (
                    id INTEGER PRIMARY KEY CHECK(id=1),
                    revision INTEGER NOT NULL CHECK(revision >= 0),
                    db_epoch TEXT NOT NULL
                );
                INSERT INTO workspace_revision(id, revision, db_epoch)
                VALUES(1, 0, lower(hex(randomblob(16))));",
            )?;
            for table in ["spaces", "sessions", "agent_runs", "session_layouts"] {
                for (operation, suffix) in [
                    ("INSERT", "insert"),
                    ("UPDATE", "update"),
                    ("DELETE", "delete"),
                ] {
                    // All identifiers are static source literals; never
                    // interpolate untrusted SQL identifiers here.
                    tx.execute_batch(&format!(
                        "CREATE TRIGGER ukk_rev_{table}_{suffix}
                         AFTER {operation} ON {table}
                         BEGIN
                            UPDATE workspace_revision
                            SET revision = revision + 1 WHERE id=1;
                         END;"
                    ))?;
                }
            }
            tx.execute_batch("PRAGMA user_version = 2;")?;
        }
        // v3: durable S1 protocol state. These tables are deliberately
        // independent of the workspace revision triggers: protocol cursors,
        // device grants and idempotency records are not UI metadata.
        if version < 3 {
            tx.execute_batch(
                r#"
                CREATE TABLE event_streams (
                    stream_id TEXT PRIMARY KEY NOT NULL,
                    generation INTEGER NOT NULL CHECK (generation > 0),
                    head INTEGER NOT NULL CHECK (head >= 0),
                    floor INTEGER NOT NULL CHECK (floor >= 0 AND floor <= head),
                    updated_ms INTEGER NOT NULL
                );
                CREATE TABLE event_log (
                    stream_id TEXT NOT NULL REFERENCES event_streams(stream_id) ON DELETE CASCADE,
                    cursor INTEGER NOT NULL CHECK (cursor > 0),
                    event_id TEXT NOT NULL,
                    generation INTEGER NOT NULL CHECK (generation > 0),
                    emitted_at_ms INTEGER NOT NULL,
                    event_json TEXT NOT NULL,
                    PRIMARY KEY (stream_id, cursor),
                    UNIQUE (stream_id, event_id)
                );
                CREATE INDEX event_log_retained ON event_log(stream_id, cursor);
                CREATE TABLE devices (
                    device_id TEXT PRIMARY KEY NOT NULL,
                    key_fingerprint TEXT NOT NULL,
                    generation INTEGER NOT NULL CHECK (generation >= 1),
                    revoked INTEGER NOT NULL DEFAULT 0 CHECK (revoked IN (0, 1)),
                    created_ms INTEGER NOT NULL,
                    updated_ms INTEGER NOT NULL
                );
                CREATE TABLE device_grants (
                    device_id TEXT NOT NULL REFERENCES devices(device_id) ON DELETE CASCADE,
                    scope TEXT NOT NULL,
                    identity_generation INTEGER NOT NULL CHECK (identity_generation >= 1),
                    expires_at_ms INTEGER NOT NULL,
                    PRIMARY KEY (device_id, scope)
                );
                CREATE TABLE idempotency_records (
                    key TEXT PRIMARY KEY NOT NULL,
                    fingerprint TEXT NOT NULL,
                    plane TEXT NOT NULL CHECK (plane IN ('local_ipc', 'remote')),
                    principal_kind TEXT NOT NULL CHECK (principal_kind IN ('local_owner', 'device')),
                    principal_id TEXT NOT NULL,
                    state TEXT NOT NULL CHECK (state IN ('in_flight', 'completed', 'expired')),
                    result_json TEXT,
                    created_ms INTEGER NOT NULL,
                    completed_at_ms INTEGER,
                    expired_at_ms INTEGER,
                    CHECK (state <> 'completed' OR (result_json IS NOT NULL AND completed_at_ms IS NOT NULL)),
                    CHECK (state <> 'expired' OR expired_at_ms IS NOT NULL)
                );
                CREATE INDEX idempotency_by_state ON idempotency_records(state, completed_at_ms);
                PRAGMA user_version = 3;
                "#,
            )?;
        }
        tx.commit()?;
        Ok(())
    }
}
