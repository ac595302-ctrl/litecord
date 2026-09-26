//! Guilds ("servers") the current user belongs to.

use rusqlite::{params, Connection, OptionalExtension, Row};

use litecord_core::events::UnifiedEvent;
use litecord_types::provenance::Origin;
use litecord_types::social::Guild;
use litecord_types::{GuildId, Revision, Timestamp};

use crate::db::WriteTx;
use crate::error::StoreResult;
use crate::repos::channels;

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
    upsert_from_source(tx, guild, origin, origin.as_str(), observed_at)
}

/// Upsert a guild and record that `source` currently retains membership.
/// The canonical row is shared across sources; membership is source-scoped.
pub fn upsert_from_source(
    tx: &WriteTx<'_>,
    guild: &Guild,
    origin: Origin,
    source: &str,
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
    let membership_changed = tx.execute(
        "INSERT OR IGNORE INTO guild_source_memberships (source, guild_id) VALUES (?1, ?2)",
        params![source, guild.id.to_sql()],
    )? > 0;
    let canonical_changed = n > 0;
    if canonical_changed {
        tx.emit(UnifiedEvent::GuildObserved { guild_id: guild.id }, origin)?;
    }
    Ok(canonical_changed || membership_changed)
}

/// Insert a placeholder guild (empty name) if `id` is unknown. Emits nothing.
pub fn ensure_stub(
    tx: &WriteTx<'_>,
    id: GuildId,
    origin: Origin,
    observed_at: Timestamp,
) -> StoreResult<bool> {
    ensure_stub_from_source(tx, id, origin, origin.as_str(), observed_at)
}

/// Ensure a placeholder guild exists and record source membership. If this
/// source sees a guild after it was globally marked departed, reactivate the
/// canonical row while preserving any hydrated fields.
pub fn ensure_stub_from_source(
    tx: &WriteTx<'_>,
    id: GuildId,
    origin: Origin,
    source: &str,
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
    let reactivated = tx.execute(
        "UPDATE guilds SET departed = 0, origin = ?1, observed_at = ?2, revision = ?3
         WHERE id = ?4 AND departed = 1",
        params![
            origin.as_str(),
            observed_at.as_millis(),
            tx.revision().get() as i64,
            id.to_sql(),
        ],
    )? > 0;
    tx.execute(
        "INSERT OR IGNORE INTO guild_source_memberships (source, guild_id) VALUES (?1, ?2)",
        params![source, id.to_sql()],
    )?;
    if reactivated {
        tx.emit(UnifiedEvent::GuildObserved { guild_id: id }, origin)?;
    }
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
    mark_departed_from_source(tx, id, origin, origin.as_str(), observed_at)
}

/// Remove one source's guild membership. The canonical guild is marked
/// departed only after no source retains it; its channel memberships are
/// retired with the same source first.
pub fn mark_departed_from_source(
    tx: &WriteTx<'_>,
    id: GuildId,
    origin: Origin,
    source: &str,
    observed_at: Timestamp,
) -> StoreResult<bool> {
    let channels_changed = channels::remove_source_for_guild(tx, id, origin, source, observed_at)?;
    let membership_removed = tx.execute(
        "DELETE FROM guild_source_memberships WHERE source = ?1 AND guild_id = ?2",
        params![source, id.to_sql()],
    )? > 0;
    let retained: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM guild_source_memberships WHERE guild_id = ?1)",
        params![id.to_sql()],
        |r| Ok(r.get::<_, i64>(0)? != 0),
    )?;
    let globally_departed = if retained {
        false
    } else {
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
        changed
    };
    Ok(channels_changed || membership_removed || globally_departed)
}

/// Replace the authoritative guild list: upsert every guild given and mark
/// any not-departed guild missing from the list as departed.
pub fn replace_all(
    tx: &WriteTx<'_>,
    guilds: &[Guild],
    origin: Origin,
    observed_at: Timestamp,
) -> StoreResult<bool> {
    replace_all_from_source(tx, guilds, origin, origin.as_str(), observed_at)
}

/// Replace only one source's authoritative membership list. Guilds retained
/// by another source remain globally active.
pub fn replace_all_from_source(
    tx: &WriteTx<'_>,
    guilds: &[Guild],
    origin: Origin,
    source: &str,
    observed_at: Timestamp,
) -> StoreResult<bool> {
    let mut changed = false;
    let mut keep: Vec<GuildId> = Vec::with_capacity(guilds.len());
    for g in guilds {
        keep.push(g.id);
        if upsert_from_source(tx, g, origin, source, observed_at)? {
            changed = true;
        }
    }
    let source_memberships: Vec<GuildId> = {
        let mut stmt =
            tx.prepare("SELECT guild_id FROM guild_source_memberships WHERE source = ?1")?;
        let rows = stmt.query_map(params![source], |r| Ok(GuildId::from_sql(r.get(0)?)))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        out
    };
    for id in source_memberships {
        if !keep.contains(&id) && mark_departed_from_source(tx, id, origin, source, observed_at)? {
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
