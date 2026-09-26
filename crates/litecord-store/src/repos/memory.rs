//! Unified memory: derived and operational knowledge, kept separate from
//! canonical Discord state (V2 layers 3-4).
//!
//! Invariants enforced here (not just documented):
//!
//! * A memory can never be *inserted* as [`MemoryStatus::Superseded`]; that
//!   status is only reachable through [`supersede`], which also stamps
//!   `superseded_by` in the same statement so the two are never out of sync
//!   (the schema's `CHECK` constraint backs this up at the SQLite level).
//! * [`MemoryStatus::UserConfirmed`] may only be the *initial* status of an
//!   item whose [`Origin`] is [`Origin::UserProvided`]; an agent cannot mint a
//!   pre-confirmed memory for itself. Moving an existing item to
//!   `UserConfirmed` after the fact is [`confirm`]'s job, and it never
//!   rewrites `origin` — provenance is permanent, confirmation is a status
//!   change layered on top of it (V2 §9).
//! * Supersession is explicit and directional: the old item is marked
//!   `superseded` and points at its replacement; nothing is overwritten in
//!   place, so [`history`] can always reconstruct the full chain.
//! * Content is never empty; empty strings carry no information and would
//!   poison full-text search.

use std::collections::HashSet;

use rusqlite::types::Value;
use rusqlite::{params, Connection, OptionalExtension, Row};

use litecord_core::events::UnifiedEvent;
use litecord_types::entity::EntityId;
use litecord_types::memory::{
    MemoryFingerprint, MemoryItem, MemoryKind, MemoryPayload, MemoryStatus, NewMemory,
};
use litecord_types::provenance::{Confidence, Origin, SourceRef};
use litecord_types::{MemoryId, Timestamp};

use crate::db::WriteTx;
use crate::error::{StoreError, StoreResult};
use crate::repos::fts::{self, FtsMode};
use crate::sql::{col_err, push_in_str, rev, ts};

const DEFAULT_LIST_LIMIT: u32 = 50;

const SELECT_COLUMNS: &str = "id, kind, content, payload_json, origin, status, confidence, \
     created_at, observed_at, expires_at, revision, superseded_by, fingerprint, importance, \
     pinned, last_retrieved_at, retrieval_count";

/// Insert a new memory item.
///
/// Rejects `status = Superseded` (use [`supersede`]), `status =
/// UserConfirmed` for a non-[`Origin::UserProvided`] origin, and empty
/// content. Emits [`UnifiedEvent::MemoryRecorded`] with `m.origin`.
pub fn insert(tx: &WriteTx<'_>, m: &NewMemory) -> StoreResult<MemoryId> {
    if m.status == MemoryStatus::Superseded {
        return Err(StoreError::Invariant(
            "a memory cannot be inserted as Superseded; use supersede()".into(),
        ));
    }
    if m.status == MemoryStatus::UserConfirmed && m.origin != Origin::UserProvided {
        return Err(StoreError::Invariant(
            "status UserConfirmed is only allowed for Origin::UserProvided".into(),
        ));
    }
    if m.content.trim().is_empty() {
        return Err(StoreError::Invariant(
            "memory content must not be empty".into(),
        ));
    }

    let payload_json = m.payload.as_ref().map(serde_json::to_string).transpose()?;
    let fingerprint = m.fingerprint.as_ref().map(|f| f.0.clone());
    let now = tx.now();
    let revision = tx.revision().get() as i64;

    tx.execute(
        "INSERT INTO memory_items \
            (kind, content, payload_json, origin, status, confidence, created_at, \
             observed_at, expires_at, revision, superseded_by, fingerprint, importance, \
             pinned, last_retrieved_at, retrieval_count) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, NULL, ?, ?, ?, NULL, 0)",
        params![
            m.kind.as_str(),
            m.content,
            payload_json,
            m.origin.as_str(),
            m.status.as_str(),
            m.confidence.get() as f64,
            now.as_millis(),
            m.observed_at.map(Timestamp::as_millis),
            m.expires_at.map(Timestamp::as_millis),
            revision,
            fingerprint,
            m.importance as f64,
            m.pinned as i64,
        ],
    )?;
    let id = MemoryId(tx.last_insert_rowid());

    for source in &m.source_refs {
        tx.execute(
            "INSERT OR IGNORE INTO memory_sources (memory_id, entity, note) VALUES (?, ?, ?)",
            params![id.get(), source.entity.to_string(), source.note],
        )?;
    }
    for entity in &m.entities {
        tx.execute(
            "INSERT OR IGNORE INTO memory_entities (memory_id, entity) VALUES (?, ?)",
            params![id.get(), entity.to_string()],
        )?;
    }

    tx.emit(UnifiedEvent::MemoryRecorded { memory_id: id }, m.origin)?;
    Ok(id)
}

fn row_to_item(row: &Row<'_>) -> rusqlite::Result<MemoryItem> {
    let id = MemoryId(row.get(0)?);
    let kind: String = row.get(1)?;
    let kind = MemoryKind::parse(&kind).map_err(|e| col_err(1, e))?;
    let content: String = row.get(2)?;
    let payload_json: Option<String> = row.get(3)?;
    let payload = payload_json
        .map(|s| serde_json::from_str::<MemoryPayload>(&s))
        .transpose()
        .map_err(|e| col_err(3, e))?;
    let origin_s: String = row.get(4)?;
    let origin = crate::sql::origin(4, &origin_s)?;
    let status: String = row.get(5)?;
    let status = MemoryStatus::parse(&status).map_err(|e| col_err(5, e))?;
    let confidence: f64 = row.get(6)?;
    let created_at: i64 = row.get(7)?;
    let observed_at: Option<i64> = row.get(8)?;
    let expires_at: Option<i64> = row.get(9)?;
    let revision: i64 = row.get(10)?;
    let superseded_by: Option<i64> = row.get(11)?;
    let fingerprint: Option<String> = row.get(12)?;
    let importance: f64 = row.get(13)?;
    let pinned: i64 = row.get(14)?;
    let last_retrieved_at: Option<i64> = row.get(15)?;
    let retrieval_count: i64 = row.get(16)?;

    Ok(MemoryItem {
        id,
        kind,
        content: content.into(),
        payload,
        origin,
        status,
        confidence: Confidence::clamped(confidence as f32),
        source_refs: Vec::new(),
        entities: Vec::new(),
        created_at: Timestamp::from_millis(created_at),
        observed_at: ts(observed_at),
        expires_at: ts(expires_at),
        revision: rev(revision),
        superseded_by: superseded_by.map(MemoryId),
        fingerprint: fingerprint.map(MemoryFingerprint),
        importance: importance as f32,
        pinned: pinned != 0,
        last_retrieved_at: ts(last_retrieved_at),
        retrieval_count: retrieval_count.max(0) as u32,
    })
}

fn load_source_refs(conn: &Connection, id: MemoryId) -> StoreResult<Vec<SourceRef>> {
    let mut stmt = conn
        .prepare("SELECT entity, note FROM memory_sources WHERE memory_id = ? ORDER BY entity")?;
    let rows = stmt.query_map(params![id.get()], |row| {
        let entity: String = row.get(0)?;
        let entity: EntityId = entity.parse().map_err(|e| col_err(0, e))?;
        let note: Option<String> = row.get(1)?;
        Ok(SourceRef { entity, note })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

fn load_entities(conn: &Connection, id: MemoryId) -> StoreResult<Vec<EntityId>> {
    let mut stmt =
        conn.prepare("SELECT entity FROM memory_entities WHERE memory_id = ? ORDER BY entity")?;
    let rows = stmt.query_map(params![id.get()], |row| {
        let entity: String = row.get(0)?;
        entity.parse::<EntityId>().map_err(|e| col_err(0, e))
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Fill in `source_refs` and `entities` for a batch of items (N+1 lookups;
/// fine at this crate's scale and keeps the row mapper free of nested
/// queries).
fn hydrate(conn: &Connection, mut items: Vec<MemoryItem>) -> StoreResult<Vec<MemoryItem>> {
    for item in &mut items {
        item.source_refs = load_source_refs(conn, item.id)?;
        item.entities = load_entities(conn, item.id)?;
    }
    Ok(items)
}

/// Load one memory item by id, including its source references and entity
/// links.
pub fn get(conn: &Connection, id: MemoryId) -> StoreResult<Option<MemoryItem>> {
    let sql = format!("SELECT {SELECT_COLUMNS} FROM memory_items WHERE id = ?");
    let base = conn
        .query_row(&sql, params![id.get()], row_to_item)
        .optional()?;
    match base {
        Some(item) => Ok(Some(hydrate(conn, vec![item])?.remove(0))),
        None => Ok(None),
    }
}

/// The newest active (candidate/derived/user_confirmed) memory with the given
/// fingerprint, if any (V2 §43 deduplication lookup).
pub fn find_active_by_fingerprint(
    conn: &Connection,
    fp: &MemoryFingerprint,
) -> StoreResult<Option<MemoryItem>> {
    let sql = format!(
        "SELECT {SELECT_COLUMNS} FROM memory_items \
         WHERE fingerprint = ? AND status IN ('candidate','derived','user_confirmed') \
         ORDER BY created_at DESC, id DESC LIMIT 1"
    );
    let base = conn
        .query_row(&sql, params![fp.0], row_to_item)
        .optional()?;
    match base {
        Some(item) => Ok(Some(hydrate(conn, vec![item])?.remove(0))),
        None => Ok(None),
    }
}

/// Change a memory's status. Rejects `Superseded` (use [`supersede`]) and
/// `UserConfirmed` (use [`confirm`]). Emits [`UnifiedEvent::MemoryUpdated`]
/// with the memory's own (unchanged) origin when the status actually
/// changes.
pub fn set_status(tx: &WriteTx<'_>, id: MemoryId, status: MemoryStatus) -> StoreResult<bool> {
    if status == MemoryStatus::Superseded {
        return Err(StoreError::Invariant(
            "use supersede() to move a memory to Superseded".into(),
        ));
    }
    if status == MemoryStatus::UserConfirmed {
        return Err(StoreError::Invariant(
            "use confirm() to move a memory to UserConfirmed".into(),
        ));
    }
    let Some(existing) = get(tx, id)? else {
        return Ok(false);
    };
    let n = tx.execute(
        "UPDATE memory_items SET status = ?, revision = ? WHERE id = ? AND status IS NOT ?",
        params![
            status.as_str(),
            tx.revision().get() as i64,
            id.get(),
            status.as_str()
        ],
    )?;
    let changed = n > 0;
    if changed {
        tx.emit(
            UnifiedEvent::MemoryUpdated { memory_id: id },
            existing.origin,
        )?;
    }
    Ok(changed)
}

/// Explicitly confirm a memory (V2 §9): status becomes `UserConfirmed`.
/// `origin` is never touched — provenance survives confirmation. Rejects
/// confirming an item that has already been superseded (its replacement is
/// the current truth).
pub fn confirm(tx: &WriteTx<'_>, id: MemoryId) -> StoreResult<bool> {
    let Some(existing) = get(tx, id)? else {
        return Ok(false);
    };
    if existing.status == MemoryStatus::Superseded {
        return Err(StoreError::Invariant(
            "cannot confirm a superseded memory; confirm its replacement instead".into(),
        ));
    }
    let n = tx.execute(
        "UPDATE memory_items SET status = 'user_confirmed', revision = ? \
         WHERE id = ? AND status IS NOT 'user_confirmed'",
        params![tx.revision().get() as i64, id.get()],
    )?;
    let changed = n > 0;
    if changed {
        tx.emit(
            UnifiedEvent::MemoryUpdated { memory_id: id },
            Origin::UserProvided,
        )?;
    }
    Ok(changed)
}

/// Replace `old` with `new`: `old` moves to `Superseded` and points at `new`.
/// Both items must exist and be active; `old != new`; and `new` must not
/// already be (transitively) superseded by `old`, which would close a cycle.
pub fn supersede(tx: &WriteTx<'_>, old: MemoryId, new: MemoryId) -> StoreResult<()> {
    if old == new {
        return Err(StoreError::Invariant(
            "cannot supersede a memory with itself".into(),
        ));
    }
    let old_item = get(tx, old)?.ok_or_else(|| StoreError::NotFound(format!("memory {old:?}")))?;
    let new_item = get(tx, new)?.ok_or_else(|| StoreError::NotFound(format!("memory {new:?}")))?;
    if !old_item.status.is_active() {
        return Err(StoreError::Invariant(
            "the superseded memory must currently be active".into(),
        ));
    }
    if !new_item.status.is_active() {
        return Err(StoreError::Invariant(
            "the replacement memory must be active".into(),
        ));
    }

    // Cycle guard: walk `new`'s own supersession chain forward; if it ever
    // reaches `old`, linking old -> new would close a loop.
    let mut visited = HashSet::new();
    visited.insert(new);
    let mut cursor = new_item.superseded_by;
    while let Some(next) = cursor {
        if next == old {
            return Err(StoreError::Invariant(
                "supersession would create a cycle".into(),
            ));
        }
        if !visited.insert(next) {
            break; // already-broken cycle elsewhere; don't loop forever here
        }
        cursor = get(tx, next)?.and_then(|i| i.superseded_by);
    }

    let revision = tx.revision().get() as i64;
    tx.execute(
        "UPDATE memory_items SET status = 'superseded', superseded_by = ?, revision = ? WHERE id = ?",
        params![new.get(), revision, old.get()],
    )?;
    tx.emit(UnifiedEvent::MemorySuperseded { old, new }, new_item.origin)?;
    Ok(())
}

/// Add sources and raise confidence to `max(existing, given)`. Duplicate
/// sources are ignored. Emits [`UnifiedEvent::MemoryUpdated`] when anything
/// actually changed.
pub fn reinforce(
    tx: &WriteTx<'_>,
    id: MemoryId,
    extra_sources: &[SourceRef],
    confidence: Confidence,
) -> StoreResult<bool> {
    let Some(existing) = get(tx, id)? else {
        return Ok(false);
    };
    let mut changed = false;
    for source in extra_sources {
        let n = tx.execute(
            "INSERT OR IGNORE INTO memory_sources (memory_id, entity, note) VALUES (?, ?, ?)",
            params![id.get(), source.entity.to_string(), source.note],
        )?;
        if n > 0 {
            changed = true;
        }
    }
    let new_confidence = confidence.get().max(existing.confidence.get());
    if new_confidence != existing.confidence.get() {
        tx.execute(
            "UPDATE memory_items SET confidence = ?, revision = ? WHERE id = ?",
            params![new_confidence as f64, tx.revision().get() as i64, id.get()],
        )?;
        changed = true;
    }
    if changed {
        tx.emit(
            UnifiedEvent::MemoryUpdated { memory_id: id },
            existing.origin,
        )?;
    }
    Ok(changed)
}

/// Pin or unpin a memory (pinned items are exempt from [`expire_due`]).
pub fn set_pinned(tx: &WriteTx<'_>, id: MemoryId, pinned: bool) -> StoreResult<bool> {
    let Some(existing) = get(tx, id)? else {
        return Ok(false);
    };
    let n = tx.execute(
        "UPDATE memory_items SET pinned = ?, revision = ? WHERE id = ? AND pinned IS NOT ?",
        params![
            pinned as i64,
            tx.revision().get() as i64,
            id.get(),
            pinned as i64
        ],
    )?;
    let changed = n > 0;
    if changed {
        tx.emit(
            UnifiedEvent::MemoryUpdated { memory_id: id },
            existing.origin,
        )?;
    }
    Ok(changed)
}

/// Filter for [`list`]. `limit` of `0` means the default (50).
#[derive(Debug, Clone, Default)]
pub struct MemoryFilter {
    pub statuses: Option<Vec<MemoryStatus>>,
    pub kinds: Option<Vec<MemoryKind>>,
    pub origins: Option<Vec<Origin>>,
    pub entity: Option<EntityId>,
    pub since: Option<Timestamp>,
    pub limit: u32,
}

/// List memories matching `filter`, newest first.
pub fn list(conn: &Connection, filter: &MemoryFilter) -> StoreResult<Vec<MemoryItem>> {
    let mut sql = format!(
        "SELECT DISTINCT {} FROM memory_items m",
        qualify_columns(SELECT_COLUMNS, "m")
    );
    let mut conditions = Vec::new();
    let mut params: Vec<Value> = Vec::new();

    if let Some(entity) = &filter.entity {
        sql.push_str(" JOIN memory_entities me ON me.memory_id = m.id");
        conditions.push("me.entity = ?".to_string());
        params.push(Value::Text(entity.to_string()));
    }
    if let Some(statuses) = &filter.statuses {
        push_in_str(
            &mut conditions,
            &mut params,
            "m.status",
            statuses.iter().map(|s| s.as_str()),
        );
    }
    if let Some(kinds) = &filter.kinds {
        push_in_str(
            &mut conditions,
            &mut params,
            "m.kind",
            kinds.iter().map(|k| k.as_str()),
        );
    }
    if let Some(origins) = &filter.origins {
        push_in_str(
            &mut conditions,
            &mut params,
            "m.origin",
            origins.iter().map(|o| o.as_str()),
        );
    }
    if let Some(since) = filter.since {
        conditions.push("m.created_at >= ?".to_string());
        params.push(Value::Integer(since.as_millis()));
    }
    if !conditions.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&conditions.join(" AND "));
    }
    sql.push_str(" ORDER BY m.created_at DESC, m.id DESC LIMIT ?");
    let limit = if filter.limit == 0 {
        DEFAULT_LIST_LIMIT
    } else {
        filter.limit
    };
    params.push(Value::Integer(limit as i64));

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(params), row_to_item)?;
    let items = rows.collect::<Result<Vec<_>, _>>()?;
    hydrate(conn, items)
}

fn qualify_columns(columns: &str, alias: &str) -> String {
    columns
        .split(", ")
        .map(|c| format!("{alias}.{c}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The whole supersession chain containing `id`, oldest first. Walks
/// predecessors (`superseded_by = x`) to find the chain's root, then walks
/// successors (`x.superseded_by`) forward, guarding against loops with a
/// visited set.
pub fn history(conn: &Connection, id: MemoryId) -> StoreResult<Vec<MemoryItem>> {
    let mut visited = HashSet::new();
    let mut head = id;
    visited.insert(head);
    loop {
        let pred: Option<i64> = conn
            .query_row(
                "SELECT id FROM memory_items WHERE superseded_by = ? ORDER BY id LIMIT 1",
                params![head.get()],
                |r| r.get(0),
            )
            .optional()?;
        match pred {
            Some(p) => {
                let p = MemoryId(p);
                if !visited.insert(p) {
                    break;
                }
                head = p;
            }
            None => break,
        }
    }

    let mut chain = Vec::new();
    let mut visited = HashSet::new();
    let mut cursor = Some(head);
    while let Some(current) = cursor {
        if !visited.insert(current) {
            break;
        }
        match get(conn, current)? {
            Some(item) => {
                cursor = item.superseded_by;
                chain.push(item);
            }
            None => break,
        }
    }
    Ok(chain)
}

/// Expire active, unpinned items whose `expires_at` is at or before `now`.
/// Returns the ids that were expired. Emits [`UnifiedEvent::MemoryUpdated`]
/// for each.
pub fn expire_due(tx: &WriteTx<'_>, now: Timestamp) -> StoreResult<Vec<MemoryId>> {
    let due: Vec<(i64, String)> = {
        let mut stmt = tx.prepare(
            "SELECT id, origin FROM memory_items \
             WHERE status IN ('candidate','derived','user_confirmed') \
               AND expires_at IS NOT NULL AND expires_at <= ? AND pinned = 0",
        )?;
        let rows = stmt.query_map(params![now.as_millis()], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect::<Result<Vec<_>, _>>()?
    };

    let mut expired = Vec::new();
    for (raw_id, origin_s) in due {
        let id = MemoryId(raw_id);
        let origin = Origin::parse(&origin_s)
            .map_err(|e| StoreError::corrupt("memory_items", e.to_string()))?;
        let n = tx.execute(
            "UPDATE memory_items SET status = 'expired', revision = ? WHERE id = ?",
            params![tx.revision().get() as i64, id.get()],
        )?;
        if n > 0 {
            tx.emit(UnifiedEvent::MemoryUpdated { memory_id: id }, origin)?;
            expired.push(id);
        }
    }
    Ok(expired)
}

/// Bump `retrieval_count` and `last_retrieved_at` for the given ids. No
/// events (this is telemetry, not a state change agents should react to).
pub fn record_retrieval(tx: &WriteTx<'_>, ids: &[MemoryId], at: Timestamp) -> StoreResult<usize> {
    let mut n = 0usize;
    for id in ids {
        n += tx.execute(
            "UPDATE memory_items SET retrieval_count = retrieval_count + 1, last_retrieved_at = ? \
             WHERE id = ?",
            params![at.as_millis(), id.get()],
        )?;
    }
    Ok(n)
}

/// Full-text search over memory content via `memory_fts`.
#[derive(Debug, Clone)]
pub struct MemorySearch<'a> {
    pub query: &'a str,
    pub statuses: Option<&'a [MemoryStatus]>,
    pub kinds: Option<&'a [MemoryKind]>,
    pub entity: Option<EntityId>,
    pub since: Option<Timestamp>,
    pub mode: FtsMode,
    pub limit: u32,
}

/// Search memory content, best match first (lowest `bm25`).
pub fn search(conn: &Connection, s: &MemorySearch<'_>) -> StoreResult<Vec<(MemoryItem, f64)>> {
    let Some(expr) = fts::match_expr(s.query, s.mode, false) else {
        return Ok(Vec::new());
    };

    let mut sql = format!(
        "SELECT {}, bm25(memory_fts) FROM memory_fts JOIN memory_items m ON m.id = memory_fts.rowid",
        qualify_columns(SELECT_COLUMNS, "m")
    );
    let mut conditions = vec!["memory_fts MATCH ?".to_string()];
    let mut params: Vec<Value> = vec![Value::Text(expr)];

    if let Some(entity) = &s.entity {
        sql.push_str(" JOIN memory_entities me ON me.memory_id = m.id");
        conditions.push("me.entity = ?".to_string());
        params.push(Value::Text(entity.to_string()));
    }
    if let Some(statuses) = s.statuses {
        push_in_str(
            &mut conditions,
            &mut params,
            "m.status",
            statuses.iter().map(|st| st.as_str()),
        );
    }
    if let Some(kinds) = s.kinds {
        push_in_str(
            &mut conditions,
            &mut params,
            "m.kind",
            kinds.iter().map(|k| k.as_str()),
        );
    }
    if let Some(since) = s.since {
        conditions.push("m.created_at >= ?".to_string());
        params.push(Value::Integer(since.as_millis()));
    }
    sql.push_str(" WHERE ");
    sql.push_str(&conditions.join(" AND "));
    sql.push_str(" ORDER BY bm25(memory_fts) ASC LIMIT ?");
    let limit = if s.limit == 0 {
        DEFAULT_LIST_LIMIT
    } else {
        s.limit
    };
    params.push(Value::Integer(limit as i64));

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(params), |row| {
        let item = row_to_item(row)?;
        let bm25: f64 = row.get(17)?;
        Ok((item, bm25))
    })?;
    let pairs = rows.collect::<Result<Vec<_>, _>>()?;
    let (items, scores): (Vec<_>, Vec<_>) = pairs.into_iter().unzip();
    let items = hydrate(conn, items)?;
    Ok(items.into_iter().zip(scores).collect())
}

/// Count active/inactive memories grouped by status.
pub fn count_by_status(conn: &Connection) -> StoreResult<Vec<(MemoryStatus, i64)>> {
    let mut stmt = conn.prepare("SELECT status, COUNT(*) FROM memory_items GROUP BY status")?;
    let rows = stmt.query_map([], |row| {
        let s: String = row.get(0)?;
        let status = MemoryStatus::parse(&s).map_err(|e| col_err(0, e))?;
        let n: i64 = row.get(1)?;
        Ok((status, n))
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::db::Database;
    use litecord_types::ids::UserId;
    use litecord_types::memory::MemoryStatus;

    fn new_agent_memory(content: &str) -> NewMemory {
        NewMemory::new(MemoryKind::Fact, content, Origin::AgentDerived)
    }

    #[test]
    fn agent_derived_cannot_be_inserted_user_confirmed() {
        let db = Database::open_in_memory().unwrap();
        let mut m = new_agent_memory("test");
        m.status = MemoryStatus::UserConfirmed;
        let err = db.write(|tx| insert(tx, &m)).unwrap_err();
        assert!(matches!(err, StoreError::Invariant(_)));
    }

    #[test]
    fn confirm_keeps_origin_agent_derived() {
        let db = Database::open_in_memory().unwrap();
        let id = db
            .write(|tx| insert(tx, &new_agent_memory("candidate fact")))
            .unwrap()
            .value;
        let changed = db.write(|tx| confirm(tx, id)).unwrap().value;
        assert!(changed);
        let item = db.read(|r| get(r, id)).unwrap().unwrap();
        assert_eq!(item.status, MemoryStatus::UserConfirmed);
        assert_eq!(item.origin, Origin::AgentDerived);
    }

    #[test]
    fn empty_content_rejected() {
        let db = Database::open_in_memory().unwrap();
        let m = new_agent_memory("   ");
        let err = db.write(|tx| insert(tx, &m)).unwrap_err();
        assert!(matches!(err, StoreError::Invariant(_)));
    }

    #[test]
    fn supersession_chain_and_history() {
        let db = Database::open_in_memory().unwrap();
        let old = db
            .write(|tx| insert(tx, &new_agent_memory("Meeting Friday")))
            .unwrap()
            .value;
        let new = db
            .write(|tx| insert(tx, &new_agent_memory("Meeting Saturday")))
            .unwrap()
            .value;
        db.write(|tx| supersede(tx, old, new)).unwrap();

        let old_item = db.read(|r| get(r, old)).unwrap().unwrap();
        assert_eq!(old_item.status, MemoryStatus::Superseded);
        assert_eq!(old_item.superseded_by, Some(new));

        let hist = db.read(|r| history(r, old)).unwrap();
        assert_eq!(hist.len(), 2);
        assert_eq!(hist[0].id, old);
        assert_eq!(hist[1].id, new);

        let active = db
            .read(|r| {
                list(
                    r,
                    &MemoryFilter {
                        statuses: Some(vec![MemoryStatus::Candidate, MemoryStatus::Derived]),
                        ..Default::default()
                    },
                )
            })
            .unwrap();
        assert!(!active.iter().any(|m| m.id == old));

        let err = db.write(|tx| supersede(tx, new, old)).unwrap_err();
        assert!(matches!(err, StoreError::Invariant(_)));
    }

    #[test]
    fn fingerprint_dedupe_and_reinforce() {
        let db = Database::open_in_memory().unwrap();
        let fp = MemoryFingerprint::compute(MemoryKind::Fact, &[EntityId::User(UserId(1))], "x");
        let mut m = new_agent_memory("likes tea");
        m.fingerprint = Some(fp.clone());
        m.confidence = Confidence::new(0.3).unwrap();
        let id = db.write(|tx| insert(tx, &m)).unwrap().value;

        let found = db.read(|r| find_active_by_fingerprint(r, &fp)).unwrap();
        assert_eq!(found.map(|i| i.id), Some(id));

        let extra = SourceRef::new(EntityId::User(UserId(2)));
        let changed = db
            .write(|tx| {
                reinforce(
                    tx,
                    id,
                    std::slice::from_ref(&extra),
                    Confidence::new(0.9).unwrap(),
                )
            })
            .unwrap()
            .value;
        assert!(changed);
        let item = db.read(|r| get(r, id)).unwrap().unwrap();
        assert!((item.confidence.get() - 0.9).abs() < 1e-6);
        assert_eq!(item.source_refs.len(), 1);
    }

    #[test]
    fn expire_due_skips_pinned() {
        let db = Database::open_in_memory().unwrap();
        let mut a = new_agent_memory("a");
        a.expires_at = Some(Timestamp::from_millis(100));
        let mut b = new_agent_memory("b");
        b.expires_at = Some(Timestamp::from_millis(100));
        b.pinned = true;
        let id_a = db.write(|tx| insert(tx, &a)).unwrap().value;
        let id_b = db.write(|tx| insert(tx, &b)).unwrap().value;

        let expired = db
            .write(|tx| expire_due(tx, Timestamp::from_millis(200)))
            .unwrap()
            .value;
        assert_eq!(expired, vec![id_a]);
        assert_eq!(
            db.read(|r| get(r, id_a)).unwrap().unwrap().status,
            MemoryStatus::Expired
        );
        assert_eq!(
            db.read(|r| get(r, id_b)).unwrap().unwrap().status,
            MemoryStatus::Candidate
        );
    }

    #[test]
    fn search_finds_by_content_and_respects_status() {
        let db = Database::open_in_memory().unwrap();
        let mut confirmed =
            NewMemory::new(MemoryKind::Fact, "the sky is blue", Origin::UserProvided);
        confirmed.status = MemoryStatus::UserConfirmed;
        db.write(|tx| insert(tx, &confirmed)).unwrap();
        db.write(|tx| insert(tx, &new_agent_memory("unrelated content")))
            .unwrap();

        let results = db
            .read(|r| {
                search(
                    r,
                    &MemorySearch {
                        query: "sky",
                        statuses: None,
                        kinds: None,
                        entity: None,
                        since: None,
                        mode: FtsMode::Any,
                        limit: 10,
                    },
                )
            })
            .unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].0.content.contains("sky"));

        let none = db
            .read(|r| {
                search(
                    r,
                    &MemorySearch {
                        query: "sky",
                        statuses: Some(&[MemoryStatus::Candidate]),
                        kinds: None,
                        entity: None,
                        since: None,
                        mode: FtsMode::Any,
                        limit: 10,
                    },
                )
            })
            .unwrap();
        assert!(none.is_empty());
    }
}
