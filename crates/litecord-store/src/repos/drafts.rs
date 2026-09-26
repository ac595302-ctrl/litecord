//! Local message drafts. Drafts never leave the machine by themselves — only
//! the Action Engine, through an explicit `SendMessage` proposal, can turn
//! one into an outgoing message.

use rusqlite::{params, types::Value, Connection, OptionalExtension, Row};

use litecord_core::events::UnifiedEvent;
use litecord_types::provenance::Origin;
use litecord_types::tasks::{Draft, DraftStatus};
use litecord_types::{ConversationId, DraftId, Timestamp};

use crate::db::WriteTx;
use crate::error::StoreResult;
use crate::sql::{col_err, push_in_str, rev};

const SELECT_COLUMNS: &str =
    "id, conversation_id, content, origin, status, created_at, updated_at, revision";

fn row_to_draft(row: &Row<'_>) -> rusqlite::Result<Draft> {
    let id = DraftId(row.get(0)?);
    let conversation_id: i64 = row.get(1)?;
    let content: String = row.get(2)?;
    let origin_s: String = row.get(3)?;
    let origin = crate::sql::origin(3, &origin_s)?;
    let status: String = row.get(4)?;
    let status = DraftStatus::parse(&status).map_err(|e| col_err(4, e))?;
    let created_at: i64 = row.get(5)?;
    let updated_at: i64 = row.get(6)?;
    let revision: i64 = row.get(7)?;
    Ok(Draft {
        id,
        conversation_id: ConversationId::from_sql(conversation_id),
        content,
        origin,
        status,
        created_at: Timestamp::from_millis(created_at),
        updated_at: Timestamp::from_millis(updated_at),
        revision: rev(revision),
    })
}

/// Create a new open draft. Emits [`UnifiedEvent::DraftSaved`].
pub fn create(
    tx: &WriteTx<'_>,
    conversation_id: ConversationId,
    content: &str,
    origin: Origin,
) -> StoreResult<DraftId> {
    let now = tx.now();
    let revision = tx.revision().get() as i64;
    tx.execute(
        "INSERT INTO drafts (conversation_id, content, origin, status, created_at, updated_at, revision) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
        params![
            conversation_id.to_sql(),
            content,
            origin.as_str(),
            DraftStatus::Open.as_str(),
            now.as_millis(),
            now.as_millis(),
            revision,
        ],
    )?;
    let id = DraftId(tx.last_insert_rowid());
    tx.emit(UnifiedEvent::DraftSaved { draft_id: id }, origin)?;
    Ok(id)
}

/// Replace a draft's content. No-op (no event) if the content is unchanged.
pub fn update_content(tx: &WriteTx<'_>, id: DraftId, content: &str) -> StoreResult<bool> {
    let n = tx.execute(
        "UPDATE drafts SET content = ?, updated_at = ?, revision = ? WHERE id = ? AND content IS NOT ?",
        params![content, tx.now().as_millis(), tx.revision().get() as i64, id.get(), content],
    )?;
    Ok(n > 0)
}

/// Change a draft's status. No-op (no event) if unchanged.
pub fn set_status(tx: &WriteTx<'_>, id: DraftId, status: DraftStatus) -> StoreResult<bool> {
    let n = tx.execute(
        "UPDATE drafts SET status = ?, updated_at = ?, revision = ? WHERE id = ? AND status IS NOT ?",
        params![
            status.as_str(),
            tx.now().as_millis(),
            tx.revision().get() as i64,
            id.get(),
            status.as_str()
        ],
    )?;
    Ok(n > 0)
}

/// Load a draft by id.
pub fn get(conn: &Connection, id: DraftId) -> StoreResult<Option<Draft>> {
    let sql = format!("SELECT {SELECT_COLUMNS} FROM drafts WHERE id = ?");
    Ok(conn
        .query_row(&sql, params![id.get()], row_to_draft)
        .optional()?)
}

/// List drafts, optionally filtered by conversation and/or status, newest
/// first. `limit = 0` means unlimited.
pub fn list(
    conn: &Connection,
    conversation_id: Option<ConversationId>,
    statuses: Option<&[DraftStatus]>,
    limit: u32,
) -> StoreResult<Vec<Draft>> {
    let mut sql = format!("SELECT {SELECT_COLUMNS} FROM drafts");
    let mut conditions = Vec::new();
    let mut params: Vec<Value> = Vec::new();

    if let Some(conv) = conversation_id {
        conditions.push("conversation_id = ?".to_string());
        params.push(Value::Integer(conv.to_sql()));
    }
    if let Some(statuses) = statuses {
        push_in_str(
            &mut conditions,
            &mut params,
            "status",
            statuses.iter().map(|s| s.as_str()),
        );
    }
    if !conditions.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&conditions.join(" AND "));
    }
    sql.push_str(" ORDER BY updated_at DESC");
    if limit > 0 {
        sql.push_str(" LIMIT ?");
        params.push(Value::Integer(limit as i64));
    }
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(params), row_to_draft)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::db::Database;

    #[test]
    fn create_update_and_list() {
        let db = Database::open_in_memory().unwrap();
        let conv = ConversationId(1);
        let id = db
            .write(|tx| create(tx, conv, "hello", Origin::UserProvided))
            .unwrap()
            .value;
        let d = db.read(|r| get(r, id)).unwrap().unwrap();
        assert_eq!(d.status, DraftStatus::Open);

        let changed = db
            .write(|tx| update_content(tx, id, "hello world"))
            .unwrap()
            .value;
        assert!(changed);
        let noop = db
            .write(|tx| update_content(tx, id, "hello world"))
            .unwrap();
        assert!(!noop.changed());

        db.write(|tx| set_status(tx, id, DraftStatus::Sent))
            .unwrap();
        let listed = db
            .read(|r| list(r, Some(conv), Some(&[DraftStatus::Sent]), 10))
            .unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].content, "hello world");
    }
}
