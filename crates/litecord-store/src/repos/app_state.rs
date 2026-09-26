//! Small non-user-facing key/value state (e.g. the last known session
//! state). Never emits events.

use rusqlite::{params, Connection, OptionalExtension};

use crate::db::WriteTx;
use crate::error::StoreResult;

/// Set `key` to `value`. No-op (no revision churn) if unchanged.
pub fn set(tx: &WriteTx<'_>, key: &str, value: &str) -> StoreResult<bool> {
    let n = tx.execute(
        "INSERT INTO app_state (key, value, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at
         WHERE app_state.value IS NOT excluded.value",
        params![key, value, tx.now().as_millis()],
    )?;
    Ok(n > 0)
}

/// Read a key, if set.
pub fn get(conn: &Connection, key: &str) -> StoreResult<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT value FROM app_state WHERE key = ?1",
            params![key],
            |r| r.get(0),
        )
        .optional()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use litecord_types::Revision;

    #[test]
    fn set_then_get_roundtrips_and_is_idempotent() {
        let db = Database::open_in_memory().unwrap();
        // `app_state` never emits events, so `Committed::changed()` (which
        // tracks emitted events) is not the right check here; the revision
        // still advances (checked below) and `set`'s own return value
        // reports whether the row changed.
        let c1 = db.write(|tx| set(tx, "k", "v")).unwrap();
        assert!(c1.value);
        let c2 = db.write(|tx| set(tx, "k", "v")).unwrap();
        assert!(!c2.value);
        assert_eq!(c1.revision, Revision(1));
        assert_eq!(c2.revision, Revision(1));
        assert_eq!(db.read(|r| get(r, "k")).unwrap(), Some("v".to_string()));
    }
}
