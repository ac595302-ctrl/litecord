//! Restart recovery for hydration work.
//!
//! On shutdown the unfinished hydration queue (pending + interrupted jobs) is
//! written to `hydration_jobs`; on the next start it is re-requested before
//! the normal initial hydration and the table is cleared. Staleness-based
//! reconciliation would eventually redo the work anyway; persisting it makes
//! recovery immediate and keeps explicit/agent requests from being lost.

use litecord_core::Result;
use litecord_hydrator::{HydrationReason, HydrationRequest, Hydrator, Priority};
use litecord_store::repos::hydration_jobs::{self, PersistedJob};
use litecord_store::Database;

/// Persist the hydrator's unfinished work, replacing any previous snapshot.
pub(crate) fn persist(db: &Database, hydrator: &Hydrator) -> Result<usize> {
    let jobs = hydrator.unfinished_snapshot();
    let now = db.now();
    db.write(|tx| -> Result<()> {
        for old in hydration_jobs::list(tx)? {
            hydration_jobs::remove(tx, &old.key)?;
        }
        for (key, priority) in &jobs {
            hydration_jobs::upsert(
                tx,
                &PersistedJob {
                    key: *key,
                    priority: priority.as_i32(),
                    reason: "restart_recovery".into(),
                    requested_at: now,
                    attempts: 0,
                    next_attempt_at: now,
                },
            )?;
        }
        Ok(())
    })?;
    Ok(jobs.len())
}

/// Re-request persisted work and clear the table. Returns how many jobs
/// were restored.
pub(crate) fn restore(db: &Database, hydrator: &Hydrator) -> Result<usize> {
    let jobs = db.read(|r| hydration_jobs::list(r))?;
    for job in &jobs {
        hydrator.request(HydrationRequest::new(
            job.key,
            Priority::from_i32(job.priority),
            HydrationReason::Startup,
        ));
    }
    if !jobs.is_empty() {
        db.write(|tx| -> Result<()> {
            for job in &jobs {
                hydration_jobs::remove(tx, &job.key)?;
            }
            Ok(())
        })?;
        tracing::info!(restored = jobs.len(), "restored hydration queue");
    }
    Ok(jobs.len())
}
