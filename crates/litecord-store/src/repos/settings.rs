//! User-changeable settings, stored as JSON under an arbitrary string key.

use rusqlite::{params, Connection, OptionalExtension};
use serde::de::DeserializeOwned;
use serde::Serialize;

use litecord_core::events::UnifiedEvent;
use litecord_types::provenance::Origin;

use crate::db::WriteTx;
use crate::error::StoreResult;

/// Set a raw JSON value for `key`. No-op (no event, no revision churn) if the
/// stored value is already identical. Emits [`UnifiedEvent::SettingChanged`]
/// with [`Origin::UserProvided`] when it actually changes.
pub fn set_json(tx: &WriteTx<'_>, key: &str, value: &serde_json::Value) -> StoreResult<bool> {
    let json = serde_json::to_string(value)?;
    let n = tx.execute(
        "INSERT INTO settings (key, value, updated_at) VALUES (?, ?, ?) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at \
         WHERE settings.value IS NOT excluded.value",
        params![key, json, tx.now().as_millis()],
    )?;
    let changed = n > 0;
    if changed {
        tx.emit(
            UnifiedEvent::SettingChanged {
                key: key.to_string(),
            },
            Origin::UserProvided,
        )?;
    }
    Ok(changed)
}

/// Read a raw JSON value.
pub fn get_json(conn: &Connection, key: &str) -> StoreResult<Option<serde_json::Value>> {
    let raw = get_raw(conn, key)?;
    raw.map(|s| serde_json::from_str(&s).map_err(Into::into))
        .transpose()
}

/// Read without parsing, for preference recovery that must preserve corrupt data.
pub fn get_raw(conn: &Connection, key: &str) -> StoreResult<Option<String>> {
    conn.query_row(
        "SELECT value FROM settings WHERE key = ?",
        params![key],
        |r| r.get(0),
    )
    .optional()
    .map_err(Into::into)
}

/// Read and deserialize a typed setting.
pub fn get<T: DeserializeOwned>(conn: &Connection, key: &str) -> StoreResult<Option<T>> {
    match get_json(conn, key)? {
        Some(v) => Ok(Some(serde_json::from_value(v)?)),
        None => Ok(None),
    }
}

/// Serialize and store a typed setting.
pub fn set<T: Serialize>(tx: &WriteTx<'_>, key: &str, value: &T) -> StoreResult<bool> {
    let v = serde_json::to_value(value)?;
    set_json(tx, key, &v)
}

/// All settings as `(key, value)` pairs.
pub fn all(conn: &Connection) -> StoreResult<Vec<(String, serde_json::Value)>> {
    let mut stmt = conn.prepare("SELECT key, value FROM settings ORDER BY key")?;
    let rows = stmt.query_map([], |r| {
        let key: String = r.get(0)?;
        let raw: String = r.get(1)?;
        Ok((key, raw))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (key, raw) = row?;
        out.push((key, serde_json::from_str(&raw)?));
    }
    Ok(out)
}

/// Remove a setting. Returns whether it existed.
pub fn remove(tx: &WriteTx<'_>, key: &str) -> StoreResult<bool> {
    let n = tx.execute("DELETE FROM settings WHERE key = ?", params![key])?;
    Ok(n > 0)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::db::Database;
    use serde::Deserialize;

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Prefs {
        theme: String,
    }

    #[test]
    fn no_op_on_identical_value() {
        let db = Database::open_in_memory().unwrap();
        let v = serde_json::json!({"a": 1});
        let c1 = db.write(|tx| set_json(tx, "k", &v)).unwrap();
        assert!(c1.changed());
        let c2 = db.write(|tx| set_json(tx, "k", &v)).unwrap();
        assert!(!c2.changed());
    }

    #[test]
    fn typed_roundtrip_and_remove() {
        let db = Database::open_in_memory().unwrap();
        let prefs = Prefs {
            theme: "dark".into(),
        };
        db.write(|tx| set(tx, "prefs", &prefs)).unwrap();
        let back: Prefs = db.read(|r| get(r, "prefs")).unwrap().unwrap();
        assert_eq!(back, prefs);

        assert_eq!(db.read(|r| all(r)).unwrap().len(), 1);
        let removed = db.write(|tx| remove(tx, "prefs")).unwrap().value;
        assert!(removed);
        assert!(db.read(|r| get::<Prefs>(r, "prefs")).unwrap().is_none());
    }
}
