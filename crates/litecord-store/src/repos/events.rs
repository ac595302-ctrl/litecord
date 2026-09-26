//! The append-only unified event log.

use rusqlite::{params, Connection};

use litecord_core::events::UnifiedEvent;
use litecord_types::entity::EntityId;
use litecord_types::{Revision, Timestamp};

use crate::db::WriteTx;
use crate::error::StoreResult;

/// One row of the `events` table.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct EventRecord {
    pub seq: i64,
    pub revision: Revision,
    pub kind: String,
    pub entity: Option<String>,
    pub source: String,
    pub event: UnifiedEvent,
    pub at: Timestamp,
}

fn map_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<EventRecord> {
    let seq: i64 = row.get(0)?;
    let revision = crate::sql::rev(row.get(1)?);
    let kind: String = row.get(2)?;
    let entity: Option<String> = row.get(3)?;
    let source: String = row.get(4)?;
    let payload: String = row.get(5)?;
    let at = Timestamp::from_millis(row.get(6)?);
    let event: UnifiedEvent =
        serde_json::from_str(&payload).map_err(|e| crate::sql::col_err(5, e))?;
    Ok(EventRecord {
        seq,
        revision,
        kind,
        entity,
        source,
        event,
        at,
    })
}

const COLUMNS: &str = "seq, revision, kind, entity, source, payload, at";

/// Events with `revision > after`, oldest first.
pub fn since(conn: &Connection, after: Revision, limit: u32) -> StoreResult<Vec<EventRecord>> {
    let sql = format!(
        "SELECT {COLUMNS} FROM events WHERE revision > ?1 ORDER BY revision ASC, seq ASC LIMIT ?2"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![after.get() as i64, limit], map_row)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Content-free change summary: `(kind, count)` of events with
/// `revision > after`, excluding events whose origin is `exclude_source`
/// (e.g. `agent_derived`, so an agent's own writes don't wake it again).
pub fn kind_counts_since(
    conn: &Connection,
    after: Revision,
    exclude_source: &str,
) -> StoreResult<Vec<(String, u64)>> {
    let mut stmt = conn.prepare(
        "SELECT kind, COUNT(*) FROM events WHERE revision > ?1 AND source != ?2 \
         GROUP BY kind ORDER BY kind",
    )?;
    let rows = stmt.query_map(params![after.get() as i64, exclude_source], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?.max(0) as u64))
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Events about `entity`, newest first.
pub fn for_entity(
    conn: &Connection,
    entity: &EntityId,
    limit: u32,
) -> StoreResult<Vec<EventRecord>> {
    let sql = format!(
        "SELECT {COLUMNS} FROM events WHERE entity = ?1 ORDER BY revision DESC, seq DESC LIMIT ?2"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![entity.to_string(), limit], map_row)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Delete events older than `before`. Returns the number of rows removed.
pub fn prune_before(tx: &WriteTx<'_>, before: Timestamp) -> StoreResult<usize> {
    Ok(tx.execute(
        "DELETE FROM events WHERE at < ?1",
        params![before.as_millis()],
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use litecord_types::provenance::Origin;
    use litecord_types::UserId;

    #[test]
    fn since_and_for_entity() {
        let db = Database::open_in_memory().unwrap();
        db.write(|tx| {
            tx.emit(
                UnifiedEvent::UserUpdated { user_id: UserId(1) },
                Origin::Synthetic,
            )
        })
        .unwrap();
        let events = db.read(|r| since(r, Revision::ZERO, 10)).unwrap();
        assert_eq!(events.len(), 1);
        let by_entity = db
            .read(|r| for_entity(r, &EntityId::User(UserId(1)), 10))
            .unwrap();
        assert_eq!(by_entity.len(), 1);
    }
}
