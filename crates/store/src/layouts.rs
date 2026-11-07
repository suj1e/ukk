//! Versioned Pane layout reads and compare-and-swap updates.
use super::*;
use rusqlite::{params, OptionalExtension};
use ukk_domain::{PaneContent, PaneNode, SessionLayout};

impl Store {
    pub fn get_layout(&self, session_id: &str) -> Result<SessionLayout> {
        let row: Option<(i64, String)> = self
            .conn
            .query_row(
                "SELECT revision,content_json FROM session_layouts WHERE session_id=?1",
                params![session_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let (revision, encoded) =
            row.ok_or_else(|| StoreError::NotFound(format!("layout for {session_id}")))?;
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
            if let PaneContent::Agent { agent_run_id } | PaneContent::Terminal { agent_run_id } =
                content
            {
                let matching_session: Option<String> = self
                    .conn
                    .query_row(
                        "SELECT session_id FROM agent_runs WHERE id=?1",
                        params![agent_run_id],
                        |row| row.get(0),
                    )
                    .optional()?;
                if matching_session.as_deref() != Some(session_id) {
                    return Err(StoreError::Invalid(
                        "pane's AgentRun must belong to this Session".into(),
                    ));
                }
            }
        }
        let encoded = serde_json::to_string(&root)?;
        let revision = expected_revision
            .checked_add(1)
            .ok_or(StoreError::Invalid("layout revision overflow".into()))?;
        let changed = self.conn.execute(
            "UPDATE session_layouts SET revision=?1,content_json=?2 WHERE session_id=?3 AND revision=?4",
            params![revision, encoded, session_id, expected_revision],
        )?;
        if changed == 0 {
            return match self.get_layout(session_id) {
                Err(StoreError::NotFound(_)) => {
                    Err(StoreError::NotFound(format!("layout for {session_id}")))
                }
                Ok(_) => Err(StoreError::Conflict),
                Err(error) => Err(error),
            };
        }
        Ok(SessionLayout {
            session_id: session_id.to_owned(),
            revision,
            root,
        })
    }
}
