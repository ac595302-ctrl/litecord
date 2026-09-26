//! Embedded, forward-only schema migrations.
//!
//! SQL lives in the repository-level `migrations/` directory. Each migration
//! runs in its own transaction and is recorded in `schema_migrations`.
//! Never edit an applied migration; add a new one.

use rusqlite::Connection;

use crate::error::{StoreError, StoreResult};

pub struct Migration {
    pub version: u32,
    pub name: &'static str,
    pub sql: &'static str,
}

impl std::fmt::Debug for Migration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Migration({} {})", self.version, self.name)
    }
}

pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "initial",
        sql: include_str!("../../../migrations/0001_initial.sql"),
    },
    Migration {
        version: 2,
        name: "task_details",
        sql: include_str!("../../../migrations/0002_task_details.sql"),
    },
];

pub fn latest_version() -> u32 {
    MIGRATIONS.iter().map(|m| m.version).max().unwrap_or(0)
}

pub fn current_version(conn: &Connection) -> StoreResult<u32> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version    INTEGER PRIMARY KEY,
            name       TEXT    NOT NULL,
            applied_at INTEGER NOT NULL
        );",
    )?;
    let v: Option<u32> = conn.query_row("SELECT MAX(version) FROM schema_migrations", [], |r| {
        r.get(0)
    })?;
    Ok(v.unwrap_or(0))
}

/// Apply all pending migrations. Returns the versions applied.
pub fn run(conn: &mut Connection) -> StoreResult<Vec<u32>> {
    let current = current_version(conn)?;
    let mut applied = Vec::new();
    for m in MIGRATIONS.iter().filter(|m| m.version > current) {
        let _span = tracing::info_span!("migration", version = m.version, name = m.name).entered();
        let tx = conn.transaction()?;
        tx.execute_batch(m.sql).map_err(|e| StoreError::Migration {
            version: m.version,
            reason: e.to_string(),
        })?;
        tx.execute(
            "INSERT INTO schema_migrations (version, name, applied_at) VALUES (?1, ?2, ?3)",
            rusqlite::params![
                m.version,
                m.name,
                litecord_types::Timestamp::now().as_millis()
            ],
        )?;
        tx.commit()?;
        applied.push(m.version);
    }
    Ok(applied)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// A database created with only migration 1 applied (as a real v1
    /// deployment would have) upgrades cleanly, and existing rows pick up
    /// migration 2's new `priority` column at its default.
    #[test]
    fn upgrade_from_v1_defaults_existing_tasks_to_normal_priority() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(MIGRATIONS[0].sql).unwrap();
        conn.execute_batch(
            "CREATE TABLE schema_migrations (
                 version    INTEGER PRIMARY KEY,
                 name       TEXT    NOT NULL,
                 applied_at INTEGER NOT NULL
             );
             INSERT INTO schema_migrations (version, name, applied_at) VALUES (1, 'initial', 0);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tasks \
                (title, description, status, origin, source_entity, source_note, \
                 conversation_id, due_at, created_at, updated_at, completed_at, revision) \
             VALUES ('a v1 task', NULL, 'open', 'user_provided', NULL, NULL, NULL, NULL, 0, 0, NULL, 0)",
            [],
        )
        .unwrap();

        let applied = run(&mut conn).unwrap();
        assert_eq!(applied, vec![2]);
        assert_eq!(current_version(&conn).unwrap(), 2);

        let priority: String = conn
            .query_row(
                "SELECT priority FROM tasks WHERE title = 'a v1 task'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(priority, "normal");

        let parent_id: Option<i64> = conn
            .query_row(
                "SELECT parent_id FROM tasks WHERE title = 'a v1 task'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(parent_id, None);
    }
}
