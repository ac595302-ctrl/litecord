//! Local-only user annotations and message bookmarks (V1 §17). Never sent to
//! Discord.

use rusqlite::{params, Connection, OptionalExtension, Row};

use litecord_core::events::UnifiedEvent;
use litecord_types::notes::{Bookmark, UserNote};
use litecord_types::provenance::Origin;
use litecord_types::{ConversationId, MessageId, Timestamp, UserId};

use crate::db::WriteTx;
use crate::error::StoreResult;
use crate::repos::fts::{self, FtsMode};

fn row_to_note(row: &Row<'_>) -> rusqlite::Result<UserNote> {
    let user_id = UserId::from_sql(row.get(0)?);
    let alias: Option<String> = row.get(1)?;
    let note: Option<String> = row.get(2)?;
    let favorite: i64 = row.get(3)?;
    let updated_at: i64 = row.get(4)?;
    Ok(UserNote {
        user_id,
        alias,
        note,
        favorite: favorite != 0,
        updated_at: Timestamp::from_millis(updated_at),
    })
}

const NOTE_COLUMNS: &str = "user_id, alias, note, favorite, updated_at";

/// Insert or replace a user's note record wholesale. `updated_at` is stamped
/// from the transaction clock (the caller's value is ignored). No-op (no
/// event) if nothing actually differs from what is stored.
pub fn upsert_note(tx: &WriteTx<'_>, note: &UserNote) -> StoreResult<bool> {
    let now = tx.now();
    let n = tx.execute(
        "INSERT INTO user_notes (user_id, alias, note, favorite, updated_at) VALUES (?, ?, ?, ?, ?) \
         ON CONFLICT(user_id) DO UPDATE SET \
            alias = excluded.alias, note = excluded.note, favorite = excluded.favorite, updated_at = excluded.updated_at \
         WHERE user_notes.alias IS NOT excluded.alias OR user_notes.note IS NOT excluded.note \
            OR user_notes.favorite IS NOT excluded.favorite",
        params![note.user_id.to_sql(), note.alias, note.note, note.favorite as i64, now.as_millis()],
    )?;
    let changed = n > 0;
    if changed {
        tx.emit(
            UnifiedEvent::NoteUpdated {
                user_id: note.user_id,
            },
            Origin::UserProvided,
        )?;
    }
    Ok(changed)
}

/// Append `text` on a new line to the user's existing note (creating one if
/// absent). A blank `text` is a no-op. Emits [`UnifiedEvent::NoteUpdated`].
pub fn append_note(
    tx: &WriteTx<'_>,
    user_id: UserId,
    text: &str,
    origin: Origin,
) -> StoreResult<bool> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(false);
    }
    let existing: Option<Option<String>> = tx
        .query_row(
            "SELECT note FROM user_notes WHERE user_id = ?",
            params![user_id.to_sql()],
            |r| r.get(0),
        )
        .optional()?;
    let new_note = match existing.flatten() {
        Some(prev) if !prev.is_empty() => format!("{prev}\n{text}"),
        _ => text.to_string(),
    };
    tx.execute(
        "INSERT INTO user_notes (user_id, alias, note, favorite, updated_at) VALUES (?, NULL, ?, 0, ?) \
         ON CONFLICT(user_id) DO UPDATE SET note = excluded.note, updated_at = excluded.updated_at",
        params![user_id.to_sql(), new_note, tx.now().as_millis()],
    )?;
    tx.emit(UnifiedEvent::NoteUpdated { user_id }, origin)?;
    Ok(true)
}

/// Load a user's note record, if any.
pub fn get_note(conn: &Connection, user_id: UserId) -> StoreResult<Option<UserNote>> {
    let sql = format!("SELECT {NOTE_COLUMNS} FROM user_notes WHERE user_id = ?");
    Ok(conn
        .query_row(&sql, params![user_id.to_sql()], row_to_note)
        .optional()?)
}

/// All favorited users' notes.
pub fn favorites(conn: &Connection) -> StoreResult<Vec<UserNote>> {
    let sql = format!(
        "SELECT {NOTE_COLUMNS} FROM user_notes WHERE favorite = 1 ORDER BY updated_at DESC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], row_to_note)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Full-text search over alias/note content, best match first.
pub fn search_notes(
    conn: &Connection,
    query: &str,
    limit: u32,
) -> StoreResult<Vec<(UserNote, f64)>> {
    let Some(expr) = fts::match_expr(query, FtsMode::Any, false) else {
        return Ok(Vec::new());
    };
    let sql = format!(
        "SELECT {}, bm25(notes_fts) FROM notes_fts JOIN user_notes n ON n.user_id = notes_fts.rowid \
         WHERE notes_fts MATCH ? ORDER BY bm25(notes_fts) ASC LIMIT ?",
        NOTE_COLUMNS.split(", ").map(|c| format!("n.{c}")).collect::<Vec<_>>().join(", ")
    );
    let limit = if limit == 0 { 50 } else { limit };
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![expr, limit as i64], |row| {
        let note = row_to_note(row)?;
        let bm25: f64 = row.get(5)?;
        Ok((note, bm25))
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

fn row_to_bookmark(row: &Row<'_>) -> rusqlite::Result<Bookmark> {
    let message_id = MessageId::from_sql(row.get(0)?);
    let conversation_id = ConversationId::from_sql(row.get(1)?);
    let note: Option<String> = row.get(2)?;
    let created_at: i64 = row.get(3)?;
    Ok(Bookmark {
        message_id,
        conversation_id,
        note,
        created_at: Timestamp::from_millis(created_at),
    })
}

const BOOKMARK_COLUMNS: &str = "message_id, conversation_id, note, created_at";

/// Add a bookmark, or update its note if the message is already bookmarked
/// with a different note. Emits [`UnifiedEvent::BookmarkChanged`] when
/// something changed.
pub fn add_bookmark(tx: &WriteTx<'_>, bookmark: &Bookmark, origin: Origin) -> StoreResult<bool> {
    let n = tx.execute(
        "INSERT INTO bookmarks (message_id, conversation_id, note, created_at) VALUES (?, ?, ?, ?) \
         ON CONFLICT(message_id) DO UPDATE SET note = excluded.note \
         WHERE bookmarks.note IS NOT excluded.note",
        params![
            bookmark.message_id.to_sql(),
            bookmark.conversation_id.to_sql(),
            bookmark.note,
            bookmark.created_at.as_millis()
        ],
    )?;
    let changed = n > 0;
    if changed {
        tx.emit(
            UnifiedEvent::BookmarkChanged {
                message_id: bookmark.message_id,
            },
            origin,
        )?;
    }
    Ok(changed)
}

/// Remove a bookmark. Emits [`UnifiedEvent::BookmarkChanged`] if one existed.
pub fn remove_bookmark(
    tx: &WriteTx<'_>,
    message_id: MessageId,
    origin: Origin,
) -> StoreResult<bool> {
    let n = tx.execute(
        "DELETE FROM bookmarks WHERE message_id = ?",
        params![message_id.to_sql()],
    )?;
    let changed = n > 0;
    if changed {
        tx.emit(UnifiedEvent::BookmarkChanged { message_id }, origin)?;
    }
    Ok(changed)
}

/// List bookmarks, newest first. `limit = 0` means unlimited.
pub fn list_bookmarks(conn: &Connection, limit: u32) -> StoreResult<Vec<Bookmark>> {
    let mut sql = format!("SELECT {BOOKMARK_COLUMNS} FROM bookmarks ORDER BY created_at DESC");
    if limit > 0 {
        sql.push_str(" LIMIT ?");
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![limit], row_to_bookmark)?;
        return Ok(rows.collect::<Result<Vec<_>, _>>()?);
    }
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], row_to_bookmark)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Whether a message is bookmarked.
pub fn is_bookmarked(conn: &Connection, message_id: MessageId) -> StoreResult<bool> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM bookmarks WHERE message_id = ?",
        params![message_id.to_sql()],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::db::Database;

    #[test]
    fn append_note_creates_and_appends() {
        let db = Database::open_in_memory().unwrap();
        let user = UserId(1);
        db.write(|tx| append_note(tx, user, "likes tea", Origin::UserProvided))
            .unwrap();
        db.write(|tx| append_note(tx, user, "likes coffee", Origin::UserProvided))
            .unwrap();
        let note = db.read(|r| get_note(r, user)).unwrap().unwrap();
        assert_eq!(note.note.as_deref(), Some("likes tea\nlikes coffee"));
    }

    #[test]
    fn bookmarks_are_idempotent() {
        let db = Database::open_in_memory().unwrap();
        let b = Bookmark {
            message_id: MessageId(1),
            conversation_id: ConversationId(1),
            note: None,
            created_at: Timestamp::from_millis(0),
        };
        let c1 = db
            .write(|tx| add_bookmark(tx, &b, Origin::UserProvided))
            .unwrap();
        assert!(c1.changed());
        let c2 = db
            .write(|tx| add_bookmark(tx, &b, Origin::UserProvided))
            .unwrap();
        assert!(!c2.changed());
        assert!(db.read(|r| is_bookmarked(r, b.message_id)).unwrap());

        db.write(|tx| remove_bookmark(tx, b.message_id, Origin::UserProvided))
            .unwrap();
        assert!(!db.read(|r| is_bookmarked(r, b.message_id)).unwrap());
    }
}
