//! Hydration bookkeeping: what's known, what's stale, what last failed.
//! No unified events are emitted from this module.

use rusqlite::{params, Connection, OptionalExtension};

use litecord_core::events::HydrationKey;
use litecord_types::{DurationMs, Revision, Timestamp};

use crate::db::WriteTx;
use crate::error::StoreResult;
use crate::sql::ts;

/// One row of the `sync_state` table.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct SyncStateRecord {
    pub key: HydrationKey,
    pub observed_at: Option<Timestamp>,
    pub stale_after: DurationMs,
    pub revision: Revision,
    pub dirty: bool,
    pub failure_count: u32,
    pub last_error: Option<String>,
}

fn map_row(
    key: HydrationKey,
    row: &rusqlite::Row<'_>,
    offset: usize,
) -> rusqlite::Result<SyncStateRecord> {
    let observed_at = ts(row.get(offset)?);
    let stale_after = DurationMs::from_millis(row.get::<_, i64>(offset + 1)? as u64);
    let revision = crate::sql::rev(row.get(offset + 2)?);
    let dirty: bool = row.get::<_, i64>(offset + 3)? != 0;
    let failure_count: u32 = row.get::<_, i64>(offset + 4)? as u32;
    let last_error: Option<String> = row.get(offset + 5)?;
    Ok(SyncStateRecord {
        key,
        observed_at,
        stale_after,
        revision,
        dirty,
        failure_count,
        last_error,
    })
}

/// Look up the sync state for one hydration key.
pub fn get(conn: &Connection, key: &HydrationKey) -> StoreResult<Option<SyncStateRecord>> {
    let storage_key = key.storage_key();
    Ok(conn
        .query_row(
            "SELECT observed_at, stale_after_ms, revision, dirty, failure_count, last_error
             FROM sync_state WHERE key = ?1",
            params![storage_key],
            |r| map_row(*key, r, 0),
        )
        .optional()?)
}

/// All sync-state rows whose key parses as a [`HydrationKey`]. Rows with an
/// unparseable key (e.g. left over from a removed key shape) are skipped.
pub fn list(conn: &Connection) -> StoreResult<Vec<SyncStateRecord>> {
    let mut stmt = conn.prepare(
        "SELECT key, observed_at, stale_after_ms, revision, dirty, failure_count, last_error FROM sync_state",
    )?;
    let mut rows = stmt.query([])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        let key_s: String = row.get(0)?;
        let Some(key) = HydrationKey::parse_storage_key(&key_s) else {
            continue;
        };
        out.push(map_row(key, row, 1)?);
    }
    Ok(out)
}

/// Mark a key fresh: clears `dirty`, `failure_count` and `last_error`.
pub fn mark_fresh(
    tx: &WriteTx<'_>,
    key: &HydrationKey,
    observed_at: Timestamp,
    stale_after: DurationMs,
) -> StoreResult<bool> {
    let n = tx.execute(
        "INSERT INTO sync_state (key, observed_at, stale_after_ms, revision, dirty, failure_count, last_error)
         VALUES (?1, ?2, ?3, ?4, 0, 0, NULL)
         ON CONFLICT(key) DO UPDATE SET
             observed_at = excluded.observed_at,
             stale_after_ms = excluded.stale_after_ms,
             revision = excluded.revision,
             dirty = 0,
             failure_count = 0,
             last_error = NULL
         WHERE sync_state.observed_at IS NOT excluded.observed_at
            OR sync_state.stale_after_ms IS NOT excluded.stale_after_ms
            OR sync_state.dirty IS NOT 0
            OR sync_state.failure_count IS NOT 0
            OR sync_state.last_error IS NOT NULL",
        params![
            key.storage_key(),
            observed_at.as_millis(),
            stale_after.as_millis() as i64,
            tx.revision().get() as i64,
        ],
    )?;
    Ok(n > 0)
}

/// Mark a key dirty, inserting a fresh row (with `default_stale_after`) if it
/// did not exist yet.
pub fn mark_dirty(
    tx: &WriteTx<'_>,
    key: &HydrationKey,
    default_stale_after: DurationMs,
) -> StoreResult<bool> {
    let n = tx.execute(
        "INSERT INTO sync_state (key, observed_at, stale_after_ms, revision, dirty, failure_count, last_error)
         VALUES (?1, NULL, ?2, ?3, 1, 0, NULL)
         ON CONFLICT(key) DO UPDATE SET dirty = 1, revision = excluded.revision
         WHERE sync_state.dirty IS NOT 1",
        params![
            key.storage_key(),
            default_stale_after.as_millis() as i64,
            tx.revision().get() as i64,
        ],
    )?;
    Ok(n > 0)
}

/// Mark every key dirty. Returns the number of rows changed.
pub fn mark_all_dirty(tx: &WriteTx<'_>) -> StoreResult<usize> {
    Ok(tx.execute(
        "UPDATE sync_state SET dirty = 1, revision = ?1 WHERE dirty = 0",
        params![tx.revision().get() as i64],
    )?)
}

/// Record a hydration failure: increments `failure_count`, replaces
/// `last_error` (truncated to 200 characters) and marks the key dirty,
/// inserting a row if it did not exist yet.
pub fn record_failure(
    tx: &WriteTx<'_>,
    key: &HydrationKey,
    error: &str,
    default_stale_after: DurationMs,
) -> StoreResult<bool> {
    let truncated: String = error.chars().take(200).collect();
    let n = tx.execute(
        "INSERT INTO sync_state (key, observed_at, stale_after_ms, revision, dirty, failure_count, last_error)
         VALUES (?1, NULL, ?2, ?3, 1, 1, ?4)
         ON CONFLICT(key) DO UPDATE SET
             dirty = 1,
             failure_count = sync_state.failure_count + 1,
             last_error = excluded.last_error,
             revision = excluded.revision",
        params![
            key.storage_key(),
            default_stale_after.as_millis() as i64,
            tx.revision().get() as i64,
            truncated,
        ],
    )?;
    Ok(n > 0)
}

/// Keep a nonretryable error visible while delaying another background fetch.
pub fn record_terminal_failure(
    tx: &WriteTx<'_>,
    key: &HydrationKey,
    error: &str,
    at: Timestamp,
    stale_after: DurationMs,
) -> StoreResult<bool> {
    let truncated: String = error.chars().take(200).collect();
    let n = tx.execute(
        "INSERT INTO sync_state (key, observed_at, stale_after_ms, revision, dirty, failure_count, last_error)
         VALUES (?1, ?2, ?3, ?4, 0, 1, ?5)
         ON CONFLICT(key) DO UPDATE SET
             observed_at = excluded.observed_at,
             stale_after_ms = excluded.stale_after_ms,
             revision = excluded.revision,
             dirty = 0,
             failure_count = sync_state.failure_count + 1,
             last_error = excluded.last_error",
        params![
            key.storage_key(),
            at.as_millis(),
            stale_after.as_millis() as i64,
            tx.revision().get() as i64,
            truncated,
        ],
    )?;
    Ok(n > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;

    #[test]
    fn dirty_fresh_and_failure_roundtrip() {
        let db = Database::open_in_memory().unwrap();
        let key = HydrationKey::CurrentUser;

        db.write(|tx| mark_dirty(tx, &key, DurationMs::from_secs(60)))
            .unwrap();
        let rec = db.read(|r| get(r, &key)).unwrap().unwrap();
        assert!(rec.dirty);

        db.write(|tx| {
            mark_fresh(
                tx,
                &key,
                Timestamp::from_millis(1),
                DurationMs::from_secs(30),
            )
        })
        .unwrap();
        let rec = db.read(|r| get(r, &key)).unwrap().unwrap();
        assert!(!rec.dirty);
        assert_eq!(rec.observed_at, Some(Timestamp::from_millis(1)));

        db.write(|tx| record_failure(tx, &key, "boom", DurationMs::from_secs(60)))
            .unwrap();
        let rec = db.read(|r| get(r, &key)).unwrap().unwrap();
        assert!(rec.dirty);
        assert_eq!(rec.failure_count, 1);
        assert_eq!(rec.last_error.as_deref(), Some("boom"));
    }

    #[test]
    fn list_skips_unparseable_keys() {
        let db = Database::open_in_memory().unwrap();
        db.write(|tx| {
            tx.execute(
                "INSERT INTO sync_state (key, stale_after_ms, revision, dirty, failure_count) VALUES ('bogus:key:shape', 0, 1, 0, 0)",
                [],
            )?;
            Ok::<_, crate::error::StoreError>(())
        })
        .unwrap();
        db.write(|tx| mark_dirty(tx, &HydrationKey::Guilds, DurationMs::from_secs(1)))
            .unwrap();
        let all = db.read(|r| list(r)).unwrap();
        assert_eq!(all.len(), 1);
    }
}
