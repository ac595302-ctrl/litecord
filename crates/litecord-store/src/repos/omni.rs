//! Omni sessions and their capped transcripts (`0003_omni.sql`).
//!
//! Only completed items are stored; streaming deltas never touch SQLite.
//! Values such as `harness`, `kind` and `mode` are the lowercase strings
//! defined by `litecord-harness`; the store does not interpret them.

use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::Serialize;

use litecord_types::Timestamp;

use crate::db::WriteTx;
use crate::error::StoreResult;

/// Longest stored transcript text; longer items are truncated.
pub const MAX_ITEM_CHARS: usize = 32_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OmniSession {
    pub id: i64,
    pub harness: String,
    pub external_id: Option<String>,
    pub kind: String,
    pub mode: String,
    pub profile: Option<String>,
    pub title: String,
    pub parent_id: Option<i64>,
    pub status: String,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub turns: u32,
    pub created_at: Timestamp,
    pub last_active_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OmniItem {
    pub seq: u32,
    pub role: String,
    pub kind: String,
    pub text: String,
    pub created_at: Timestamp,
}

#[derive(Debug, Clone)]
pub struct NewSession<'a> {
    pub harness: &'a str,
    pub kind: &'a str,
    pub mode: &'a str,
    pub profile: Option<&'a str>,
    pub title: &'a str,
    pub parent_id: Option<i64>,
}

const COLUMNS: &str = "id, harness, external_id, kind, mode, profile, title, parent_id, status, \
                       tokens_in, tokens_out, turns, created_at, last_active_at";

fn row_to_session(r: &Row<'_>) -> rusqlite::Result<OmniSession> {
    Ok(OmniSession {
        id: r.get(0)?,
        harness: r.get(1)?,
        external_id: r.get(2)?,
        kind: r.get(3)?,
        mode: r.get(4)?,
        profile: r.get(5)?,
        title: r.get(6)?,
        parent_id: r.get(7)?,
        status: r.get(8)?,
        tokens_in: r.get::<_, i64>(9)?.max(0) as u64,
        tokens_out: r.get::<_, i64>(10)?.max(0) as u64,
        turns: r.get::<_, i64>(11)?.max(0) as u32,
        created_at: Timestamp::from_millis(r.get(12)?),
        last_active_at: Timestamp::from_millis(r.get(13)?),
    })
}

fn truncate(s: &str, max: usize) -> &str {
    match s.char_indices().nth(max) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

pub fn create_session(tx: &WriteTx<'_>, s: &NewSession<'_>) -> StoreResult<i64> {
    let now = tx.now().as_millis();
    tx.execute(
        "INSERT INTO omni_sessions (harness, kind, mode, profile, title, parent_id, created_at, last_active_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        params![s.harness, s.kind, s.mode, s.profile, truncate(s.title, 120), s.parent_id, now, now],
    )?;
    Ok(tx.last_insert_rowid())
}

pub fn set_external_id(tx: &WriteTx<'_>, id: i64, external_id: Option<&str>) -> StoreResult<()> {
    tx.execute(
        "UPDATE omni_sessions SET external_id = ? WHERE id = ?",
        params![external_id, id],
    )?;
    Ok(())
}

pub fn set_title(tx: &WriteTx<'_>, id: i64, title: &str) -> StoreResult<()> {
    tx.execute(
        "UPDATE omni_sessions SET title = ? WHERE id = ?",
        params![truncate(title, 120), id],
    )?;
    Ok(())
}

/// Record a finished turn: bumps counters and `last_active_at`.
pub fn record_turn(tx: &WriteTx<'_>, id: i64, tokens_in: u64, tokens_out: u64) -> StoreResult<()> {
    tx.execute(
        "UPDATE omni_sessions SET tokens_in = tokens_in + ?, tokens_out = tokens_out + ?, \
         turns = turns + 1, last_active_at = ? WHERE id = ?",
        params![
            tokens_in as i64,
            tokens_out as i64,
            tx.now().as_millis(),
            id
        ],
    )?;
    Ok(())
}

pub fn archive(tx: &WriteTx<'_>, id: i64) -> StoreResult<()> {
    tx.execute(
        "UPDATE omni_sessions SET status = 'archived' WHERE id = ? AND status != 'archived'",
        params![id],
    )?;
    Ok(())
}

pub fn get(conn: &Connection, id: i64) -> StoreResult<Option<OmniSession>> {
    Ok(conn
        .query_row(
            &format!("SELECT {COLUMNS} FROM omni_sessions WHERE id = ?"),
            params![id],
            row_to_session,
        )
        .optional()?)
}

/// Newest sessions first. `kind = None` lists every kind.
pub fn list(
    conn: &Connection,
    kind: Option<&str>,
    include_archived: bool,
    limit: u32,
) -> StoreResult<Vec<OmniSession>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM omni_sessions \
         WHERE (?1 IS NULL OR kind = ?1) AND (?2 OR status = 'active') \
         ORDER BY last_active_at DESC, id DESC LIMIT ?3"
    ))?;
    let rows = stmt.query_map(params![kind, include_archived, limit], row_to_session)?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Appends a completed item; returns its sequence number.
pub fn append_item(
    tx: &WriteTx<'_>,
    session_id: i64,
    role: &str,
    kind: &str,
    text: &str,
) -> StoreResult<u32> {
    let seq: i64 = tx.query_row(
        "SELECT COALESCE(MAX(seq), 0) + 1 FROM omni_items WHERE session_id = ?",
        params![session_id],
        |r| r.get(0),
    )?;
    tx.execute(
        "INSERT INTO omni_items (session_id, seq, role, kind, text, created_at) VALUES (?, ?, ?, ?, ?, ?)",
        params![session_id, seq, role, kind, truncate(text, MAX_ITEM_CHARS), tx.now().as_millis()],
    )?;
    tx.execute(
        "UPDATE omni_sessions SET last_active_at = ? WHERE id = ?",
        params![tx.now().as_millis(), session_id],
    )?;
    Ok(seq as u32)
}

/// The last `limit` items of a session, oldest first.
pub fn items(conn: &Connection, session_id: i64, limit: u32) -> StoreResult<Vec<OmniItem>> {
    let mut stmt = conn.prepare(
        "SELECT seq, role, kind, text, created_at FROM ( \
            SELECT * FROM omni_items WHERE session_id = ? ORDER BY seq DESC LIMIT ?) \
         ORDER BY seq ASC",
    )?;
    let rows = stmt.query_map(params![session_id, limit], |r| {
        Ok(OmniItem {
            seq: r.get::<_, i64>(0)?.max(0) as u32,
            role: r.get(1)?,
            kind: r.get(2)?,
            text: r.get(3)?,
            created_at: Timestamp::from_millis(r.get(4)?),
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Deletes archived sessions beyond the newest `keep` (items cascade).
/// Active sessions are never pruned.
pub fn prune_archived(tx: &WriteTx<'_>, keep: u32) -> StoreResult<usize> {
    Ok(tx.execute(
        "DELETE FROM omni_sessions WHERE status = 'archived' AND id NOT IN ( \
            SELECT id FROM omni_sessions WHERE status = 'archived' \
            ORDER BY last_active_at DESC, id DESC LIMIT ?)",
        params![keep],
    )?)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::Database;

    #[test]
    fn sessions_items_and_pruning() {
        let db = Database::open_in_memory().unwrap();
        let new = |title: &'static str| NewSession {
            harness: "fake",
            kind: "chat",
            mode: "assistant",
            profile: None,
            title,
            parent_id: None,
        };
        let a = db
            .write(|tx| create_session(tx, &new("first")))
            .unwrap()
            .value;
        db.write(|tx| -> StoreResult<()> {
            append_item(tx, a, "user", "message", "hi")?;
            append_item(tx, a, "omni", "message", &"x".repeat(MAX_ITEM_CHARS + 10))?;
            record_turn(tx, a, 10, 5)
        })
        .unwrap();
        let got = db.read(|r| items(r, a, 10)).unwrap();
        assert_eq!(got.iter().map(|i| i.seq).collect::<Vec<_>>(), [1, 2]);
        assert_eq!(got[1].text.chars().count(), MAX_ITEM_CHARS);
        let s = db.read(|r| get(r, a)).unwrap().unwrap();
        assert_eq!((s.turns, s.tokens_in, s.tokens_out), (1, 10, 5));

        let b = db.write(|tx| create_session(tx, &new("b"))).unwrap().value;
        let c = db.write(|tx| create_session(tx, &new("c"))).unwrap().value;
        db.write(|tx| -> StoreResult<()> {
            archive(tx, a)?;
            archive(tx, b)
        })
        .unwrap();
        assert_eq!(db.write(|tx| prune_archived(tx, 1)).unwrap().value, 1);
        let all = db.read(|r| list(r, None, true, 10)).unwrap();
        assert_eq!(all.len(), 2, "one archived kept, active untouched");
        assert!(all.iter().any(|s| s.id == c));
        assert!(all.iter().any(|s| s.id == b), "newest archived kept");
        assert!(
            db.read(|r| get(r, a)).unwrap().is_none(),
            "oldest archived pruned"
        );
        assert!(
            db.read(|r| items(r, a, 10)).unwrap().is_empty(),
            "items cascade"
        );
    }
}
