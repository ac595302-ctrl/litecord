//! SQLite-backed [`FreshnessStore`] over the `sync_state` table, so hydration
//! freshness survives restarts and is shared with other processes.

use litecord_core::events::HydrationKey;
use litecord_hydrator::error::HydrationError;
use litecord_hydrator::freshness::{Freshness, FreshnessStore};
use litecord_store::repos::sync_state::{self, SyncStateRecord};
use litecord_store::{Database, StoreError};
use litecord_types::{DurationMs, Timestamp};

#[derive(Debug, Clone)]
pub struct StoreFreshness {
    db: Database,
}

impl StoreFreshness {
    pub fn new(db: Database) -> Self {
        Self { db }
    }
}

fn err(e: StoreError) -> HydrationError {
    HydrationError::Store(e.to_string())
}

fn to_freshness(r: &SyncStateRecord) -> Freshness {
    Freshness {
        observed_at: r.observed_at,
        stale_after: r.stale_after,
        dirty: r.dirty,
        failure_count: r.failure_count,
    }
}

impl FreshnessStore for StoreFreshness {
    fn get(&self, key: &HydrationKey) -> Result<Option<Freshness>, HydrationError> {
        let rec = self.db.read(|r| sync_state::get(r, key)).map_err(err)?;
        Ok(rec.as_ref().map(to_freshness))
    }

    fn list(&self) -> Result<Vec<(HydrationKey, Freshness)>, HydrationError> {
        let recs = self.db.read(|r| sync_state::list(r)).map_err(err)?;
        Ok(recs.iter().map(|r| (r.key, to_freshness(r))).collect())
    }

    fn mark_fresh(
        &self,
        key: &HydrationKey,
        at: Timestamp,
        stale_after: DurationMs,
    ) -> Result<(), HydrationError> {
        self.db
            .write(|tx| sync_state::mark_fresh(tx, key, at, stale_after))
            .map_err(err)?;
        Ok(())
    }

    fn mark_dirty(
        &self,
        key: &HydrationKey,
        stale_after: DurationMs,
    ) -> Result<(), HydrationError> {
        self.db
            .write(|tx| sync_state::mark_dirty(tx, key, stale_after))
            .map_err(err)?;
        Ok(())
    }

    fn mark_all_dirty(&self) -> Result<(), HydrationError> {
        self.db.write(sync_state::mark_all_dirty).map_err(err)?;
        Ok(())
    }

    fn record_failure(
        &self,
        key: &HydrationKey,
        error: &str,
        stale_after: DurationMs,
    ) -> Result<(), HydrationError> {
        self.db
            .write(|tx| sync_state::record_failure(tx, key, error, stale_after))
            .map_err(err)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_through_sqlite() {
        let db = Database::open_in_memory().unwrap();
        let s = StoreFreshness::new(db);
        let key = HydrationKey::Relationships;
        assert!(s.get(&key).unwrap().is_none());
        s.mark_fresh(&key, Timestamp(1_000), DurationMs::from_secs(60))
            .unwrap();
        let f = s.get(&key).unwrap().unwrap();
        assert!(!f.is_stale(Timestamp(2_000)));
        assert!(f.is_stale(Timestamp(62_000)));
        s.mark_all_dirty().unwrap();
        assert!(s.get(&key).unwrap().unwrap().dirty);
        assert_eq!(s.list().unwrap().len(), 1);
    }
}
