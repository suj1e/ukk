//! Space, Session and AgentRun persistence; no terminal process actions.
use super::*;
use rusqlite::{params, OptionalExtension};
use std::time::{SystemTime, UNIX_EPOCH};
use ukk_domain::{new_id, AgentRun, Session, SessionLayout, Space};

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn nonempty(value: &str, field: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > 200 {
        return Err(StoreError::Invalid(format!(
            "{field} must contain 1–200 characters"
        )));
    }
    Ok(value.to_owned())
}

fn agent_run_row(conn: &Connection, id: &str) -> Result<Option<AgentRun>> {
    Ok(conn
        .query_row(
            "SELECT id,session_id,core_id,terminal_address,parent_run_id,adapter,created_ms FROM agent_runs WHERE id=?1",
            params![id],
            |row| {
                Ok(AgentRun {
                    id: row.get(0)?,
                    session_id: row.get(1)?,
                    core_id: row.get(2)?,
                    terminal_address: row.get(3)?,
                    parent_run_id: row.get(4)?,
                    adapter: row.get(5)?,
                    created_ms: row.get(6)?,
                })
            },
        )
        .optional()?)
}

impl Store {
    pub fn create_space(&mut self, title: &str, project_root: &str) -> Result<Space> {
        self.create_space_with_id(&new_id(), title, project_root)
    }

    /// Idempotent Space creation across retries and Host restarts. The client
    /// chooses the UUID once per deliberate action and REUSES it when retrying;
    /// repeating it with different inputs is rejected as a conflict rather
    /// than creating a second Space. This operation never starts a process.
    pub fn create_space_with_id(
        &mut self,
        id: &str,
        title: &str,
        project_root: &str,
    ) -> Result<Space> {
        let title = nonempty(title, "Space title")?;
        let project_root = nonempty(project_root, "project root")?;
        let id = uuid::Uuid::parse_str(id)
            .map_err(|_| StoreError::Invalid("request_id must be a UUID".into()))?
            .to_string();
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let previous = tx
            .query_row(
                "SELECT id,title,project_root,created_ms FROM spaces WHERE id=?1",
                params![id],
                |row| {
                    Ok(Space {
                        id: row.get(0)?,
                        title: row.get(1)?,
                        project_root: row.get(2)?,
                        created_ms: row.get(3)?,
                    })
                },
            )
            .optional()?;
        if let Some(existing) = previous {
            if existing.title == title && existing.project_root == project_root {
                return Ok(existing);
            }
            return Err(StoreError::Conflict);
        }
        let space = Space {
            id,
            title,
            project_root,
            created_ms: now_ms(),
        };
        tx.execute(
            "INSERT INTO spaces(id,title,project_root,created_ms) VALUES(?1,?2,?3,?4)",
            params![space.id, space.title, space.project_root, space.created_ms],
        )?;
        tx.commit()?;
        Ok(space)
    }

    /// Renames a Space. Only durable product metadata changes; Sessions,
    /// AgentRun bindings and herdr-core terminals keep their lifetimes.
    pub fn rename_space(&mut self, space_id: &str, title: &str) -> Result<Space> {
        let title = nonempty(title, "Space title")?;
        let space_id = nonempty(space_id, "Space ID")?;
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let updated = tx.execute(
            "UPDATE spaces SET title=?2 WHERE id=?1",
            params![space_id, title],
        )?;
        if updated == 0 {
            return Err(StoreError::NotFound(format!("space {space_id}")));
        }
        let space = tx.query_row(
            "SELECT id,title,project_root,created_ms FROM spaces WHERE id=?1",
            params![space_id],
            |row| {
                Ok(Space {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    project_root: row.get(2)?,
                    created_ms: row.get(3)?,
                })
            },
        )?;
        tx.commit()?;
        Ok(space)
    }

    pub fn create_session(&mut self, space_id: &str, title: &str) -> Result<Session> {
        self.create_session_with_id(space_id, title, &new_id())
    }

    /// Idempotent creation across retries and Host restarts. The client picks
    /// a fresh UUID per deliberate click and REUSES it when retrying.
    /// Repeating that UUID with different inputs is rejected (not silently
    /// interpreted as a new action). This operation never starts a terminal.
    pub fn create_session_with_id(
        &mut self,
        space_id: &str,
        title: &str,
        id: &str,
    ) -> Result<Session> {
        let title = nonempty(title, "Session title")?;
        let space_id = nonempty(space_id, "Space ID")?;
        let id = uuid::Uuid::parse_str(id)
            .map_err(|_| StoreError::Invalid("request_id must be a UUID".into()))?
            .to_string();
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let previous = tx
            .query_row(
                "SELECT id,space_id,title,archived,created_ms,updated_ms FROM sessions WHERE id=?1",
                params![id],
                |row| {
                    Ok(Session {
                        id: row.get(0)?,
                        space_id: row.get(1)?,
                        title: row.get(2)?,
                        archived: row.get::<_, i64>(3)? != 0,
                        created_ms: row.get(4)?,
                        updated_ms: row.get(5)?,
                    })
                },
            )
            .optional()?;
        if let Some(existing) = previous {
            if existing.space_id == space_id && existing.title == title {
                return Ok(existing);
            }
            return Err(StoreError::Conflict);
        }
        let exists: Option<String> = tx
            .query_row(
                "SELECT id FROM spaces WHERE id=?1",
                params![space_id],
                |row| row.get(0),
            )
            .optional()?;
        if exists.is_none() {
            return Err(StoreError::NotFound(format!("space {space_id}")));
        }
        let created = now_ms();
        let session = Session {
            id,
            space_id,
            title,
            archived: false,
            created_ms: created,
            updated_ms: created,
        };
        let layout = SessionLayout::new(session.id.clone());
        let encoded = serde_json::to_string(&layout.root)?;
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
    ///
    /// This is the legacy convenience path used by the CLI: it mints a fresh
    /// AgentRun id and therefore cannot deduplicate a retried request. Callers
    /// that need idempotency use [`Self::bind_agent_with_id`].
    pub fn bind_agent(
        &mut self,
        session_id: &str,
        core_id: &str,
        terminal_address: &str,
        adapter: &str,
        parent_run_id: Option<&str>,
    ) -> Result<AgentRun> {
        self.bind_agent_inner(
            None,
            session_id,
            core_id,
            terminal_address,
            adapter,
            parent_run_id,
        )
    }

    /// Idempotent explicit binding. `request_id` becomes the AgentRun id, so a
    /// retried request with identical inputs returns the existing row instead
    /// of creating a second AgentRun. Reusing the id with different inputs is
    /// a conflict. The same `(core_id, terminal_address)` may not be bound to
    /// two AgentRuns. This operation never starts, signals or attaches to the
    /// terminal process.
    pub fn bind_agent_with_id(
        &mut self,
        request_id: &str,
        session_id: &str,
        core_id: &str,
        terminal_address: &str,
        adapter: &str,
        parent_run_id: Option<&str>,
    ) -> Result<AgentRun> {
        let request_id = uuid::Uuid::parse_str(request_id.trim())
            .map_err(|_| StoreError::Invalid("request_id must be a UUID".into()))?
            .to_string();
        self.bind_agent_inner(
            Some(&request_id),
            session_id,
            core_id,
            terminal_address,
            adapter,
            parent_run_id,
        )
    }

    fn bind_agent_inner(
        &mut self,
        request_id: Option<&str>,
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
        let parent_run_id = parent_run_id.map(str::to_owned);
        let request_id = request_id.map(str::to_owned);
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

        if let Some(id) = request_id.as_deref() {
            if let Some(existing) = agent_run_row(&tx, id)? {
                if existing.session_id == session_id
                    && existing.core_id == core_id
                    && existing.terminal_address == terminal_address
                    && existing.parent_run_id == parent_run_id
                    && existing.adapter == adapter
                {
                    return Ok(existing);
                }
                return Err(StoreError::Conflict);
            }
        }

        let session: Option<String> = tx
            .query_row(
                "SELECT id FROM sessions WHERE id=?1",
                params![session_id],
                |row| row.get(0),
            )
            .optional()?;
        if session.is_none() {
            return Err(StoreError::NotFound(format!("session {session_id}")));
        }
        if let Some(parent) = parent_run_id.as_deref() {
            let parent_session: Option<String> = tx
                .query_row(
                    "SELECT session_id FROM agent_runs WHERE id=?1",
                    params![parent],
                    |row| row.get(0),
                )
                .optional()?;
            match parent_session {
                None => {
                    return Err(StoreError::NotFound(format!("parent AgentRun {parent}")));
                }
                Some(parent_session) if parent_session != session_id => {
                    return Err(StoreError::Invalid(
                        "parent AgentRun must belong to the same Session".into(),
                    ));
                }
                Some(_) => {}
            }
        }
        let already_bound: Option<String> = tx
            .query_row(
                "SELECT id FROM agent_runs WHERE core_id=?1 AND terminal_address=?2 LIMIT 1",
                params![core_id, terminal_address],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(existing) = already_bound {
            return Err(StoreError::AlreadyBound(format!(
                "terminal {terminal_address} on core {core_id} is already AgentRun {existing}"
            )));
        }

        let run = AgentRun {
            id: request_id.unwrap_or_else(new_id),
            session_id,
            core_id,
            terminal_address,
            parent_run_id,
            adapter,
            created_ms: now_ms(),
        };
        tx.execute(
            "INSERT INTO agent_runs(id,session_id,core_id,terminal_address,parent_run_id,adapter,created_ms) VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![run.id, run.session_id, run.core_id, run.terminal_address, run.parent_run_id, run.adapter, run.created_ms],
        )?;
        tx.commit()?;
        Ok(run)
    }

    /// Durable AgentRun lookup by its id. Used for idempotent bind retries;
    /// it is not an authorization check and never inspects herdr-core.
    pub fn agent_run_by_id(&self, id: &str) -> Result<Option<AgentRun>> {
        let id = nonempty(id, "AgentRun ID")?;
        agent_run_row(&self.conn, &id)
    }

    /// Looks up the AgentRun bound to a herdr-core `(core_id, terminal)`.
    ///
    /// Returns `(session_id, agent_run_id)`. An unbound terminal has no
    /// durable Agent identity and must not gain one implicitly. When the
    /// caller does not know the reporting core, only an unambiguous single
    /// binding is returned; two cores sharing a terminal address must not be
    /// attributed to the wrong AgentRun.
    pub fn agent_run_for_terminal(
        &self,
        core_id: Option<&str>,
        terminal_address: &str,
    ) -> Result<Option<(String, String)>> {
        if let Some(core_id) = core_id {
            return Ok(self
                .conn
                .query_row(
                    "SELECT session_id,id FROM agent_runs
                     WHERE core_id=?1 AND terminal_address=?2",
                    params![core_id, terminal_address],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?);
        }
        let mut stmt = self
            .conn
            .prepare("SELECT session_id,id FROM agent_runs WHERE terminal_address=?1 LIMIT 2")?;
        let rows = stmt.query_map(params![terminal_address], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut matches = Vec::new();
        for row in rows {
            matches.push(row?);
        }
        Ok(if matches.len() == 1 {
            matches.into_iter().next()
        } else {
            None
        })
    }

    /// True when a Space exists. Used to reject an unexecutable command before
    /// idempotency admission.
    pub fn space_exists(&self, space_id: &str) -> Result<bool> {
        Ok(self
            .conn
            .query_row(
                "SELECT 1 FROM spaces WHERE id=?1",
                params![space_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::fs;

    #[test]
    fn space_creation_is_idempotent_and_rejects_reused_id_with_new_inputs() {
        let mut store = Store::open_memory().unwrap();
        let id = new_id();
        let created = store.create_space_with_id(&id, "  ukk  ", "/work").unwrap();
        assert_eq!(created.id, id);
        assert_eq!(created.title, "ukk");
        let repeated = store.create_space_with_id(&id, "ukk", "/work").unwrap();
        assert_eq!(repeated, created);
        assert_eq!(store.snapshot().unwrap().spaces.len(), 1);
        assert_eq!(
            store
                .create_space_with_id(&id, "Other", "/work")
                .unwrap_err()
                .to_string(),
            StoreError::Conflict.to_string()
        );
        assert_eq!(store.snapshot().unwrap().spaces[0], created);
    }

    #[test]
    fn space_creation_and_rename_validate_bounds_and_identity() {
        let mut store = Store::open_memory().unwrap();
        for (title, root) in [("", "/work"), ("   ", "/work"), ("ukk", "")] {
            assert!(matches!(
                store.create_space(title, root),
                Err(StoreError::Invalid(_))
            ));
        }
        let oversized = "x".repeat(201);
        assert!(matches!(
            store.create_space(&oversized, "/work"),
            Err(StoreError::Invalid(_))
        ));
        let exact = "x".repeat(200);
        let created = store.create_space(&exact, "/work").unwrap();
        assert_eq!(created.title, exact);
        assert!(matches!(
            store.rename_space(&created.id, &oversized),
            Err(StoreError::Invalid(_))
        ));
        assert!(matches!(
            store.rename_space(&created.id, "  "),
            Err(StoreError::Invalid(_))
        ));
        assert!(matches!(
            store.create_space_with_id("not-a-uuid", "ukk", "/work"),
            Err(StoreError::Invalid(_))
        ));
        assert_eq!(store.snapshot().unwrap().spaces.len(), 1);
    }

    #[test]
    fn rename_space_rejects_missing_space_and_persists_across_reopen() {
        let dir = env::temp_dir().join(format!("ukk-store-space-{}", new_id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("workspace.db");
        let space_id = new_id();
        {
            let mut store = Store::open(&path).unwrap();
            store
                .create_space_with_id(&space_id, "Original", "/work")
                .unwrap();
            let renamed = store.rename_space(&space_id, " Renamed ").unwrap();
            assert_eq!(renamed.title, "Renamed");
            let again = store.rename_space(&space_id, "Renamed").unwrap();
            assert_eq!(again, renamed);
            assert!(matches!(
                store.rename_space(&new_id(), "Missing"),
                Err(StoreError::NotFound(_))
            ));
        }
        {
            let store = Store::open(&path).unwrap();
            let spaces = store.snapshot().unwrap().spaces;
            assert_eq!(spaces.len(), 1);
            assert_eq!(spaces[0].id, space_id);
            assert_eq!(spaces[0].title, "Renamed");
            assert_eq!(spaces[0].project_root, "/work");
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn space_mutations_never_create_sessions_or_agent_runs() {
        let mut store = Store::open_memory().unwrap();
        let space = store.create_space("ukk", "/work").unwrap();
        store.rename_space(&space.id, "ukk renamed").unwrap();
        let snapshot = store.snapshot().unwrap();
        assert_eq!(snapshot.spaces.len(), 1);
        assert!(snapshot.sessions.is_empty());
        assert!(snapshot.agent_runs.is_empty());
        assert!(snapshot.layouts.is_empty());
    }
}
