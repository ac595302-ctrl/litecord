//! Optional semantic vectors, keyed by `(owner, model)`. Purely additive: the
//! rest of the system works without embeddings, so this repository never
//! emits a `UnifiedEvent` (there is none for embeddings) and never fails a
//! caller for a missing vector — only for a stored one that is corrupt.

use rusqlite::{params, types::Value, Connection, OptionalExtension};

use litecord_types::entity::EntityId;

use crate::db::WriteTx;
use crate::error::{StoreError, StoreResult};

/// Insert or replace the embedding for `(owner, model)`. Rejects an empty
/// vector or one containing a non-finite value. Returns `false` (no-op) if
/// the stored vector is already identical.
pub fn upsert(
    tx: &WriteTx<'_>,
    owner: &EntityId,
    model: &str,
    vector: &[f32],
) -> StoreResult<bool> {
    if vector.is_empty() {
        return Err(StoreError::Invariant(
            "embedding vector must not be empty".into(),
        ));
    }
    if vector.iter().any(|v| !v.is_finite()) {
        return Err(StoreError::Invariant(
            "embedding vector must contain only finite values".into(),
        ));
    }
    let bytes = encode_vector(vector);
    let dims = vector.len() as i64;
    let n = tx.execute(
        "INSERT INTO embeddings (owner, model, dims, vector, created_at) VALUES (?, ?, ?, ?, ?) \
         ON CONFLICT(owner, model) DO UPDATE SET \
            dims = excluded.dims, vector = excluded.vector, created_at = excluded.created_at \
         WHERE embeddings.vector IS NOT excluded.vector OR embeddings.dims IS NOT excluded.dims",
        params![owner.to_string(), model, dims, bytes, tx.now().as_millis()],
    )?;
    Ok(n > 0)
}

fn encode_vector(vector: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(vector.len() * 4);
    for v in vector {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    bytes
}

fn decode_vector(dims: i64, bytes: &[u8]) -> StoreResult<Vec<f32>> {
    if !bytes.len().is_multiple_of(4) || bytes.len() as i64 / 4 != dims {
        return Err(StoreError::corrupt(
            "embeddings",
            format!(
                "blob length {} is inconsistent with stored dims {dims}",
                bytes.len()
            ),
        ));
    }
    Ok(bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| f32::from_le_bytes(*c))
        .collect())
}

/// Load the embedding for `(owner, model)`, if any.
pub fn get(conn: &Connection, owner: &EntityId, model: &str) -> StoreResult<Option<Vec<f32>>> {
    let row: Option<(i64, Vec<u8>)> = conn
        .query_row(
            "SELECT dims, vector FROM embeddings WHERE owner = ? AND model = ?",
            params![owner.to_string(), model],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    row.map(|(dims, bytes)| decode_vector(dims, &bytes))
        .transpose()
}

/// Load embeddings for several owners under the same model. Owners with no
/// stored embedding are simply absent from the result.
pub fn get_many(
    conn: &Connection,
    owners: &[EntityId],
    model: &str,
) -> StoreResult<Vec<(EntityId, Vec<f32>)>> {
    if owners.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = vec!["?"; owners.len()].join(",");
    let sql = format!(
        "SELECT owner, dims, vector FROM embeddings WHERE model = ? AND owner IN ({placeholders})"
    );
    let mut params: Vec<Value> = vec![Value::Text(model.to_string())];
    params.extend(owners.iter().map(|o| Value::Text(o.to_string())));

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(params), |r| {
        let owner: String = r.get(0)?;
        let dims: i64 = r.get(1)?;
        let bytes: Vec<u8> = r.get(2)?;
        Ok((owner, dims, bytes))
    })?;

    let mut out = Vec::new();
    for row in rows {
        let (owner_s, dims, bytes) = row?;
        let owner = owner_s
            .parse::<EntityId>()
            .map_err(|e| StoreError::corrupt("embeddings", e.to_string()))?;
        out.push((owner, decode_vector(dims, &bytes)?));
    }
    Ok(out)
}

/// Delete every embedding stored for `owner` (any model). Returns the number
/// removed.
pub fn delete_for_owner(tx: &WriteTx<'_>, owner: &EntityId) -> StoreResult<usize> {
    Ok(tx.execute(
        "DELETE FROM embeddings WHERE owner = ?",
        params![owner.to_string()],
    )?)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::db::Database;
    use litecord_types::ids::UserId;

    #[test]
    fn roundtrip_and_get_many() {
        let db = Database::open_in_memory().unwrap();
        let owner1 = EntityId::User(UserId(1));
        let owner2 = EntityId::User(UserId(2));
        db.write(|tx| upsert(tx, &owner1, "m1", &[1.0, 2.0, 3.0]))
            .unwrap();
        db.write(|tx| upsert(tx, &owner2, "m1", &[4.0, 5.0, 6.0]))
            .unwrap();

        let v = db.read(|r| get(r, &owner1, "m1")).unwrap().unwrap();
        assert_eq!(v, vec![1.0, 2.0, 3.0]);

        let many = db.read(|r| get_many(r, &[owner1, owner2], "m1")).unwrap();
        assert_eq!(many.len(), 2);

        let n = db.write(|tx| delete_for_owner(tx, &owner1)).unwrap().value;
        assert_eq!(n, 1);
        assert!(db.read(|r| get(r, &owner1, "m1")).unwrap().is_none());
    }

    #[test]
    fn rejects_empty_and_non_finite() {
        let db = Database::open_in_memory().unwrap();
        let owner = EntityId::User(UserId(1));
        assert!(db.write(|tx| upsert(tx, &owner, "m1", &[])).is_err());
        assert!(db
            .write(|tx| upsert(tx, &owner, "m1", &[f32::NAN]))
            .is_err());
    }

    #[test]
    fn corrupt_blob_is_reported() {
        let db = Database::open_in_memory().unwrap();
        let owner = EntityId::User(UserId(1));
        db.write(|tx| {
            tx.execute(
                "INSERT INTO embeddings (owner, model, dims, vector, created_at) VALUES (?, ?, ?, ?, 0)",
                params![owner.to_string(), "m1", 3_i64, vec![0_u8, 1, 2]],
            )?;
            Ok::<_, StoreError>(())
        })
        .unwrap();
        let err = db.read(|r| get(r, &owner, "m1")).unwrap_err();
        assert!(matches!(err, StoreError::Corrupt { .. }));
    }
}
