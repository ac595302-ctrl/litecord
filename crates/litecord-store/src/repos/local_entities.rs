//! Graph-only local entities (topics, projects, people not yet Discord
//! users, ...) that the entity graph can point at (V2 §11). Names are unique
//! per kind, case-insensitively, so "Project Nova" and "project nova" resolve
//! to the same entity.

use rusqlite::{params, Connection, OptionalExtension};

use litecord_types::entity::{EntityId, LocalEntityKind};
use litecord_types::memory::LocalEntity;
use litecord_types::provenance::Origin;
use litecord_types::{LocalEntityId, Timestamp, ValidationError};

use crate::db::WriteTx;
use crate::error::{StoreError, StoreResult};

/// [`LocalEntityKind`] has no `parse` of its own (it is not built with the
/// `str_enum!` macro in `litecord-types`); this mirrors `EntityId`'s own
/// string mapping for the four kinds.
fn parse_kind(s: &str) -> Result<LocalEntityKind, ValidationError> {
    match s {
        "topic" => Ok(LocalEntityKind::Topic),
        "project" => Ok(LocalEntityKind::Project),
        "person" => Ok(LocalEntityKind::Person),
        "entity" => Ok(LocalEntityKind::Other),
        _ => Err(ValidationError::Parse {
            what: "LocalEntityKind",
            input: s.to_owned(),
        }),
    }
}

/// Find or create a local entity by (kind, name), matching case-insensitively
/// on an existing name. Rejects a blank name.
pub fn get_or_create(
    tx: &WriteTx<'_>,
    kind: LocalEntityKind,
    name: &str,
    origin: Origin,
) -> StoreResult<EntityId> {
    let name = name.trim();
    if name.is_empty() {
        return Err(StoreError::Invariant(
            "local entity name must not be empty".into(),
        ));
    }
    let existing: Option<i64> = tx
        .query_row(
            "SELECT id FROM local_entities WHERE kind = ? AND lower(name) = lower(?)",
            params![kind.as_str(), name],
            |r| r.get(0),
        )
        .optional()?;
    let id = match existing {
        Some(id) => id,
        None => {
            tx.execute(
                "INSERT INTO local_entities (kind, name, origin, created_at) VALUES (?, ?, ?, ?)",
                params![kind.as_str(), name, origin.as_str(), tx.now().as_millis()],
            )?;
            tx.last_insert_rowid()
        }
    };
    Ok(EntityId::Local(kind, LocalEntityId(id)))
}

/// Load a local entity by id. Returns `None` for a non-local `EntityId` or a
/// missing row.
pub fn get(conn: &Connection, id: &EntityId) -> StoreResult<Option<LocalEntity>> {
    let EntityId::Local(kind, local_id) = *id else {
        return Ok(None);
    };
    let row = conn
        .query_row(
            "SELECT name, origin, created_at FROM local_entities WHERE kind = ? AND id = ?",
            params![kind.as_str(), local_id.get()],
            |r| {
                let name: String = r.get(0)?;
                let origin_s: String = r.get(1)?;
                let origin = crate::sql::origin(1, &origin_s)?;
                let created_at: i64 = r.get(2)?;
                Ok((name, origin, created_at))
            },
        )
        .optional()?;
    Ok(row.map(|(name, origin, created_at)| LocalEntity {
        id: *id,
        name: name.into(),
        origin,
        created_at: Timestamp::from_millis(created_at),
    }))
}

/// Find local entities, optionally filtered by kind and by a case-insensitive
/// name prefix, ordered by name. `limit = 0` means unlimited.
pub fn find(
    conn: &Connection,
    kind: Option<LocalEntityKind>,
    name_prefix: &str,
    limit: u32,
) -> StoreResult<Vec<LocalEntity>> {
    let mut sql =
        String::from("SELECT id, kind, name, origin, created_at FROM local_entities WHERE 1 = 1");
    let mut params: Vec<rusqlite::types::Value> = Vec::new();
    if let Some(k) = kind {
        sql.push_str(" AND kind = ?");
        params.push(k.as_str().to_string().into());
    }
    let prefix = name_prefix.trim();
    if !prefix.is_empty() {
        let escaped = prefix
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        sql.push_str(" AND lower(name) LIKE lower(?) ESCAPE '\\'");
        params.push(format!("{escaped}%").into());
    }
    sql.push_str(" ORDER BY name ASC");
    if limit > 0 {
        sql.push_str(" LIMIT ?");
        params.push((limit as i64).into());
    }
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(params), |r| {
        let id: i64 = r.get(0)?;
        let kind_s: String = r.get(1)?;
        let kind = parse_kind(&kind_s).map_err(|e| crate::sql::col_err(1, e))?;
        let name: String = r.get(2)?;
        let origin_s: String = r.get(3)?;
        let origin = crate::sql::origin(3, &origin_s)?;
        let created_at: i64 = r.get(4)?;
        Ok(LocalEntity {
            id: EntityId::Local(kind, LocalEntityId(id)),
            name: name.into(),
            origin,
            created_at: Timestamp::from_millis(created_at),
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::db::Database;

    #[test]
    fn get_or_create_is_case_insensitive() {
        let db = Database::open_in_memory().unwrap();
        let a = db
            .write(|tx| {
                get_or_create(
                    tx,
                    LocalEntityKind::Topic,
                    "Project Nova",
                    Origin::AgentDerived,
                )
            })
            .unwrap()
            .value;
        let b = db
            .write(|tx| {
                get_or_create(
                    tx,
                    LocalEntityKind::Topic,
                    "project nova",
                    Origin::AgentDerived,
                )
            })
            .unwrap()
            .value;
        assert_eq!(a, b);

        let entity = db.read(|r| get(r, &a)).unwrap().unwrap();
        assert_eq!(&*entity.name, "Project Nova");

        let err = db
            .write(|tx| get_or_create(tx, LocalEntityKind::Topic, "   ", Origin::AgentDerived))
            .unwrap_err();
        assert!(matches!(err, StoreError::Invariant(_)));
    }

    #[test]
    fn find_matches_prefix() {
        let db = Database::open_in_memory().unwrap();
        db.write(|tx| get_or_create(tx, LocalEntityKind::Topic, "Rust", Origin::AgentDerived))
            .unwrap();
        db.write(|tx| {
            get_or_create(
                tx,
                LocalEntityKind::Topic,
                "Rusty Nails",
                Origin::AgentDerived,
            )
        })
        .unwrap();
        db.write(|tx| {
            get_or_create(
                tx,
                LocalEntityKind::Project,
                "Rust Tool",
                Origin::AgentDerived,
            )
        })
        .unwrap();

        let found = db
            .read(|r| find(r, Some(LocalEntityKind::Topic), "rus", 10))
            .unwrap();
        assert_eq!(found.len(), 2);
    }
}
