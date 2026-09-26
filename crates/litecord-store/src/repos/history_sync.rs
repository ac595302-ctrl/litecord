//! Paged, resumable history backfill bookkeeping (`history_sync`).
//!
//! One row per conversation the user selected for history sync. The row's
//! cursor (`oldest_message_id`) marks how far back the backfill has walked;
//! the next page is requested with `before = oldest_message_id`. The reducer
//! advances it via [`advance`] in the same transaction that stores a
//! `DiscordEvent::MessagesPage`, so a page and its checkpoint always commit
//! (or roll back) together. No unified events are emitted from this module.

use rusqlite::{params, Connection, OptionalExtension, Row};

use litecord_types::{ConversationId, MessageId, Timestamp};

use crate::db::WriteTx;
use crate::error::StoreResult;
use crate::repos::messages;

/// Longest `last_error` / `paused_reason` stored, in characters.
const MAX_REASON_CHARS: usize = 200;

/// One row of the `history_sync` table.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct HistorySyncRecord {
    pub conversation_id: ConversationId,
    pub enabled: bool,
    /// Oldest message the backfill has reached; `None` until anything is
    /// known (then the first page is requested without a cursor).
    pub oldest_message_id: Option<MessageId>,
    /// An empty page came back: the beginning of history was reached.
    pub complete: bool,
    pub pages: u64,
    pub messages: u64,
    pub last_error: Option<String>,
    pub paused_reason: Option<String>,
    pub requested_at: Timestamp,
    pub updated_at: Timestamp,
}

const COLUMNS: &str = "conversation_id, enabled, oldest_message_id, complete, pages, messages, \
     last_error, paused_reason, requested_at, updated_at";

fn map_row(row: &Row<'_>) -> rusqlite::Result<HistorySyncRecord> {
    Ok(HistorySyncRecord {
        conversation_id: ConversationId::from_sql(row.get(0)?),
        enabled: row.get::<_, i64>(1)? != 0,
        oldest_message_id: row.get::<_, Option<i64>>(2)?.map(MessageId::from_sql),
        complete: row.get::<_, i64>(3)? != 0,
        pages: row.get::<_, i64>(4)?.max(0) as u64,
        messages: row.get::<_, i64>(5)?.max(0) as u64,
        last_error: row.get(6)?,
        paused_reason: row.get(7)?,
        requested_at: Timestamp::from_millis(row.get(8)?),
        updated_at: Timestamp::from_millis(row.get(9)?),
    })
}

fn truncate(s: Option<&str>) -> Option<String> {
    s.map(|s| s.chars().take(MAX_REASON_CHARS).collect())
}

/// Request history sync for a conversation: inserts a row (cursor starting
/// at the oldest cached message of the conversation, if any) or re-enables
/// an existing one, keeping its cursor and counters. Clears any pause
/// reason and error. Returns whether a row changed.
pub fn request(tx: &WriteTx<'_>, conversation_id: ConversationId) -> StoreResult<bool> {
    let initial_oldest = messages::oldest_in(tx, conversation_id)?;
    let now = tx.now().as_millis();
    let n = tx.execute(
        "INSERT INTO history_sync (conversation_id, enabled, oldest_message_id, requested_at, updated_at)
         VALUES (?1, 1, ?2, ?3, ?3)
         ON CONFLICT(conversation_id) DO UPDATE SET
             enabled = 1,
             oldest_message_id = COALESCE(history_sync.oldest_message_id, excluded.oldest_message_id),
             last_error = NULL,
             paused_reason = NULL,
             requested_at = excluded.requested_at,
             updated_at = excluded.updated_at",
        params![
            conversation_id.to_sql(),
            initial_oldest.map(|m| m.to_sql()),
            now
        ],
    )?;
    Ok(n > 0)
}

/// Stop syncing a conversation (the row and its cursor are kept so a later
/// [`request`] resumes). Returns whether a row changed.
pub fn disable(tx: &WriteTx<'_>, conversation_id: ConversationId) -> StoreResult<bool> {
    let n = tx.execute(
        "UPDATE history_sync SET enabled = 0, updated_at = ?2
         WHERE conversation_id = ?1 AND enabled = 1",
        params![conversation_id.to_sql(), tx.now().as_millis()],
    )?;
    Ok(n > 0)
}

pub fn get(
    conn: &Connection,
    conversation_id: ConversationId,
) -> StoreResult<Option<HistorySyncRecord>> {
    let sql = format!("SELECT {COLUMNS} FROM history_sync WHERE conversation_id = ?1");
    Ok(conn
        .query_row(&sql, params![conversation_id.to_sql()], map_row)
        .optional()?)
}

/// All rows, by conversation id.
pub fn list(conn: &Connection) -> StoreResult<Vec<HistorySyncRecord>> {
    let sql = format!("SELECT {COLUMNS} FROM history_sync ORDER BY conversation_id");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], map_row)?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// The next conversation to backfill: enabled and not complete, least
/// recently updated first (so work round-robins across conversations).
/// `paused_reason` is informational and does not exclude a row; the
/// scheduler decides when to pause globally.
pub fn next_pending(conn: &Connection) -> StoreResult<Option<HistorySyncRecord>> {
    let sql = format!(
        "SELECT {COLUMNS} FROM history_sync
         WHERE enabled = 1 AND complete = 0
         ORDER BY updated_at ASC, conversation_id ASC
         LIMIT 1"
    );
    Ok(conn.query_row(&sql, [], map_row).optional()?)
}

/// Record one stored history page: moves the cursor back to
/// `min(existing, page_oldest)`, adds one page and `count` messages, clears
/// `last_error`, and marks the row complete when the page was empty
/// (`count == 0`). A no-op (returns `false`) when the conversation has no
/// `history_sync` row. Called by the reducer inside the page's transaction.
pub fn advance(
    tx: &WriteTx<'_>,
    conversation_id: ConversationId,
    page_oldest: Option<MessageId>,
    count: usize,
) -> StoreResult<bool> {
    let n = tx.execute(
        "UPDATE history_sync SET
             oldest_message_id = CASE
                 WHEN ?2 IS NULL THEN oldest_message_id
                 WHEN oldest_message_id IS NULL OR ?2 < oldest_message_id THEN ?2
                 ELSE oldest_message_id
             END,
             pages = pages + 1,
             messages = messages + ?3,
             complete = CASE WHEN ?3 = 0 THEN 1 ELSE complete END,
             last_error = NULL,
             updated_at = ?4
         WHERE conversation_id = ?1",
        params![
            conversation_id.to_sql(),
            page_oldest.map(|m| m.to_sql()),
            count as i64,
            tx.now().as_millis(),
        ],
    )?;
    Ok(n > 0)
}

/// Set (or clear, with `None`) the last error, truncated to 200 characters.
/// Setting an error also bumps `updated_at`, moving the conversation to the
/// back of the [`next_pending`] queue.
pub fn set_error(
    tx: &WriteTx<'_>,
    conversation_id: ConversationId,
    error: Option<&str>,
) -> StoreResult<bool> {
    let n = tx.execute(
        "UPDATE history_sync SET
             last_error = ?2,
             updated_at = CASE WHEN ?2 IS NULL THEN updated_at ELSE ?3 END
         WHERE conversation_id = ?1 AND last_error IS NOT ?2",
        params![
            conversation_id.to_sql(),
            truncate(error),
            tx.now().as_millis()
        ],
    )?;
    Ok(n > 0)
}

/// Set (or clear, with `None`) why syncing this conversation is paused.
pub fn set_paused(
    tx: &WriteTx<'_>,
    conversation_id: ConversationId,
    reason: Option<&str>,
) -> StoreResult<bool> {
    let n = tx.execute(
        "UPDATE history_sync SET paused_reason = ?2
         WHERE conversation_id = ?1 AND paused_reason IS NOT ?2",
        params![conversation_id.to_sql(), truncate(reason)],
    )?;
    Ok(n > 0)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::db::Database;
    use crate::error::StoreError;
    use litecord_types::provenance::Origin;
    use litecord_types::social::Message;
    use litecord_types::UserId;

    fn msg(id: u64, conv: u64) -> Message {
        Message {
            id: MessageId(id),
            conversation_id: ConversationId(conv),
            author_id: UserId(1),
            content: "x".into(),
            sent_at: Timestamp::from_millis(id as i64),
            edited_at: None,
            reply_to: None,
            extras: Vec::new(),
        }
    }

    #[test]
    fn request_starts_at_oldest_cached_and_reenables() {
        let db = Database::open_in_memory().unwrap();
        let conv = ConversationId(10);
        db.write(|tx| {
            for id in [30, 20, 25] {
                messages::upsert(
                    tx,
                    &msg(id, 10),
                    Origin::Synthetic,
                    Timestamp::from_millis(0),
                )?;
            }
            Ok::<_, StoreError>(())
        })
        .unwrap();

        db.write(|tx| request(tx, conv)).unwrap();
        let rec = db.read(|r| get(r, conv)).unwrap().unwrap();
        assert!(rec.enabled && !rec.complete);
        assert_eq!(rec.oldest_message_id, Some(MessageId(20)));

        db.write(|tx| advance(tx, conv, Some(MessageId(5)), 3))
            .unwrap();
        assert!(db.write(|tx| disable(tx, conv)).unwrap().value);
        assert!(db.read(|r| next_pending(r)).unwrap().is_none());

        // Re-enabling keeps the cursor and counters.
        db.write(|tx| request(tx, conv)).unwrap();
        let rec = db.read(|r| get(r, conv)).unwrap().unwrap();
        assert!(rec.enabled);
        assert_eq!(rec.oldest_message_id, Some(MessageId(5)));
        assert_eq!((rec.pages, rec.messages), (1, 3));

        // No cached messages: no initial cursor.
        db.write(|tx| request(tx, ConversationId(11))).unwrap();
        let rec = db.read(|r| get(r, ConversationId(11))).unwrap().unwrap();
        assert_eq!(rec.oldest_message_id, None);
        assert_eq!(db.read(|r| list(r)).unwrap().len(), 2);
    }

    #[test]
    fn advance_moves_cursor_back_only_and_empty_page_completes() {
        let db = Database::open_in_memory().unwrap();
        let conv = ConversationId(10);
        db.write(|tx| request(tx, conv)).unwrap();

        db.write(|tx| advance(tx, conv, Some(MessageId(50)), 10))
            .unwrap();
        db.write(|tx| advance(tx, conv, Some(MessageId(70)), 2))
            .unwrap();
        let rec = db.read(|r| get(r, conv)).unwrap().unwrap();
        assert_eq!(rec.oldest_message_id, Some(MessageId(50)));
        assert_eq!((rec.pages, rec.messages, rec.complete), (2, 12, false));

        db.write(|tx| advance(tx, conv, None, 0)).unwrap();
        let rec = db.read(|r| get(r, conv)).unwrap().unwrap();
        assert!(rec.complete);
        assert_eq!(rec.oldest_message_id, Some(MessageId(50)));
        assert!(db.read(|r| next_pending(r)).unwrap().is_none());

        // No row: no-op.
        assert!(
            !db.write(|tx| advance(tx, ConversationId(99), None, 0))
                .unwrap()
                .value
        );
    }

    #[test]
    fn next_pending_is_least_recently_updated_and_errors_rotate() {
        let clock = std::sync::Arc::new(litecord_core::clock::ManualClock::new(
            Timestamp::from_millis(1_000),
        ));
        let db = Database::open_in_memory_with(clock.clone(), None).unwrap();
        db.write(|tx| request(tx, ConversationId(1))).unwrap();
        clock.advance(litecord_types::DurationMs::from_secs(1));
        db.write(|tx| request(tx, ConversationId(2))).unwrap();
        clock.advance(litecord_types::DurationMs::from_secs(1));

        let next = db.read(|r| next_pending(r)).unwrap().unwrap();
        assert_eq!(next.conversation_id, ConversationId(1));

        let long = "e".repeat(500);
        db.write(|tx| set_error(tx, ConversationId(1), Some(&long)))
            .unwrap();
        let rec = db.read(|r| get(r, ConversationId(1))).unwrap().unwrap();
        assert_eq!(rec.last_error.as_ref().map(String::len), Some(200));
        let next = db.read(|r| next_pending(r)).unwrap().unwrap();
        assert_eq!(next.conversation_id, ConversationId(2));

        db.write(|tx| set_paused(tx, ConversationId(2), Some("user active")))
            .unwrap();
        let rec = db.read(|r| get(r, ConversationId(2))).unwrap().unwrap();
        assert_eq!(rec.paused_reason.as_deref(), Some("user active"));
        db.write(|tx| set_paused(tx, ConversationId(2), None))
            .unwrap();
        db.write(|tx| set_error(tx, ConversationId(1), None))
            .unwrap();
        let rec = db.read(|r| get(r, ConversationId(1))).unwrap().unwrap();
        assert_eq!(rec.last_error, None);
    }
}
