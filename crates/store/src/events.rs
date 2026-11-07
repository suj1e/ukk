//! Durable S1 event-log persistence.
//!
//! The store assigns cursors inside one SQLite transaction, so a Host restart
//! continues at `head + 1` instead of resetting. Event payloads are stored as
//! typed [`Event`] JSON and decoded through the protocol envelope on read; the
//! store never re-implements cursor or replay semantics.
//!
//! Retention is a floor, not a window that can move backwards. A cursor below
//! the floor requires a snapshot at the current head; a cursor beyond the head
//! is a typed error, never a reset.

use super::*;
use rusqlite::{params, OptionalExtension};
use serde_json::Value;
use ukk_protocol::{
    DurableCursor, Event, EventEnvelope, EventId, Resume, Snapshot, StreamBounds, StreamId,
    SyncError, CURRENT_GENERATION,
};

fn storage_generation() -> i64 {
    i64::from(CURRENT_GENERATION.raw())
}

fn check_generation(generation: i64) -> Result<()> {
    if generation != storage_generation() {
        return Err(StoreError::Protocol(SyncError::InvalidDurableLog(
            "stream generation does not match the current protocol",
        )));
    }
    Ok(())
}

fn cursor_from_i64(raw: i64) -> Result<DurableCursor> {
    let raw = u64::try_from(raw).map_err(|_| {
        StoreError::Protocol(SyncError::InvalidDurableLog(
            "durable cursor is outside the stored range",
        ))
    })?;
    Ok(DurableCursor::from_raw(raw))
}

fn cursor_to_i64(cursor: DurableCursor) -> Result<i64> {
    i64::try_from(cursor.get()).map_err(|_| {
        StoreError::Protocol(SyncError::InvalidDurableLog(
            "durable cursor exceeds the storage range",
        ))
    })
}

/// Creates the stream row on first use. The caller owns the transaction so
/// stream creation and the first append commit together.
fn ensure_stream_tx(tx: &rusqlite::Transaction<'_>, stream: &StreamId) -> Result<StreamBounds> {
    let row: Option<(i64, i64, i64)> = tx
        .query_row(
            "SELECT generation,floor,head FROM event_streams WHERE stream_id=?1",
            params![stream.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    if let Some((generation, floor, head)) = row {
        check_generation(generation)?;
        let floor = cursor_from_i64(floor)?;
        let head = cursor_from_i64(head)?;
        if floor > head {
            return Err(StoreError::Protocol(SyncError::InvalidDurableLog(
                "retention floor is beyond the head",
            )));
        }
        return Ok(StreamBounds {
            stream: stream.clone(),
            floor,
            head,
        });
    }
    let now = now_ms();
    tx.execute(
        "INSERT INTO event_streams(stream_id,generation,head,floor,updated_ms)
         VALUES(?1,?2,0,0,?3)",
        params![stream.as_str(), storage_generation(), now],
    )?;
    Ok(StreamBounds {
        stream: stream.clone(),
        floor: DurableCursor::ZERO,
        head: DurableCursor::ZERO,
    })
}

/// Applies the configured retained-event bound and advances the floor to the
/// last pruned cursor. This mirrors `ukk_protocol::EventLog` retention.
fn prune_retained_tx(
    tx: &rusqlite::Transaction<'_>,
    stream: &StreamId,
    limits: &ukk_protocol::Limits,
) -> Result<()> {
    let max = i64::try_from(limits.max_retained_events).unwrap_or(i64::MAX);
    let count: i64 = tx.query_row(
        "SELECT COUNT(*) FROM event_log WHERE stream_id=?1",
        params![stream.as_str()],
        |row| row.get(0),
    )?;
    if count <= max {
        return Ok(());
    }
    let excess = count - max;
    let cutoff: i64 = tx.query_row(
        "SELECT cursor FROM event_log WHERE stream_id=?1 ORDER BY cursor ASC LIMIT 1 OFFSET ?2",
        params![stream.as_str(), excess - 1],
        |row| row.get(0),
    )?;
    tx.execute(
        "DELETE FROM event_log WHERE stream_id=?1 AND cursor<=?2",
        params![stream.as_str(), cutoff],
    )?;
    tx.execute(
        "UPDATE event_streams SET floor=?2, updated_ms=?3 WHERE stream_id=?1",
        params![stream.as_str(), cutoff, now_ms()],
    )?;
    Ok(())
}

impl Store {
    /// Creates or reads a stream row and returns its durable bounds.
    pub fn ensure_event_stream(&mut self, stream: &StreamId) -> Result<StreamBounds> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let bounds = ensure_stream_tx(&tx, stream)?;
        tx.commit()?;
        Ok(bounds)
    }

    /// Returns `None` when the stream has never been created. An existing
    /// stream whose generation is not the current protocol is refused rather
    /// than treated as empty.
    pub fn event_stream_bounds(&self, stream: &StreamId) -> Result<Option<StreamBounds>> {
        let row: Option<(i64, i64, i64)> = self
            .conn
            .query_row(
                "SELECT generation,floor,head FROM event_streams WHERE stream_id=?1",
                params![stream.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let Some((generation, floor, head)) = row else {
            return Ok(None);
        };
        check_generation(generation)?;
        let floor = cursor_from_i64(floor)?;
        let head = cursor_from_i64(head)?;
        if floor > head {
            return Err(StoreError::Protocol(SyncError::InvalidDurableLog(
                "retention floor is beyond the head",
            )));
        }
        Ok(Some(StreamBounds {
            stream: stream.clone(),
            floor,
            head,
        }))
    }

    /// Appends one event and assigns the next durable cursor.
    ///
    /// The stream is created at the current protocol generation on first use.
    /// A different stored generation, a duplicate event id or an oversized
    /// payload is refused before the head moves.
    pub fn append_event(
        &mut self,
        stream: &StreamId,
        event_id: EventId,
        event: Event,
        emitted_at_ms: i64,
    ) -> Result<EventEnvelope> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let bounds = ensure_stream_tx(&tx, stream)?;
        let next = bounds.head.next().ok_or_else(|| {
            StoreError::Protocol(SyncError::InvalidDurableLog("cursor space exhausted"))
        })?;
        let next_raw = cursor_to_i64(next)?;
        let envelope = EventEnvelope {
            protocol: CURRENT_GENERATION,
            event_id: event_id.clone(),
            stream: stream.clone(),
            cursor: next,
            emitted_at_ms,
            event,
        };
        // Validate payload and total envelope size before touching the head.
        envelope.encode(&self.limits)?;
        let duplicate: Option<i64> = tx
            .query_row(
                "SELECT cursor FROM event_log WHERE stream_id=?1 AND event_id=?2",
                params![stream.as_str(), event_id.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        if duplicate.is_some() {
            return Err(StoreError::DuplicateEvent(event_id.to_string()));
        }
        let payload = serde_json::to_string(&envelope.event)?;
        tx.execute(
            "INSERT INTO event_log(stream_id,cursor,event_id,generation,emitted_at_ms,event_json)
             VALUES(?1,?2,?3,?4,?5,?6)",
            params![
                stream.as_str(),
                next_raw,
                event_id.as_str(),
                storage_generation(),
                emitted_at_ms,
                payload
            ],
        )?;
        tx.execute(
            "UPDATE event_streams SET head=?2, updated_ms=?3 WHERE stream_id=?1",
            params![stream.as_str(), next_raw, now_ms()],
        )?;
        prune_retained_tx(&tx, stream, &self.limits)?;
        tx.commit()?;
        Ok(envelope)
    }

    /// Reads ordered events after `after`, or requires a snapshot when the
    /// requested history was pruned. The returned value is the protocol's own
    /// [`Resume`] type so transport code cannot reinterpret the outcome.
    pub fn read_events(
        &self,
        stream: &StreamId,
        after: DurableCursor,
        max_batch: usize,
    ) -> Result<Resume> {
        self.limits.check_replay_batch(max_batch)?;
        let tx = self.conn.unchecked_transaction()?;
        let row: Option<(i64, i64, i64)> = tx
            .query_row(
                "SELECT generation,floor,head FROM event_streams WHERE stream_id=?1",
                params![stream.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let Some((generation, floor, head)) = row else {
            return Err(StoreError::NotFound(format!("event stream {stream}")));
        };
        check_generation(generation)?;
        let floor = cursor_from_i64(floor)?;
        let head = cursor_from_i64(head)?;
        if after > head {
            return Err(StoreError::Protocol(SyncError::CursorBeyondHead {
                cursor: after.get(),
                head: head.get(),
            }));
        }
        if after < floor {
            let bounds = StreamBounds {
                stream: stream.clone(),
                floor,
                head,
            };
            tx.commit()?;
            return Ok(Resume::SnapshotRequired {
                at_cursor: head,
                bounds,
            });
        }
        let batch = i64::try_from(max_batch).unwrap_or(i64::MAX);
        let events = {
            let mut stmt = tx.prepare(
                "SELECT cursor,event_id,generation,emitted_at_ms,event_json
                 FROM event_log
                 WHERE stream_id=?1 AND cursor>?2 AND cursor<=?3
                 ORDER BY cursor ASC LIMIT ?4",
            )?;
            let rows = stmt.query_map(
                params![
                    stream.as_str(),
                    cursor_to_i64(after)?,
                    cursor_to_i64(head)?,
                    batch
                ],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                },
            )?;
            let mut expected = after.next().ok_or_else(|| {
                StoreError::Protocol(SyncError::InvalidDurableLog("cursor space exhausted"))
            })?;
            let mut events = Vec::new();
            for row in rows {
                let (cursor_raw, event_id_raw, generation_raw, emitted_at_ms, event_json) = row?;
                let cursor = cursor_from_i64(cursor_raw)?;
                if cursor != expected {
                    return Err(StoreError::Protocol(SyncError::InvalidDurableLog(
                        "retained event cursors are not contiguous",
                    )));
                }
                if generation_raw != generation {
                    return Err(StoreError::Protocol(SyncError::InvalidDurableLog(
                        "event generation does not match its stream",
                    )));
                }
                let event_id = EventId::new(event_id_raw).map_err(|error| {
                    StoreError::Invalid(format!("stored event id is invalid: {error}"))
                })?;
                let event: Event = serde_json::from_str(&event_json)?;
                let envelope = EventEnvelope {
                    protocol: CURRENT_GENERATION,
                    event_id,
                    stream: stream.clone(),
                    cursor,
                    emitted_at_ms,
                    event,
                };
                // Re-check payload and envelope bounds on read, not only write.
                envelope.encode(&self.limits)?;
                events.push(envelope);
                expected = cursor.next().ok_or_else(|| {
                    StoreError::Protocol(SyncError::InvalidDurableLog("cursor space exhausted"))
                })?;
            }
            events
        };
        let has_more = events.last().is_some_and(|event| event.cursor < head);
        tx.commit()?;
        Ok(Resume::Events {
            events,
            head,
            has_more,
        })
    }

    /// Advances the retention floor through `through` and deletes covered
    /// events. Moving the floor backwards is refused: a client that already
    /// received a snapshot must not be told older history still exists.
    pub fn prune_events_through(
        &mut self,
        stream: &StreamId,
        through: DurableCursor,
    ) -> Result<StreamBounds> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let row: Option<(i64, i64, i64)> = tx
            .query_row(
                "SELECT generation,floor,head FROM event_streams WHERE stream_id=?1",
                params![stream.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let Some((generation, floor, head)) = row else {
            return Err(StoreError::NotFound(format!("event stream {stream}")));
        };
        check_generation(generation)?;
        let floor = cursor_from_i64(floor)?;
        let head = cursor_from_i64(head)?;
        if through < floor {
            return Err(StoreError::Protocol(SyncError::InvalidDurableLog(
                "retention floor would move backwards",
            )));
        }
        if through > head {
            return Err(StoreError::Protocol(SyncError::InvalidDurableLog(
                "retention floor is beyond the head",
            )));
        }
        tx.execute(
            "DELETE FROM event_log WHERE stream_id=?1 AND cursor<=?2",
            params![stream.as_str(), cursor_to_i64(through)?],
        )?;
        tx.execute(
            "UPDATE event_streams SET floor=?2, updated_ms=?3 WHERE stream_id=?1",
            params![stream.as_str(), cursor_to_i64(through)?, now_ms()],
        )?;
        tx.commit()?;
        Ok(StreamBounds {
            stream: stream.clone(),
            floor: through,
            head,
        })
    }

    /// Materializes the protocol snapshot at the current durable head. The
    /// body remains domain-owned; the store only binds it to a cursor and
    /// enforces the snapshot size bound.
    pub fn snapshot_at_head(
        &self,
        stream: &StreamId,
        revision: u64,
        body: Value,
    ) -> Result<Snapshot> {
        let bounds = self
            .event_stream_bounds(stream)?
            .ok_or_else(|| StoreError::NotFound(format!("event stream {stream}")))?;
        let snapshot = Snapshot::new(stream.clone(), bounds.head, revision, body);
        snapshot.encode(&self.limits)?;
        Ok(snapshot)
    }
}
