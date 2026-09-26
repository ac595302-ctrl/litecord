//! Guilds ("servers") the current user belongs to.

use rusqlite::{params, Connection, OptionalExtension, Row};

use litecord_core::events::UnifiedEvent;
use litecord_types::provenance::Origin;
use litecord_types::social::Guild;
use litecord_types::{GuildId, Revision, Timestamp};

use crate::db::WriteTx;
use crate::error::StoreResult;

/// A stored guild row plus bookkeeping.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct GuildRecord {
    pub guild: Guild,
    pub departed: bool,
    pub origin: Origin,
    pub revision: Revision,
}

const COLUMNS: &str = "id, name, icon_url, departed, origin, observed_at, revision";

fn map_row(row: &Row<'_>) -> rusqlite::Result<GuildRecord> {
    let id = GuildId::from_sql(row.get(0)?);
    let name: String = row.get(1)?;
    let icon_url: Option<String> = row.get(2)?;
    let departed: bool = row.get::<_, i64>(3)? != 0;
    let origin_s: String = row.get(4)?;
    let origin = crate::sql::origin(4, &origin_s)?;
    // observed_at (index 5) is not exposed on `GuildRecord`.
    let revision = crate::sql::rev(row.get(6)?);
    Ok(GuildRecord {
        guild: Guild {
            id,
            name: name.into(),
            icon_url: icon_url.map(Into::into),
        },
        departed,
        origin,
        revision,
    })
}

/// Insert or update a guild, marking it not departed. Emits
/// [`UnifiedEvent::GuildObserved`] if changed.
pub fn upsert(
    tx: &WriteTx<'_>,
    guild: &Guild,
    origin: Origin,
    observed_at: Timestamp,
) -> StoreResult<bool> {
    let n = tx.execute(
        "INSERT INTO guilds (id, name, icon_url, departed, origin, observed_at, revision)
         VALUES (?1, ?2, ?3, 0, ?4, ?5, ?6)
         ON CONFLICT(id) DO UPDATE SET
             name = excluded.name,
             icon_url = excluded.icon_url,
             departed = 0,
             origin = excluded.origin,
             observed_at = excluded.observed_at,
             revision = excluded.revision
         WHERE guilds.name IS NOT excluded.name
            OR guilds.icon_url IS NOT excluded.icon_url
            OR guilds.departed IS NOT 0",
        params![
            guild.id.to_sql(),
            guild.name.as_ref(),
            guild.icon_url.as_deref(),
            origin.as_str(),
            observed_at.as_millis(),
            tx.revision().get() as i64,
        ],
    )?;
    let changed = n > 0;
    if changed {
        tx.emit(UnifiedEvent::GuildObserved { guild_id: guild.id }, origin)?;
    }
    Ok(changed)
}

/// Insert a placeholder guild (empty name) if `id` is unknown. Emits nothing.
pub fn ensure_stub(
    tx: &WriteTx<'_>,
    id: GuildId,
    origin: Origin,
    observed_at: Timestamp,
) -> StoreResult<bool> {
    let n = tx.execute(
        "INSERT OR IGNORE INTO guilds (id, name, icon_url, departed, origin, observed_at, revision)
         VALUES (?1, '', NULL, 0, ?2, ?3, ?4)",
        params![
            id.to_sql(),
            origin.as_str(),
            observed_at.as_millis(),
            tx.revision().get() as i64,
        ],
    )?;
    Ok(n > 0)
}

/// Mark a single guild departed (left/removed). Emits
/// [`UnifiedEvent::GuildRemoved`] if it was not already departed.
///
/// Not listed explicitly in the handoff spec's API table, but required by
/// the reducer for `DiscordEvent::GuildRemoved`.
pub fn mark_departed(
    tx: &WriteTx<'_>,
    id: GuildId,
    origin: Origin,
    observed_at: Timestamp,
) -> StoreResult<bool> {
    let n = tx.execute(
        "UPDATE guilds SET departed = 1, origin = ?1, observed_at = ?2, revision = ?3
         WHERE id = ?4 AND departed = 0",
        params![
            origin.as_str(),
            observed_at.as_millis(),
            tx.revision().get() as i64,
            id.to_sql(),
        ],
    )?;
    let changed = n > 0;
    if changed {
        tx.emit(UnifiedEvent::GuildRemoved { guild_id: id }, origin)?;
    }
    Ok(changed)
}

/// Replace the authoritative guild list: upsert every guild given and mark
/// any not-departed guild missing from the list as departed.
pub fn replace_all(
    tx: &WriteTx<'_>,
    guilds: &[Guild],
    origin: Origin,
    observed_at: Timestamp,
) -> StoreResult<bool> {
    let mut changed = false;
    let mut keep: Vec<GuildId> = Vec::with_capacity(guilds.len());
    for g in guilds {
        keep.push(g.id);
        if upsert(tx, g, origin, observed_at)? {
            changed = true;
        }
    }
    let existing: Vec<GuildId> = {
        let mut stmt = tx.prepare("SELECT id FROM guilds WHERE departed = 0")?;
        let rows = stmt.query_map([], |r| Ok(GuildId::from_sql(r.get(0)?)))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        out
    };
    for id in existing {
        if !keep.contains(&id) && mark_departed(tx, id, origin, observed_at)? {
            changed = true;
        }
    }
    Ok(changed)
}

/// Look up one guild.
pub fn get(conn: &Connection, id: GuildId) -> StoreResult<Option<GuildRecord>> {
    let sql = format!("SELECT {COLUMNS} FROM guilds WHERE id = ?1");
    Ok(conn
        .query_row(&sql, params![id.to_sql()], map_row)
        .optional()?)
}

/// List guilds ordered by name, optionally including departed ones.
pub fn list(conn: &Connection, include_departed: bool) -> StoreResult<Vec<GuildRecord>> {
    let sql = if include_departed {
        format!("SELECT {COLUMNS} FROM guilds ORDER BY name")
    } else {
        format!("SELECT {COLUMNS} FROM guilds WHERE departed = 0 ORDER BY name")
    };
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], map_row)?;
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

    fn guild(id: u64, name: &str) -> Guild {
        Guild {
            id: GuildId(id),
            name: name.into(),
            icon_url: None,
        }
    }

    #[test]
    fn replace_all_marks_departed() {
        let db = Database::open_in_memory().unwrap();
        db.write(|tx| {
            replace_all(
                tx,
                &[guild(1, "a"), guild(2, "b")],
                Origin::Synthetic,
                Timestamp::from_millis(1),
            )
        })
        .unwrap();
        db.write(|tx| {
            replace_all(
                tx,
                &[guild(1, "a")],
                Origin::Synthetic,
                Timestamp::from_millis(2),
            )
        })
        .unwrap();
        let rec = db.read(|r| get(r, GuildId(2))).unwrap().unwrap();
        assert!(rec.departed);
        let rec1 = db.read(|r| get(r, GuildId(1))).unwrap().unwrap();
        assert!(!rec1.departed);
    }
}
