//! The entity graph: directed, provenance-carrying edges between
//! [`EntityId`]s (V2 §11). An edge is keyed by `(from, relation, to, origin)`,
//! so the same relation can be asserted independently by different sources
//! (e.g. observed Discord membership vs. an agent's inference) without one
//! overwriting the other's confidence.

use rusqlite::types::Value;
use rusqlite::{params, Connection, Row};

use litecord_core::events::UnifiedEvent;
use litecord_types::entity::EntityId;
use litecord_types::memory::{Edge, RelationType};
use litecord_types::provenance::{Confidence, Origin};
use litecord_types::Timestamp;

use crate::db::WriteTx;
use crate::error::StoreResult;
use crate::sql::{col_err, rev};

/// Insert or update an edge. If it already exists with the same confidence
/// this is a no-op (returns `false`, no event, no revision churn); otherwise
/// the confidence and `updated_at` are refreshed and
/// [`UnifiedEvent::EdgeRecorded`] is emitted.
pub fn upsert(
    tx: &WriteTx<'_>,
    from: &EntityId,
    relation: RelationType,
    to: &EntityId,
    origin: Origin,
    confidence: Confidence,
) -> StoreResult<bool> {
    let now = tx.now();
    let revision = tx.revision().get() as i64;
    let n = tx.execute(
        "INSERT INTO memory_edges (from_entity, relation, to_entity, origin, confidence, updated_at, revision) \
         VALUES (?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(from_entity, relation, to_entity, origin) DO UPDATE SET \
            confidence = excluded.confidence, updated_at = excluded.updated_at, revision = excluded.revision \
         WHERE memory_edges.confidence IS NOT excluded.confidence",
        params![
            from.to_string(),
            relation.as_str(),
            to.to_string(),
            origin.as_str(),
            confidence.get() as f64,
            now.as_millis(),
            revision,
        ],
    )?;
    let changed = n > 0;
    if changed {
        tx.emit(
            UnifiedEvent::EdgeRecorded {
                from: *from,
                to: *to,
            },
            origin,
        )?;
    }
    Ok(changed)
}

/// Remove one edge. No event: edge removal is a correction, not a fact worth
/// surfacing to agents on its own.
pub fn remove(
    tx: &WriteTx<'_>,
    from: &EntityId,
    relation: RelationType,
    to: &EntityId,
    origin: Origin,
) -> StoreResult<bool> {
    let n = tx.execute(
        "DELETE FROM memory_edges WHERE from_entity = ? AND relation = ? AND to_entity = ? AND origin = ?",
        params![from.to_string(), relation.as_str(), to.to_string(), origin.as_str()],
    )?;
    Ok(n > 0)
}

fn row_to_edge(row: &Row<'_>) -> rusqlite::Result<Edge> {
    let from: String = row.get(0)?;
    let from = from.parse::<EntityId>().map_err(|e| col_err(0, e))?;
    let relation: String = row.get(1)?;
    let relation = RelationType::parse(&relation).map_err(|e| col_err(1, e))?;
    let to: String = row.get(2)?;
    let to = to.parse::<EntityId>().map_err(|e| col_err(2, e))?;
    let origin_s: String = row.get(3)?;
    let origin = crate::sql::origin(3, &origin_s)?;
    let confidence: f64 = row.get(4)?;
    let updated_at: i64 = row.get(5)?;
    let revision: i64 = row.get(6)?;
    Ok(Edge {
        from,
        relation,
        to,
        origin,
        confidence: Confidence::clamped(confidence as f32),
        updated_at: Timestamp::from_millis(updated_at),
        revision: rev(revision),
    })
}

const EDGE_COLUMNS: &str =
    "from_entity, relation, to_entity, origin, confidence, updated_at, revision";

/// Edges leaving `from`, optionally filtered by relation, highest confidence
/// first. `limit = 0` means unlimited.
pub fn outgoing(
    conn: &Connection,
    from: &EntityId,
    relation: Option<RelationType>,
    limit: u32,
) -> StoreResult<Vec<Edge>> {
    let mut sql = format!("SELECT {EDGE_COLUMNS} FROM memory_edges WHERE from_entity = ?");
    let mut params: Vec<Value> = vec![Value::Text(from.to_string())];
    if let Some(r) = relation {
        sql.push_str(" AND relation = ?");
        params.push(Value::Text(r.as_str().to_string()));
    }
    sql.push_str(" ORDER BY confidence DESC, updated_at DESC");
    if limit > 0 {
        sql.push_str(" LIMIT ?");
        params.push(Value::Integer(limit as i64));
    }
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(params), row_to_edge)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Edges arriving at `to`, optionally filtered by relation, highest
/// confidence first. `limit = 0` means unlimited.
pub fn incoming(
    conn: &Connection,
    to: &EntityId,
    relation: Option<RelationType>,
    limit: u32,
) -> StoreResult<Vec<Edge>> {
    let mut sql = format!("SELECT {EDGE_COLUMNS} FROM memory_edges WHERE to_entity = ?");
    let mut params: Vec<Value> = vec![Value::Text(to.to_string())];
    if let Some(r) = relation {
        sql.push_str(" AND relation = ?");
        params.push(Value::Text(r.as_str().to_string()));
    }
    sql.push_str(" ORDER BY confidence DESC, updated_at DESC");
    if limit > 0 {
        sql.push_str(" LIMIT ?");
        params.push(Value::Integer(limit as i64));
    }
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(params), row_to_edge)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Edges touching `entity` in either direction, highest confidence first.
/// `limit = 0` means unlimited.
pub fn neighbors(conn: &Connection, entity: &EntityId, limit: u32) -> StoreResult<Vec<Edge>> {
    let mut sql = format!(
        "SELECT {EDGE_COLUMNS} FROM memory_edges WHERE from_entity = ? OR to_entity = ? \
         ORDER BY confidence DESC, updated_at DESC"
    );
    let key = entity.to_string();
    let mut params: Vec<Value> = vec![Value::Text(key.clone()), Value::Text(key)];
    if limit > 0 {
        sql.push_str(" LIMIT ?");
        params.push(Value::Integer(limit as i64));
    }
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(params), row_to_edge)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::db::Database;
    use litecord_types::ids::UserId;

    #[test]
    fn upsert_is_idempotent_and_neighbors_both_directions() {
        let db = Database::open_in_memory().unwrap();
        let a = EntityId::User(UserId(1));
        let b = EntityId::User(UserId(2));
        let c1 = db
            .write(|tx| {
                upsert(
                    tx,
                    &a,
                    RelationType::FriendOf,
                    &b,
                    Origin::AgentDerived,
                    Confidence::new(0.5).unwrap(),
                )
            })
            .unwrap();
        assert!(c1.changed());
        assert!(c1.value);

        let c2 = db
            .write(|tx| {
                upsert(
                    tx,
                    &a,
                    RelationType::FriendOf,
                    &b,
                    Origin::AgentDerived,
                    Confidence::new(0.5).unwrap(),
                )
            })
            .unwrap();
        assert!(!c2.changed());
        assert!(!c2.value);

        let from_a = db.read(|r| outgoing(r, &a, None, 0)).unwrap();
        assert_eq!(from_a.len(), 1);
        let neigh_a = db.read(|r| neighbors(r, &a, 0)).unwrap();
        let neigh_b = db.read(|r| neighbors(r, &b, 0)).unwrap();
        assert_eq!(neigh_a.len(), 1);
        assert_eq!(neigh_b.len(), 1);
        assert_eq!(neigh_a[0].to, b);
        assert_eq!(neigh_b[0].from, a);
    }
}
