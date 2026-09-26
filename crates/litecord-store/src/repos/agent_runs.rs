//! Bookkeeping for agent/harness runs: when a run started, what it knew
//! (`as_of_revision`), and how it finished. Used for the UI's "what has the
//! agent been doing" view and for debugging context compilation.

use rusqlite::{params, Connection, OptionalExtension, Row};

use litecord_core::events::UnifiedEvent;
use litecord_types::provenance::Origin;
use litecord_types::{AgentRunId, Revision, Timestamp};

use crate::db::WriteTx;
use crate::error::StoreResult;
use crate::sql::{rev, ts};

const SELECT_COLUMNS: &str =
    "id, harness, request_summary, status, as_of_revision, context_items, \
     tokens_estimated, error, started_at, finished_at";

/// A recorded agent run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRunRecord {
    pub id: AgentRunId,
    pub harness: String,
    pub request_summary: String,
    pub status: String,
    pub as_of_revision: Revision,
    pub context_items: u32,
    pub tokens_estimated: u32,
    pub error: Option<String>,
    pub started_at: Timestamp,
    pub finished_at: Option<Timestamp>,
}

fn row_to_run(row: &Row<'_>) -> rusqlite::Result<AgentRunRecord> {
    let id = AgentRunId(row.get(0)?);
    let harness: String = row.get(1)?;
    let request_summary: String = row.get(2)?;
    let status: String = row.get(3)?;
    let as_of_revision: i64 = row.get(4)?;
    let context_items: i64 = row.get(5)?;
    let tokens_estimated: i64 = row.get(6)?;
    let error: Option<String> = row.get(7)?;
    let started_at: i64 = row.get(8)?;
    let finished_at: Option<i64> = row.get(9)?;
    Ok(AgentRunRecord {
        id,
        harness,
        request_summary,
        status,
        as_of_revision: rev(as_of_revision),
        context_items: context_items.max(0) as u32,
        tokens_estimated: tokens_estimated.max(0) as u32,
        error,
        started_at: Timestamp::from_millis(started_at),
        finished_at: ts(finished_at),
    })
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect()
    }
}

/// Start a run in `running` status. `request_summary` is truncated to 500
/// characters. Emits [`UnifiedEvent::AgentRunRecorded`] with
/// [`Origin::AgentDerived`].
pub fn start(
    tx: &WriteTx<'_>,
    harness: &str,
    request_summary: &str,
    as_of_revision: Revision,
) -> StoreResult<AgentRunId> {
    let summary = truncate_chars(request_summary, 500);
    tx.execute(
        "INSERT INTO agent_runs \
            (harness, request_summary, status, as_of_revision, context_items, tokens_estimated, \
             error, started_at, finished_at) \
         VALUES (?, ?, 'running', ?, 0, 0, NULL, ?, NULL)",
        params![
            harness,
            summary,
            as_of_revision.get() as i64,
            tx.now().as_millis()
        ],
    )?;
    let id = AgentRunId(tx.last_insert_rowid());
    tx.emit(
        UnifiedEvent::AgentRunRecorded { run_id: id },
        Origin::AgentDerived,
    )?;
    Ok(id)
}

/// Mark a run finished (only if it had not already finished). No event: the
/// run's completion is surfaced through its own `AgentRunRecord`, not a
/// separate `UnifiedEvent`.
pub fn finish(
    tx: &WriteTx<'_>,
    id: AgentRunId,
    succeeded: bool,
    context_items: u32,
    tokens_estimated: u32,
    error: Option<&str>,
) -> StoreResult<bool> {
    let status = if succeeded { "succeeded" } else { "failed" };
    let n = tx.execute(
        "UPDATE agent_runs SET status = ?, context_items = ?, tokens_estimated = ?, error = ?, finished_at = ? \
         WHERE id = ? AND finished_at IS NULL",
        params![status, context_items, tokens_estimated, error, tx.now().as_millis(), id.get()],
    )?;
    Ok(n > 0)
}

/// Load a run by id.
pub fn get(conn: &Connection, id: AgentRunId) -> StoreResult<Option<AgentRunRecord>> {
    let sql = format!("SELECT {SELECT_COLUMNS} FROM agent_runs WHERE id = ?");
    Ok(conn
        .query_row(&sql, params![id.get()], row_to_run)
        .optional()?)
}

/// The most recently started runs. `limit = 0` means unlimited.
pub fn recent(conn: &Connection, limit: u32) -> StoreResult<Vec<AgentRunRecord>> {
    let mut sql = format!("SELECT {SELECT_COLUMNS} FROM agent_runs ORDER BY started_at DESC");
    if limit > 0 {
        sql.push_str(" LIMIT ?");
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![limit], row_to_run)?;
        return Ok(rows.collect::<Result<Vec<_>, _>>()?);
    }
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], row_to_run)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Delete runs started before `before`. Returns how many were removed.
pub fn prune_before(tx: &WriteTx<'_>, before: Timestamp) -> StoreResult<usize> {
    Ok(tx.execute(
        "DELETE FROM agent_runs WHERE started_at < ?",
        params![before.as_millis()],
    )?)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::db::Database;

    #[test]
    fn start_finish_and_prune() {
        let db = Database::open_in_memory().unwrap();
        let id = db
            .write(|tx| start(tx, "cli", "summarize channel", Revision(5)))
            .unwrap()
            .value;
        let run = db.read(|r| get(r, id)).unwrap().unwrap();
        assert_eq!(run.status, "running");

        let changed = db
            .write(|tx| finish(tx, id, true, 3, 120, None))
            .unwrap()
            .value;
        assert!(changed);
        let run = db.read(|r| get(r, id)).unwrap().unwrap();
        assert_eq!(run.status, "succeeded");
        assert!(run.finished_at.is_some());

        let again = db
            .write(|tx| finish(tx, id, false, 0, 0, Some("late")))
            .unwrap()
            .value;
        assert!(!again, "finishing an already-finished run is a no-op");

        assert_eq!(db.read(|r| recent(r, 10)).unwrap().len(), 1);
        let pruned = db
            .write(|tx| prune_before(tx, Timestamp::from_millis(i64::MAX)))
            .unwrap()
            .value;
        assert_eq!(pruned, 1);
    }

    #[test]
    fn request_summary_is_truncated() {
        let db = Database::open_in_memory().unwrap();
        let long = "x".repeat(600);
        let id = db
            .write(|tx| start(tx, "cli", &long, Revision(1)))
            .unwrap()
            .value;
        let run = db.read(|r| get(r, id)).unwrap().unwrap();
        assert_eq!(run.request_summary.chars().count(), 500);
    }
}
