//! Relationships between the current user and other users.

use rusqlite::{params, Connection, OptionalExtension, Row};

use litecord_core::events::UnifiedEvent;
use litecord_types::provenance::Origin;
use litecord_types::social::{Relationship, RelationshipKind, User};
use litecord_types::{Revision, Timestamp, UserId};

use crate::db::WriteTx;
use crate::error::StoreResult;
use crate::repos::users::{self, UserRecord};
use crate::sql::{col_err, ts};

/// A relationship row plus the joined user, if known.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct RelationshipRecord {
    pub relationship: Relationship,
    pub user: Option<UserRecord>,
    pub origin: Origin,
    pub revision: Revision,
}

fn map_relationship(row: &Row<'_>) -> rusqlite::Result<Relationship> {
    let user_id = UserId::from_sql(row.get(0)?);
    let discord_s: String = row.get(1)?;
    let discord = RelationshipKind::parse(&discord_s).map_err(|e| col_err(1, e))?;
    let game_s: String = row.get(2)?;
    let game = RelationshipKind::parse(&game_s).map_err(|e| col_err(2, e))?;
    let since = ts(row.get(3)?);
    Ok(Relationship {
        user_id,
        discord,
        game,
        since,
    })
}

/// Insert or update a relationship row. Emits
/// [`UnifiedEvent::RelationshipChanged`] only when it changed.
pub fn upsert(
    tx: &WriteTx<'_>,
    rel: &Relationship,
    origin: Origin,
    observed_at: Timestamp,
) -> StoreResult<bool> {
    let n = tx.execute(
        "INSERT INTO relationships (user_id, discord_kind, game_kind, since, origin, observed_at, revision)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(user_id) DO UPDATE SET
             discord_kind = excluded.discord_kind,
             game_kind = excluded.game_kind,
             since = excluded.since,
             origin = excluded.origin,
             observed_at = excluded.observed_at,
             revision = excluded.revision
         WHERE relationships.discord_kind IS NOT excluded.discord_kind
            OR relationships.game_kind IS NOT excluded.game_kind
            OR relationships.since IS NOT excluded.since",
        params![
            rel.user_id.to_sql(),
            rel.discord.as_str(),
            rel.game.as_str(),
            rel.since.map(Timestamp::as_millis),
            origin.as_str(),
            observed_at.as_millis(),
            tx.revision().get() as i64,
        ],
    )?;
    let changed = n > 0;
    if changed {
        tx.emit(
            UnifiedEvent::RelationshipChanged {
                user_id: rel.user_id,
            },
            origin,
        )?;
    }
    Ok(changed)
}

/// Remove a relationship. Emits [`UnifiedEvent::RelationshipRemoved`] if a
/// row was actually deleted.
///
/// Deviation from the handoff spec: `origin` is an explicit parameter here
/// (the spec's signature omits it), since stamping the removal event
/// requires knowing which source observed the removal.
pub fn remove(tx: &WriteTx<'_>, user_id: UserId, origin: Origin) -> StoreResult<bool> {
    let n = tx.execute(
        "DELETE FROM relationships WHERE user_id = ?1",
        params![user_id.to_sql()],
    )?;
    let changed = n > 0;
    if changed {
        tx.emit(UnifiedEvent::RelationshipRemoved { user_id }, origin)?;
    }
    Ok(changed)
}

/// Ensure `entries` is the authoritative relationship list: upsert every
/// entry (creating a user row or a stub as needed) and remove relationships
/// not present in `entries`.
pub fn replace_all(
    tx: &WriteTx<'_>,
    entries: &[(Relationship, Option<User>)],
    origin: Origin,
    observed_at: Timestamp,
) -> StoreResult<bool> {
    let mut changed = false;
    let mut keep: Vec<UserId> = Vec::with_capacity(entries.len());
    for (rel, user) in entries {
        keep.push(rel.user_id);
        match user {
            Some(u) => {
                if users::upsert(tx, u, origin, observed_at)? {
                    changed = true;
                }
            }
            None => {
                if users::ensure_stub(tx, rel.user_id, origin, observed_at)? {
                    changed = true;
                }
            }
        }
        if upsert(tx, rel, origin, observed_at)? {
            changed = true;
        }
    }

    let existing: Vec<UserId> = {
        let mut stmt = tx.prepare("SELECT user_id FROM relationships")?;
        let rows = stmt.query_map([], |r| Ok(UserId::from_sql(r.get(0)?)))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        out
    };
    for id in existing {
        if !keep.contains(&id) && remove(tx, id, origin)? {
            changed = true;
        }
    }
    Ok(changed)
}

/// Look up one relationship.
pub fn get(conn: &Connection, user_id: UserId) -> StoreResult<Option<Relationship>> {
    Ok(conn
        .query_row(
            "SELECT user_id, discord_kind, game_kind, since FROM relationships WHERE user_id = ?1",
            params![user_id.to_sql()],
            map_relationship,
        )
        .optional()?)
}

/// List relationships, optionally filtered to those whose discord- or
/// game-scoped kind is in `kinds`, joined with the user and ordered by
/// display name.
pub fn list(
    conn: &Connection,
    kinds: Option<&[RelationshipKind]>,
) -> StoreResult<Vec<RelationshipRecord>> {
    let mut sql = String::from(
        "SELECT r.user_id, r.discord_kind, r.game_kind, r.since, r.origin, r.observed_at, r.revision,
                u.id, u.username, u.global_name, u.avatar_url, u.is_bot, u.is_provisional, u.is_stub,
                u.presence_status, u.activity_name, u.activity_details, u.activity_state,
                u.origin, u.observed_at, u.revision
         FROM relationships r
         LEFT JOIN users u ON u.id = r.user_id",
    );
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    if let Some(kinds) = kinds {
        if kinds.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = vec!["?"; kinds.len()].join(",");
        sql.push_str(&format!(
            " WHERE r.discord_kind IN ({placeholders}) OR r.game_kind IN ({placeholders})"
        ));
        for k in kinds {
            params.push(Box::new(k.as_str()));
        }
        for k in kinds {
            params.push(Box::new(k.as_str()));
        }
    }
    sql.push_str(" ORDER BY COALESCE(u.global_name, u.username, '')");

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(
        rusqlite::params_from_iter(params.iter().map(|b| b.as_ref())),
        |row| {
            let relationship = map_relationship(row)?;
            let rel_origin_s: String = row.get(4)?;
            let rel_origin = crate::sql::origin(4, &rel_origin_s)?;
            let rel_revision = crate::sql::rev(row.get(6)?);
            let user_id: Option<i64> = row.get(7)?;
            let user = if user_id.is_some() {
                Some(crate::repos::users::map_row_at(row, 7)?)
            } else {
                None
            };
            Ok(RelationshipRecord {
                relationship,
                user,
                origin: rel_origin,
                revision: rel_revision,
            })
        },
    )?;
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

    fn rel(id: u64, kind: RelationshipKind) -> Relationship {
        Relationship {
            user_id: UserId(id),
            discord: kind,
            game: RelationshipKind::None,
            since: None,
        }
    }

    #[test]
    fn upsert_and_remove() {
        let db = Database::open_in_memory().unwrap();
        let r = rel(1, RelationshipKind::Friend);
        let c1 = db
            .write(|tx| upsert(tx, &r, Origin::Synthetic, Timestamp::from_millis(1)))
            .unwrap();
        assert!(c1.changed());
        let c2 = db
            .write(|tx| remove(tx, UserId(1), Origin::Synthetic))
            .unwrap();
        assert!(c2.changed());
        let got = db.read(|r| get(r, UserId(1))).unwrap();
        assert!(got.is_none());
    }

    #[test]
    fn replace_all_removes_missing() {
        let db = Database::open_in_memory().unwrap();
        let entries = vec![
            (rel(1, RelationshipKind::Friend), None),
            (rel(2, RelationshipKind::Friend), None),
        ];
        db.write(|tx| replace_all(tx, &entries, Origin::Synthetic, Timestamp::from_millis(1)))
            .unwrap();
        let entries2 = vec![(rel(1, RelationshipKind::Friend), None)];
        let c = db
            .write(|tx| replace_all(tx, &entries2, Origin::Synthetic, Timestamp::from_millis(2)))
            .unwrap();
        assert!(c.changed());
        assert!(db.read(|r| get(r, UserId(2))).unwrap().is_none());
        assert!(db.read(|r| get(r, UserId(1))).unwrap().is_some());
    }
}
