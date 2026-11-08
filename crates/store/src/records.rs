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

impl Store {
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
        let session: Option<String> = self
            .conn
            .query_row(
                "SELECT id FROM sessions WHERE id=?1",
                params![session_id],
                |row| row.get(0),
            )
            .optional()?;
        if session.is_none() {
            return Err(StoreError::NotFound(format!("session {session_id}")));
        }
        if let Some(parent) = parent_run_id {
            let parent_session: Option<String> = self
                .conn
                .query_row(
                    "SELECT session_id FROM agent_runs WHERE id=?1",
                    params![parent],
                    |row| row.get(0),
                )
                .optional()?;
            if parent_session.as_deref() != Some(&session_id) {
                return Err(StoreError::Invalid(
                    "parent AgentRun must belong to the same Session".into(),
                ));
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
