//! Social SDK lobbies.

use rusqlite::{params, Connection, OptionalExtension};

use litecord_core::events::UnifiedEvent;
use litecord_types::provenance::Origin;
use litecord_types::social::Lobby;
use litecord_types::{ChannelId, LobbyId, Timestamp, UserId};

use crate::db::WriteTx;
use crate::error::StoreResult;

/// Insert or update a lobby and replace its member list. Emits
/// [`UnifiedEvent::LobbyUpdated`] if anything changed.
pub fn upsert(
    tx: &WriteTx<'_>,
    lobby: &Lobby,
    origin: Origin,
    observed_at: Timestamp,
) -> StoreResult<bool> {
    let mut changed = false;

    let n = tx.execute(
        "INSERT INTO lobbies (id, linked_channel_id, origin, observed_at, revision)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(id) DO UPDATE SET
             linked_channel_id = excluded.linked_channel_id,
             origin = excluded.origin,
             observed_at = excluded.observed_at,
             revision = excluded.revision
         WHERE lobbies.linked_channel_id IS NOT excluded.linked_channel_id",
        params![
            lobby.id.to_sql(),
            lobby.linked_channel_id.map(|c| c.to_sql()),
            origin.as_str(),
            observed_at.as_millis(),
            tx.revision().get() as i64,
        ],
    )?;
    if n > 0 {
        changed = true;
    }

    let mut existing: Vec<i64> = {
        let mut stmt = tx.prepare("SELECT user_id FROM lobby_members WHERE lobby_id = ?1")?;
        let rows = stmt.query_map(params![lobby.id.to_sql()], |r| r.get::<_, i64>(0))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        out
    };
    existing.sort_unstable();

    let mut wanted: Vec<i64> = lobby.member_ids.iter().map(|u| u.to_sql()).collect();
    wanted.sort_unstable();
    wanted.dedup();

    if existing != wanted {
        tx.execute(
            "DELETE FROM lobby_members WHERE lobby_id = ?1",
            params![lobby.id.to_sql()],
        )?;
        for uid in &wanted {
            tx.execute(
                "INSERT INTO lobby_members (lobby_id, user_id) VALUES (?1, ?2)",
                params![lobby.id.to_sql(), uid],
            )?;
        }
        changed = true;
    }

    if changed {
        tx.emit(UnifiedEvent::LobbyUpdated { lobby_id: lobby.id }, origin)?;
    }
    Ok(changed)
}

/// Look up one lobby with its members.
pub fn get(conn: &Connection, id: LobbyId) -> StoreResult<Option<Lobby>> {
    let linked_channel_id: Option<i64> = match conn
        .query_row(
            "SELECT linked_channel_id FROM lobbies WHERE id = ?1",
            params![id.to_sql()],
            |r| r.get(0),
        )
        .optional()?
    {
        Some(v) => v,
        None => return Ok(None),
    };

    let mut stmt =
        conn.prepare("SELECT user_id FROM lobby_members WHERE lobby_id = ?1 ORDER BY user_id")?;
    let rows = stmt.query_map(params![id.to_sql()], |r| Ok(UserId::from_sql(r.get(0)?)))?;
    let mut member_ids = Vec::new();
    for r in rows {
        member_ids.push(r?);
    }

    Ok(Some(Lobby {
        id,
        member_ids,
        linked_channel_id: linked_channel_id.map(ChannelId::from_sql),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;

    #[test]
    fn upsert_replaces_members() {
        let db = Database::open_in_memory().unwrap();
        let lobby = Lobby {
            id: LobbyId(1),
            member_ids: vec![UserId(1), UserId(2)],
            linked_channel_id: None,
        };
        db.write(|tx| upsert(tx, &lobby, Origin::Synthetic, Timestamp::from_millis(1)))
            .unwrap();
        let lobby2 = Lobby {
            member_ids: vec![UserId(2), UserId(3)],
            ..lobby.clone()
        };
        db.write(|tx| upsert(tx, &lobby2, Origin::Synthetic, Timestamp::from_millis(2)))
            .unwrap();
        let got = db.read(|r| get(r, LobbyId(1))).unwrap().unwrap();
        assert_eq!(got.member_ids, vec![UserId(2), UserId(3)]);
    }
}
