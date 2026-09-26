//! Canonical user profiles and presence.
//!
//! A user row can exist as a *stub* (`is_stub = 1`, empty username) created
//! from a reference — a message author, a relationship, a lobby member —
//! before the full profile is hydrated. [`upsert`] always clears the stub
//! flag; [`ensure_stub`] never resurrects a hydrated profile.

use rusqlite::{params, Connection, OptionalExtension, Row};

use litecord_core::events::UnifiedEvent;
use litecord_types::provenance::Origin;
use litecord_types::social::{Activity, Presence, PresenceStatus, User};
use litecord_types::{Revision, Timestamp, UserId};

use crate::db::WriteTx;
use crate::error::StoreResult;
use crate::repos::fts::{match_expr, FtsMode};
use crate::sql::col_err;

/// A stored user row plus bookkeeping.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct UserRecord {
    pub user: User,
    pub presence: Presence,
    pub is_stub: bool,
    pub origin: Origin,
    pub observed_at: Timestamp,
    pub revision: Revision,
}

const COLUMNS: &str = "id, username, global_name, avatar_url, is_bot, is_provisional, is_stub, \
     presence_status, activity_name, activity_details, activity_state, origin, observed_at, revision";

fn map_row(row: &Row<'_>) -> rusqlite::Result<UserRecord> {
    map_row_at(row, 0)
}

/// Like [`map_row`] but the user columns start at `offset` (for queries that
/// join users onto another aggregate, e.g. `relationships::list`).
pub(crate) fn map_row_at(row: &Row<'_>, offset: usize) -> rusqlite::Result<UserRecord> {
    let id = UserId::from_sql(row.get(offset)?);
    let username: String = row.get(offset + 1)?;
    let global_name: Option<String> = row.get(offset + 2)?;
    let avatar_url: Option<String> = row.get(offset + 3)?;
    let is_bot: bool = row.get::<_, i64>(offset + 4)? != 0;
    let is_provisional: bool = row.get::<_, i64>(offset + 5)? != 0;
    let is_stub: bool = row.get::<_, i64>(offset + 6)? != 0;
    let presence_status_s: String = row.get(offset + 7)?;
    let presence_status =
        PresenceStatus::parse(&presence_status_s).map_err(|e| col_err(offset + 7, e))?;
    let activity_name: Option<String> = row.get(offset + 8)?;
    let activity_details: Option<String> = row.get(offset + 9)?;
    let activity_state: Option<String> = row.get(offset + 10)?;
    let origin_s: String = row.get(offset + 11)?;
    let origin = crate::sql::origin(offset + 11, &origin_s)?;
    let observed_at = Timestamp::from_millis(row.get(offset + 12)?);
    let revision = crate::sql::rev(row.get(offset + 13)?);

    let activity = activity_name.map(|name| Activity {
        name,
        details: activity_details,
        state: activity_state,
    });

    Ok(UserRecord {
        user: User {
            id,
            username: username.into(),
            global_name: global_name.map(Into::into),
            avatar_url: avatar_url.map(Into::into),
            is_bot,
            is_provisional,
        },
        presence: Presence {
            status: presence_status,
            activity,
        },
        is_stub,
        origin,
        observed_at,
        revision,
    })
}

/// Insert or update a full user profile, clearing any stub flag. Emits
/// [`UnifiedEvent::UserUpdated`] only when a column actually changed.
pub fn upsert(
    tx: &WriteTx<'_>,
    user: &User,
    origin: Origin,
    observed_at: Timestamp,
) -> StoreResult<bool> {
    let n = tx.execute(
        "INSERT INTO users (id, username, global_name, avatar_url, is_bot, is_provisional, \
             is_stub, presence_status, origin, observed_at, revision)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, 'unknown', ?7, ?8, ?9)
         ON CONFLICT(id) DO UPDATE SET
             username = excluded.username,
             global_name = excluded.global_name,
             avatar_url = excluded.avatar_url,
             is_bot = excluded.is_bot,
             is_provisional = excluded.is_provisional,
             is_stub = 0,
             origin = excluded.origin,
             observed_at = excluded.observed_at,
             revision = excluded.revision
         WHERE users.username IS NOT excluded.username
            OR users.global_name IS NOT excluded.global_name
            OR users.avatar_url IS NOT excluded.avatar_url
            OR users.is_bot IS NOT excluded.is_bot
            OR users.is_provisional IS NOT excluded.is_provisional
            OR users.is_stub IS NOT 0",
        params![
            user.id.to_sql(),
            user.username.as_ref(),
            user.global_name.as_deref(),
            user.avatar_url.as_deref(),
            user.is_bot as i64,
            user.is_provisional as i64,
            origin.as_str(),
            observed_at.as_millis(),
            tx.revision().get() as i64,
        ],
    )?;
    let changed = n > 0;
    if changed {
        tx.emit(UnifiedEvent::UserUpdated { user_id: user.id }, origin)?;
    }
    Ok(changed)
}

/// Insert a placeholder row (empty username, `is_stub = 1`) if `id` is not
/// already known. Never overwrites an existing row. Emits nothing.
pub fn ensure_stub(
    tx: &WriteTx<'_>,
    id: UserId,
    origin: Origin,
    observed_at: Timestamp,
) -> StoreResult<bool> {
    let n = tx.execute(
        "INSERT OR IGNORE INTO users (id, username, is_bot, is_provisional, is_stub, \
             presence_status, origin, observed_at, revision)
         VALUES (?1, '', 0, 0, 1, 'unknown', ?2, ?3, ?4)",
        params![
            id.to_sql(),
            origin.as_str(),
            observed_at.as_millis(),
            tx.revision().get() as i64,
        ],
    )?;
    Ok(n > 0)
}

/// Update presence for `id`, creating a stub first if the user is unknown.
/// Emits [`UnifiedEvent::PresenceChanged`] only when the presence actually
/// changed.
pub fn set_presence(
    tx: &WriteTx<'_>,
    id: UserId,
    presence: &Presence,
    origin: Origin,
    observed_at: Timestamp,
) -> StoreResult<bool> {
    ensure_stub(tx, id, origin, observed_at)?;
    let (activity_name, activity_details, activity_state) = match &presence.activity {
        Some(a) => (
            Some(a.name.as_str()),
            a.details.as_deref(),
            a.state.as_deref(),
        ),
        None => (None, None, None),
    };
    let n = tx.execute(
        "UPDATE users SET
             presence_status = ?1,
             activity_name = ?2,
             activity_details = ?3,
             activity_state = ?4,
             presence_updated_at = ?5,
             origin = ?6,
             observed_at = ?7,
             revision = ?8
         WHERE id = ?9
           AND (presence_status IS NOT ?1
                OR activity_name IS NOT ?2
                OR activity_details IS NOT ?3
                OR activity_state IS NOT ?4)",
        params![
            presence.status.as_str(),
            activity_name,
            activity_details,
            activity_state,
            observed_at.as_millis(),
            origin.as_str(),
            observed_at.as_millis(),
            tx.revision().get() as i64,
            id.to_sql(),
        ],
    )?;
    let changed = n > 0;
    if changed {
        tx.emit(UnifiedEvent::PresenceChanged { user_id: id }, origin)?;
    }
    Ok(changed)
}

/// Look up one user by id.
pub fn get(conn: &Connection, id: UserId) -> StoreResult<Option<UserRecord>> {
    let sql = format!("SELECT {COLUMNS} FROM users WHERE id = ?1");
    Ok(conn
        .query_row(&sql, params![id.to_sql()], map_row)
        .optional()?)
}

/// Look up several users by id, in no particular order. Missing ids are
/// silently omitted.
pub fn get_many(conn: &Connection, ids: &[UserId]) -> StoreResult<Vec<UserRecord>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = vec!["?"; ids.len()].join(",");
    let sql = format!("SELECT {COLUMNS} FROM users WHERE id IN ({placeholders})");
    let mut stmt = conn.prepare(&sql)?;
    let params: Vec<i64> = ids.iter().map(|id| id.to_sql()).collect();
    let rows = stmt.query_map(rusqlite::params_from_iter(params.iter()), map_row)?;
    let mut out = Vec::with_capacity(ids.len());
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Number of non-stub users known to the store.
pub fn count(conn: &Connection) -> StoreResult<i64> {
    Ok(
        conn.query_row("SELECT COUNT(*) FROM users WHERE is_stub = 0", [], |r| {
            r.get(0)
        })?,
    )
}

/// Search users by username/display name using FTS5, best match first
/// (lowest bm25 first).
pub fn search_by_name(
    conn: &Connection,
    query: &str,
    limit: u32,
) -> StoreResult<Vec<(UserRecord, f64)>> {
    let Some(expr) = match_expr(query, FtsMode::All, true) else {
        return Ok(Vec::new());
    };
    let sql = "SELECT u.id, u.username, u.global_name, u.avatar_url, u.is_bot, u.is_provisional, \
             u.is_stub, u.presence_status, u.activity_name, u.activity_details, u.activity_state, \
             u.origin, u.observed_at, u.revision, bm25(users_fts) AS score
         FROM users_fts
         JOIN users u ON u.id = users_fts.rowid
         WHERE users_fts MATCH ?1
         ORDER BY score ASC
         LIMIT ?2";
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(params![expr, limit], |row| {
        let record = map_row(row)?;
        let score: f64 = row.get(14)?;
        Ok((record, score))
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;

    fn user(id: u64, name: &str) -> User {
        User {
            id: UserId(id),
            username: name.into(),
            global_name: None,
            avatar_url: None,
            is_bot: false,
            is_provisional: false,
        }
    }

    #[test]
    fn upsert_then_reobserve_is_noop() {
        let db = Database::open_in_memory().unwrap();
        let u = user(1, "ada");
        let c1 = db
            .write(|tx| upsert(tx, &u, Origin::Synthetic, Timestamp::from_millis(1)))
            .unwrap();
        assert!(c1.changed());
        let c2 = db
            .write(|tx| upsert(tx, &u, Origin::Synthetic, Timestamp::from_millis(2)))
            .unwrap();
        assert!(!c2.changed());
    }

    #[test]
    fn ensure_stub_then_upsert_clears_flag() {
        let db = Database::open_in_memory().unwrap();
        db.write(|tx| ensure_stub(tx, UserId(1), Origin::Synthetic, Timestamp::from_millis(1)))
            .unwrap();
        let rec = db.read(|r| get(r, UserId(1))).unwrap().unwrap();
        assert!(rec.is_stub);
        db.write(|tx| {
            upsert(
                tx,
                &user(1, "ada"),
                Origin::Synthetic,
                Timestamp::from_millis(2),
            )
        })
        .unwrap();
        let rec = db.read(|r| get(r, UserId(1))).unwrap().unwrap();
        assert!(!rec.is_stub);
    }

    #[test]
    fn search_by_name_finds_users() {
        let db = Database::open_in_memory().unwrap();
        db.write(|tx| {
            upsert(
                tx,
                &user(1, "gandalf"),
                Origin::Synthetic,
                Timestamp::from_millis(1),
            )
        })
        .unwrap();
        let hits = db.read(|r| search_by_name(r, "gandalf", 10)).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0.user.id, UserId(1));
    }
}
