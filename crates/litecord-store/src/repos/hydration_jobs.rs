//! Pending hydration work, persisted so it survives a restart. No unified
//! events are emitted from this module.

use rusqlite::params;

use litecord_core::events::HydrationKey;
use litecord_types::Timestamp;

use crate::db::WriteTx;
use crate::error::StoreResult;

/// A hydration job as persisted for restart recovery.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct PersistedJob {
    pub key: HydrationKey,
    pub priority: i32,
    pub reason: String,
    pub requested_at: Timestamp,
    pub attempts: u32,
    pub next_attempt_at: Timestamp,
}

/// Insert or update a job. No-op (no revision churn) if nothing changed.
pub fn upsert(tx: &WriteTx<'_>, job: &PersistedJob) -> StoreResult<bool> {
    let n = tx.execute(
        "INSERT INTO hydration_jobs (key, priority, reason, requested_at, attempts, next_attempt_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(key) DO UPDATE SET
             priority = excluded.priority,
             reason = excluded.reason,
             requested_at = excluded.requested_at,
             attempts = excluded.attempts,
             next_attempt_at = excluded.next_attempt_at
         WHERE hydration_jobs.priority IS NOT excluded.priority
            OR hydration_jobs.reason IS NOT excluded.reason
            OR hydration_jobs.requested_at IS NOT excluded.requested_at
            OR hydration_jobs.attempts IS NOT excluded.attempts
            OR hydration_jobs.next_attempt_at IS NOT excluded.next_attempt_at",
        params![
            job.key.storage_key(),
            job.priority,
            job.reason,
            job.requested_at.as_millis(),
            job.attempts,
            job.next_attempt_at.as_millis(),
        ],
    )?;
    Ok(n > 0)
}

/// Remove a job. Returns whether a row was actually removed.
pub fn remove(tx: &WriteTx<'_>, key: &HydrationKey) -> StoreResult<bool> {
    let n = tx.execute(
        "DELETE FROM hydration_jobs WHERE key = ?1",
        params![key.storage_key()],
    )?;
    Ok(n > 0)
}

/// All persisted jobs, highest priority first, then soonest next attempt.
/// Rows with an unparseable key are skipped.
pub fn list(conn: &rusqlite::Connection) -> StoreResult<Vec<PersistedJob>> {
    let mut stmt = conn.prepare(
        "SELECT key, priority, reason, requested_at, attempts, next_attempt_at
         FROM hydration_jobs
         ORDER BY priority DESC, next_attempt_at ASC",
    )?;
    let mut rows = stmt.query([])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        let key_s: String = row.get(0)?;
        let Some(key) = HydrationKey::parse_storage_key(&key_s) else {
            continue;
        };
        out.push(PersistedJob {
            key,
            priority: row.get(1)?,
            reason: row.get(2)?,
            requested_at: Timestamp::from_millis(row.get(3)?),
            attempts: row.get::<_, i64>(4)? as u32,
            next_attempt_at: Timestamp::from_millis(row.get(5)?),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;

    fn job(key: HydrationKey, priority: i32) -> PersistedJob {
        PersistedJob {
            key,
            priority,
            reason: "test".into(),
            requested_at: Timestamp::from_millis(1),
            attempts: 0,
            next_attempt_at: Timestamp::from_millis(1),
        }
    }

    #[test]
    fn upsert_list_remove_roundtrip() {
        let db = Database::open_in_memory().unwrap();
        db.write(|tx| upsert(tx, &job(HydrationKey::Guilds, 5)))
            .unwrap();
        db.write(|tx| upsert(tx, &job(HydrationKey::CurrentUser, 10)))
            .unwrap();
        let jobs = db.read(|r| list(r)).unwrap();
        assert_eq!(jobs.len(), 2);
        assert_eq!(jobs[0].key, HydrationKey::CurrentUser);

        let removed = db.write(|tx| remove(tx, &HydrationKey::Guilds)).unwrap();
        assert!(removed.value);
        let jobs = db.read(|r| list(r)).unwrap();
        assert_eq!(jobs.len(), 1);
    }
}
