//! Read-only materialization of durable Spaces, Sessions and pane layout.
use super::*;
use ukk_domain::{AgentRun, DomainSnapshot, PaneNode, Session, SessionLayout, Space};

impl Store {
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
