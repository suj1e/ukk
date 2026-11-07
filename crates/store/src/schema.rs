//! Database open, permissions and versioned migrations.
use super::*;
use std::{env, fs, path::{Path, PathBuf}, time::Duration};

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
}
