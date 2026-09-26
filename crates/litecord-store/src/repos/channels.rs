//! Guild channels.

use rusqlite::{params, Connection, OptionalExtension, Row};

use litecord_core::events::UnifiedEvent;
use litecord_types::provenance::Origin;
use litecord_types::social::{Channel, ChannelAccess, ChannelCapabilities, ChannelKind};
use litecord_types::{ChannelId, GuildId, Revision, Timestamp};

use crate::db::WriteTx;
use crate::error::StoreResult;
use crate::repos::guilds;
use crate::sql::col_err;

/// A stored channel row plus bookkeeping.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ChannelRecord {
    pub channel: Channel,
    pub removed: bool,
    pub origin: Origin,
    pub revision: Revision,
}

const COLUMNS: &str =
    "id, guild_id, name, kind, position, parent_id, access, capabilities, removed, origin, observed_at, revision";

fn map_row(row: &Row<'_>) -> rusqlite::Result<ChannelRecord> {
    let id = ChannelId::from_sql(row.get(0)?);
    let guild_id = GuildId::from_sql(row.get(1)?);
    let name: String = row.get(2)?;
    let kind_s: String = row.get(3)?;
    let kind = ChannelKind::parse(&kind_s).map_err(|e| col_err(3, e))?;
    let position: i32 = row.get(4)?;
    let parent_id: Option<i64> = row.get(5)?;
    let access_s: String = row.get(6)?;
    let access = ChannelAccess::parse(&access_s).map_err(|e| col_err(6, e))?;
    let capabilities: u32 = row.get::<_, i64>(7)? as u32;
    let removed: bool = row.get::<_, i64>(8)? != 0;
    let origin_s: String = row.get(9)?;
    let origin = crate::sql::origin(9, &origin_s)?;
    // observed_at (index 10) is not exposed on `ChannelRecord`.
    let revision = crate::sql::rev(row.get(11)?);
    Ok(ChannelRecord {
        channel: Channel {
            id,
            guild_id,
            name: name.into(),
            kind,
            position,
            parent_id: parent_id.map(ChannelId::from_sql),
            access,
            capabilities: ChannelCapabilities(capabilities),
        },
        removed,
        origin,
        revision,
    })
}

/// Insert or update a channel, marking it not removed. Ensures the owning
/// guild exists (as a stub if necessary). Emits
/// [`UnifiedEvent::ChannelObserved`] if changed.
pub fn upsert(
    tx: &WriteTx<'_>,
    channel: &Channel,
    origin: Origin,
    observed_at: Timestamp,
) -> StoreResult<bool> {
    guilds::ensure_stub(tx, channel.guild_id, origin, observed_at)?;
    let n = tx.execute(
        "INSERT INTO channels (id, guild_id, name, kind, position, parent_id, access, capabilities, removed, origin, observed_at, revision)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0, ?9, ?10, ?11)
         ON CONFLICT(id) DO UPDATE SET
             guild_id = excluded.guild_id,
             name = excluded.name,
             kind = excluded.kind,
             position = excluded.position,
             parent_id = excluded.parent_id,
             access = excluded.access,
             capabilities = excluded.capabilities,
             removed = 0,
             origin = excluded.origin,
             observed_at = excluded.observed_at,
             revision = excluded.revision
         WHERE channels.guild_id IS NOT excluded.guild_id
            OR channels.name IS NOT excluded.name
            OR channels.kind IS NOT excluded.kind
            OR channels.position IS NOT excluded.position
            OR channels.parent_id IS NOT excluded.parent_id
            OR channels.access IS NOT excluded.access
            OR channels.capabilities IS NOT excluded.capabilities
            OR channels.removed IS NOT 0",
        params![
            channel.id.to_sql(),
            channel.guild_id.to_sql(),
            channel.name.as_ref(),
            channel.kind.as_str(),
            channel.position,
            channel.parent_id.map(|p| p.to_sql()),
            channel.access.as_str(),
            channel.capabilities.0 as i64,
            origin.as_str(),
            observed_at.as_millis(),
            tx.revision().get() as i64,
        ],
    )?;
    let changed = n > 0;
    if changed {
        tx.emit(
            UnifiedEvent::ChannelObserved {
                channel_id: channel.id,
            },
            origin,
        )?;
    }
    Ok(changed)
}

/// Replace the authoritative channel list for one guild: upsert every
/// channel given and mark any not-removed channel in the guild missing from
/// the list as removed.
pub fn replace_for_guild(
    tx: &WriteTx<'_>,
    guild_id: GuildId,
    channels: &[Channel],
    origin: Origin,
    observed_at: Timestamp,
) -> StoreResult<bool> {
    let mut changed = false;
    let mut keep: Vec<ChannelId> = Vec::with_capacity(channels.len());
    for c in channels {
        keep.push(c.id);
        if upsert(tx, c, origin, observed_at)? {
            changed = true;
        }
    }
    let existing: Vec<ChannelId> = {
        let mut stmt = tx.prepare("SELECT id FROM channels WHERE guild_id = ?1 AND removed = 0")?;
        let rows = stmt.query_map(params![guild_id.to_sql()], |r| {
            Ok(ChannelId::from_sql(r.get(0)?))
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        out
    };
    for id in existing {
        if keep.contains(&id) {
            continue;
        }
        let n = tx.execute(
            "UPDATE channels SET removed = 1, origin = ?1, observed_at = ?2, revision = ?3
             WHERE id = ?4 AND removed = 0",
            params![
                origin.as_str(),
                observed_at.as_millis(),
                tx.revision().get() as i64,
                id.to_sql(),
            ],
        )?;
        if n > 0 {
            changed = true;
            tx.emit(UnifiedEvent::ChannelRemoved { channel_id: id }, origin)?;
        }
    }
    Ok(changed)
}

/// Look up one channel.
pub fn get(conn: &Connection, id: ChannelId) -> StoreResult<Option<ChannelRecord>> {
    let sql = format!("SELECT {COLUMNS} FROM channels WHERE id = ?1");
    Ok(conn
        .query_row(&sql, params![id.to_sql()], map_row)
        .optional()?)
}

/// List channels for a guild, ordered by position then name.
pub fn list_for_guild(
    conn: &Connection,
    guild_id: GuildId,
    include_removed: bool,
) -> StoreResult<Vec<ChannelRecord>> {
    let sql = if include_removed {
        format!("SELECT {COLUMNS} FROM channels WHERE guild_id = ?1 ORDER BY position, name")
    } else {
        format!(
            "SELECT {COLUMNS} FROM channels WHERE guild_id = ?1 AND removed = 0 ORDER BY position, name"
        )
    };
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![guild_id.to_sql()], map_row)?;
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

    fn channel(id: u64, guild_id: u64, name: &str, pos: i32) -> Channel {
        Channel {
            id: ChannelId(id),
            guild_id: GuildId(guild_id),
            name: name.into(),
            kind: ChannelKind::Text,
            position: pos,
            parent_id: None,
            access: ChannelAccess::Native,
            capabilities: ChannelCapabilities::READABLE,
        }
    }

    #[test]
    fn upsert_creates_guild_stub() {
        let db = Database::open_in_memory().unwrap();
        db.write(|tx| {
            upsert(
                tx,
                &channel(1, 10, "general", 0),
                Origin::Synthetic,
                Timestamp::from_millis(1),
            )
        })
        .unwrap();
        let g = db.read(|r| guilds::get(r, GuildId(10))).unwrap().unwrap();
        assert!(g.guild.name.is_empty());
    }

    #[test]
    fn replace_for_guild_removes_missing() {
        let db = Database::open_in_memory().unwrap();
        db.write(|tx| {
            replace_for_guild(
                tx,
                GuildId(10),
                &[channel(1, 10, "a", 0), channel(2, 10, "b", 1)],
                Origin::Synthetic,
                Timestamp::from_millis(1),
            )
        })
        .unwrap();
        db.write(|tx| {
            replace_for_guild(
                tx,
                GuildId(10),
                &[channel(1, 10, "a", 0)],
                Origin::Synthetic,
                Timestamp::from_millis(2),
            )
        })
        .unwrap();
        let rec = db.read(|r| get(r, ChannelId(2))).unwrap().unwrap();
        assert!(rec.removed);
    }
}
