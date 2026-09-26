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
    Migration {
        version: 3,
        name: "omni",
        sql: include_str!("../../../migrations/0003_omni.sql"),
    },
    Migration {
        version: 4,
        name: "omni_automations",
        sql: include_str!("../../../migrations/0004_omni_automations.sql"),
    },
    Migration {
        version: 5,
        name: "history_sync",
        sql: include_str!("../../../migrations/0005_history_sync.sql"),
    },
    Migration {
        version: 6,
        name: "message_tombstones",
        sql: include_str!("../../../migrations/0006_message_tombstones.sql"),
    },
    Migration {
        version: 7,
        name: "source_memberships",
        sql: include_str!("../../../migrations/0007_source_memberships.sql"),
    },
    Migration {
        version: 8,
        name: "account_recovery",
        sql: include_str!("../../../migrations/0008_account_recovery.sql"),
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
        assert_eq!(applied, vec![2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(current_version(&conn).unwrap(), latest_version());

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

    /// A v4 database gains history, message tombstones and source membership
    /// tables, including backfills for existing canonical rows.
    #[test]
    fn upgrade_from_v4_adds_history_sync() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE schema_migrations (
                 version    INTEGER PRIMARY KEY,
                 name       TEXT    NOT NULL,
                 applied_at INTEGER NOT NULL
             );",
        )
        .unwrap();
        for m in MIGRATIONS.iter().filter(|m| m.version <= 4) {
            conn.execute_batch(m.sql).unwrap();
            conn.execute(
                "INSERT INTO schema_migrations (version, name, applied_at) VALUES (?1, ?2, 0)",
                rusqlite::params![m.version, m.name],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO conversations (id, kind, origin, observed_at, revision)
             VALUES (10, 'dm', 'synthetic', 0, 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO messages
                (id, conversation_id, author_id, content, sent_at, deleted, origin, observed_at, revision)
             VALUES (99, 10, 20, '', 1, 1, 'synthetic', 2, 3)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO guilds (id, name, departed, origin, observed_at, revision)
             VALUES (20, 'guild', 0, 'discord_user_session', 0, 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO channels
                (id, guild_id, name, kind, position, access, capabilities, removed, origin, observed_at, revision)
             VALUES (200, 20, 'general', 'text', 0, 'native', 1, 0, 'discord_bot_gateway', 0, 0)",
            [],
        )
        .unwrap();
        assert_eq!(run(&mut conn).unwrap(), vec![5, 6, 7, 8]);
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM history_sync", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
        let tombstones: i64 = conn
            .query_row("SELECT COUNT(*) FROM message_tombstones", [], |r| r.get(0))
            .unwrap();
        assert_eq!(tombstones, 1);
        let tombstone_conversation: i64 = conn
            .query_row(
                "SELECT conversation_id FROM message_tombstones WHERE message_id = 99",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(tombstone_conversation, 10);
        let guild_membership: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM guild_source_memberships
                 WHERE source = 'discord_user_session' AND guild_id = 20",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(guild_membership, 1);
        let channel_guild_membership: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM guild_source_memberships
                 WHERE source = 'discord_bot_gateway' AND guild_id = 20",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(channel_guild_membership, 1);
        let channel_membership: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM channel_source_memberships
                 WHERE source = 'discord_bot_gateway' AND channel_id = 200",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(channel_membership, 1);
    }
}
