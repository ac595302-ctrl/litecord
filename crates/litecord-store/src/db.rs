use std::cell::RefCell;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use rusqlite::{Connection, OpenFlags, Transaction, TransactionBehavior};

use litecord_core::clock::{SharedClock, SystemClock};
use litecord_core::config::DatabaseConfig;
use litecord_core::events::UnifiedEvent;
use litecord_core::metrics::Metrics;
use litecord_types::provenance::Origin;
use litecord_types::{MemorySnapshot, Revision, Timestamp};

use crate::error::{StoreError, StoreResult};
use crate::migrations;

/// Handle to the Litecord database. Cheap to clone; share freely.
///
/// Thread-safety: one writer connection behind a mutex (SQLite allows one
/// writer at a time anyway) plus a small pool of read-only connections. With
/// WAL, readers never block the writer. Calls are synchronous and fast;
/// async callers doing heavy scans should use `spawn_blocking`.
#[derive(Clone)]
pub struct Database {
    inner: Arc<Inner>,
}

struct Inner {
    writer: Mutex<Connection>,
    readers: Mutex<Vec<Connection>>,
    path: Option<PathBuf>,
    clock: SharedClock,
    metrics: Option<Arc<Metrics>>,
}

impl std::fmt::Debug for Database {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Database")
            .field("path", &self.inner.path)
            .finish_non_exhaustive()
    }
}

/// Result of a committed write.
#[derive(Debug, Clone, PartialEq)]
pub struct Committed<T> {
    pub value: T,
    /// Revision of this write, or the unchanged current revision if the write
    /// turned out to be a no-op (nothing changed, no events).
    pub revision: Revision,
    /// Unified events appended to the event log by this write.
    pub events: Vec<UnifiedEvent>,
}

impl<T> Committed<T> {
    pub fn changed(&self) -> bool {
        !self.events.is_empty()
    }
}

fn configure(conn: &Connection, cfg: &DatabaseConfig, file_backed: bool) -> StoreResult<()> {
    conn.busy_timeout(std::time::Duration::from_millis(cfg.busy_timeout_ms))?;
    conn.execute_batch(
        "PRAGMA foreign_keys = ON;
         PRAGMA temp_store = MEMORY;",
    )?;
    if file_backed {
        // WAL: concurrent readers with a single writer, good for a responsive
        // desktop app; NORMAL sync is durable across app crashes and only
        // risks the last transactions on power loss.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
    }
    Ok(())
}

impl Database {
    /// Open (creating if needed) a file-backed database and run migrations.
    pub fn open(path: impl AsRef<Path>, cfg: &DatabaseConfig) -> StoreResult<Self> {
        Self::open_with(path, cfg, Arc::new(SystemClock), None)
    }

    pub fn open_with(
        path: impl AsRef<Path>,
        cfg: &DatabaseConfig,
        clock: SharedClock,
        metrics: Option<Arc<Metrics>>,
    ) -> StoreResult<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    StoreError::Invariant(format!("cannot create {}: {e}", parent.display()))
                })?;
            }
        }
        let _span = tracing::info_span!("db_open", path = %path.display()).entered();
        let mut writer = Connection::open(&path)?;
        configure(&writer, cfg, true)?;
        let applied = migrations::run(&mut writer)?;
        if !applied.is_empty() {
            tracing::info!(?applied, "applied migrations");
        }
        let mut readers = Vec::with_capacity(cfg.read_pool_size);
        for _ in 0..cfg.read_pool_size {
            let r = Connection::open_with_flags(
                &path,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )?;
            configure(&r, cfg, false)?;
            readers.push(r);
        }
        Ok(Self {
            inner: Arc::new(Inner {
                writer: Mutex::new(writer),
                readers: Mutex::new(readers),
                path: Some(path),
                clock,
                metrics,
            }),
        })
    }

    /// Private in-memory database (tests, ephemeral tools). Reads share the
    /// writer connection.
    pub fn open_in_memory() -> StoreResult<Self> {
        Self::open_in_memory_with(Arc::new(SystemClock), None)
    }

    pub fn open_in_memory_with(
        clock: SharedClock,
        metrics: Option<Arc<Metrics>>,
    ) -> StoreResult<Self> {
        let mut writer = Connection::open_in_memory()?;
        configure(&writer, &DatabaseConfig::default(), false)?;
        migrations::run(&mut writer)?;
        Ok(Self {
            inner: Arc::new(Inner {
                writer: Mutex::new(writer),
                readers: Mutex::new(Vec::new()),
                path: None,
                clock,
                metrics,
            }),
        })
    }

    pub fn path(&self) -> Option<&Path> {
        self.inner.path.as_deref()
    }

    pub fn clock(&self) -> &SharedClock {
        &self.inner.clock
    }

    pub fn now(&self) -> Timestamp {
        self.inner.clock.now()
    }

    fn writer(&self) -> StoreResult<MutexGuard<'_, Connection>> {
        self.inner.writer.lock().map_err(|_| StoreError::Poisoned)
    }

    /// Current global revision.
    pub fn current_revision(&self) -> StoreResult<Revision> {
        self.read(|r| Ok::<_, StoreError>(r.revision()))
    }

    /// Run `f` in a single write transaction that advances the revision.
    ///
    /// * `f` returning `Err` rolls everything back (revision included).
    /// * If `f` changed no rows and emitted no events, the transaction is
    ///   rolled back and the current revision is returned unchanged, so
    ///   idempotent upserts do not churn revisions.
    pub fn write<T, E>(
        &self,
        f: impl FnOnce(&WriteTx<'_>) -> Result<T, E>,
    ) -> Result<Committed<T>, E>
    where
        E: From<StoreError>,
    {
        let start = Instant::now();
        let mut conn = self.writer()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::from)?;
        let rev: i64 = tx
            .query_row(
                "UPDATE revision_counter SET revision = revision + 1 WHERE id = 1 RETURNING revision",
                [],
                |r| r.get(0),
            )
            .map_err(StoreError::from)?;
        let base_changes = total_changes(&tx)?;
        let wtx = WriteTx {
            tx,
            revision: Revision(rev as u64),
            now: self.inner.clock.now(),
            events: RefCell::new(Vec::new()),
        };
        let value = f(&wtx)?;
        let changed = total_changes(&wtx.tx)? > base_changes;
        let WriteTx { tx, events, .. } = wtx;
        let events = events.into_inner();
        let committed = if !changed && events.is_empty() {
            tx.rollback().map_err(StoreError::from)?;
            Committed {
                value,
                revision: Revision((rev - 1).max(0) as u64),
                events,
            }
        } else {
            tx.commit().map_err(StoreError::from)?;
            Committed {
                value,
                revision: Revision(rev as u64),
                events,
            }
        };
        if let Some(m) = &self.inner.metrics {
            m.db_write.record(start.elapsed());
        }
        Ok(committed)
    }

    /// Run `f` inside a consistent read snapshot.
    pub fn read<T, E>(&self, f: impl FnOnce(&ReadTx<'_>) -> Result<T, E>) -> Result<T, E>
    where
        E: From<StoreError>,
    {
        let start = Instant::now();
        let pooled = {
            let mut readers = self
                .inner
                .readers
                .lock()
                .map_err(|_| StoreError::Poisoned)?;
            readers.pop()
        };
        let out = match pooled {
            Some(mut conn) => {
                let result = run_read(&mut conn, self.inner.clock.now(), f);
                if let Ok(mut readers) = self.inner.readers.lock() {
                    readers.push(conn);
                }
                result
            }
            None => {
                let mut conn = self.writer()?;
                run_read(&mut conn, self.inner.clock.now(), f)
            }
        };
        if let Some(m) = &self.inner.metrics {
            m.db_read.record(start.elapsed());
        }
        out
    }
}

fn total_changes(conn: &Connection) -> StoreResult<i64> {
    Ok(conn.query_row("SELECT total_changes()", [], |r| r.get(0))?)
}

fn run_read<T, E>(
    conn: &mut Connection,
    now: Timestamp,
    f: impl FnOnce(&ReadTx<'_>) -> Result<T, E>,
) -> Result<T, E>
where
    E: From<StoreError>,
{
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Deferred)
        .map_err(StoreError::from)?;
    // The first SELECT pins the WAL snapshot for the rest of the transaction.
    let rev: i64 = tx
        .query_row(
            "SELECT revision FROM revision_counter WHERE id = 1",
            [],
            |r| r.get(0),
        )
        .map_err(StoreError::from)?;
    let rtx = ReadTx {
        tx,
        snapshot: MemorySnapshot {
            revision: Revision(rev.max(0) as u64),
            created_at: now,
        },
    };
    let out = f(&rtx);
    // Read-only; finishing via rollback is correct and cheap.
    let _ = rtx.tx.rollback();
    out
}

/// A write transaction bound to one revision. Derefs to [`Connection`] so
/// repository functions can take `&Connection` for reads and `&WriteTx` for
/// writes.
pub struct WriteTx<'c> {
    tx: Transaction<'c>,
    revision: Revision,
    now: Timestamp,
    events: RefCell<Vec<UnifiedEvent>>,
}

impl std::fmt::Debug for WriteTx<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WriteTx")
            .field("revision", &self.revision)
            .finish_non_exhaustive()
    }
}

impl WriteTx<'_> {
    /// The revision this transaction will commit as.
    pub fn revision(&self) -> Revision {
        self.revision
    }

    /// Transaction timestamp from the database clock.
    pub fn now(&self) -> Timestamp {
        self.now
    }

    /// Append a unified event to the event log (same transaction).
    pub fn emit(&self, event: UnifiedEvent, source: Origin) -> StoreResult<()> {
        let payload = serde_json::to_string(&event)?;
        self.tx.execute(
            "INSERT INTO events (revision, kind, entity, source, payload, at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                self.revision.get() as i64,
                event.kind(),
                event.entity().map(|e| e.to_string()),
                source.as_str(),
                payload,
                self.now.as_millis(),
            ],
        )?;
        self.events.borrow_mut().push(event);
        Ok(())
    }

    /// Events emitted so far in this transaction.
    pub fn emitted(&self) -> usize {
        self.events.borrow().len()
    }
}

impl Deref for WriteTx<'_> {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        &self.tx
    }
}

/// A consistent read snapshot.
pub struct ReadTx<'c> {
    tx: Transaction<'c>,
    snapshot: MemorySnapshot,
}

impl std::fmt::Debug for ReadTx<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReadTx")
            .field("snapshot", &self.snapshot)
            .finish_non_exhaustive()
    }
}

impl ReadTx<'_> {
    pub fn revision(&self) -> Revision {
        self.snapshot.revision
    }
    pub fn snapshot(&self) -> MemorySnapshot {
        self.snapshot
    }
}

impl Deref for ReadTx<'_> {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        &self.tx
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set_setting(tx: &WriteTx<'_>, key: &str, value: &str) -> StoreResult<()> {
        tx.execute(
            "INSERT INTO settings (key, value, updated_at) VALUES (?1, ?2, 0)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value
             WHERE settings.value IS NOT excluded.value",
            rusqlite::params![key, value],
        )?;
        Ok(())
    }

    #[test]
    fn migrations_apply_and_are_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.db");
        let db = Database::open(&path, &DatabaseConfig::default()).unwrap();
        assert_eq!(db.current_revision().unwrap(), Revision(0));
        drop(db);
        // Re-opening does not re-run migrations.
        let db = Database::open(&path, &DatabaseConfig::default()).unwrap();
        let v = db.read(|r| migrations::current_version(r)).unwrap();
        assert_eq!(v, migrations::latest_version());
    }

    #[test]
    fn writes_advance_revision_by_one() {
        let db = Database::open_in_memory().unwrap();
        let c1 = db.write(|tx| set_setting(tx, "a", "1")).unwrap();
        let c2 = db.write(|tx| set_setting(tx, "b", "2")).unwrap();
        assert_eq!(c1.revision, Revision(1));
        assert_eq!(c2.revision, Revision(2));
        assert_eq!(db.current_revision().unwrap(), Revision(2));
    }

    #[test]
    fn noop_writes_do_not_advance_revision() {
        let db = Database::open_in_memory().unwrap();
        db.write(|tx| set_setting(tx, "a", "1")).unwrap();
        let c = db.write(|tx| set_setting(tx, "a", "1")).unwrap();
        assert!(!c.changed());
        assert_eq!(c.revision, Revision(1));
        assert_eq!(db.current_revision().unwrap(), Revision(1));
    }

    #[test]
    fn failed_writes_roll_back_revision() {
        let db = Database::open_in_memory().unwrap();
        let r: Result<Committed<()>, StoreError> = db.write(|tx| {
            set_setting(tx, "a", "1")?;
            Err(StoreError::Invariant("boom".into()))
        });
        assert!(r.is_err());
        assert_eq!(db.current_revision().unwrap(), Revision(0));
        let n: i64 = db
            .read(|r| {
                r.query_row("SELECT COUNT(*) FROM settings", [], |row| row.get(0))
                    .map_err(StoreError::from)
            })
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn emitted_events_are_logged_with_revision() {
        let db = Database::open_in_memory().unwrap();
        let c = db
            .write(|tx| {
                tx.emit(
                    UnifiedEvent::SettingChanged { key: "x".into() },
                    Origin::UserProvided,
                )
            })
            .unwrap();
        assert_eq!(c.events.len(), 1);
        let (rev, kind): (i64, String) = db
            .read(|r| {
                r.query_row("SELECT revision, kind FROM events", [], |row| {
                    Ok((row.get(0)?, row.get(1)?))
                })
                .map_err(StoreError::from)
            })
            .unwrap();
        assert_eq!(rev, 1);
        assert_eq!(kind, "setting_changed");
    }

    #[test]
    fn file_backed_reads_use_pool_and_see_committed_state() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path().join("p.db"), &DatabaseConfig::default()).unwrap();
        db.write(|tx| set_setting(tx, "k", "v")).unwrap();
        let snap = db.read(|r| Ok::<_, StoreError>(r.snapshot())).unwrap();
        assert_eq!(snap.revision, Revision(1));
    }
}
