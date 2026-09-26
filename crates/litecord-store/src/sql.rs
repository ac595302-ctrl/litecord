//! Small helpers shared by repositories.

use litecord_types::provenance::Origin;
use litecord_types::{Revision, Timestamp};
use rusqlite::types::Type;

use crate::error::StoreError;

pub(crate) fn ts(v: Option<i64>) -> Option<Timestamp> {
    v.map(Timestamp::from_millis)
}

pub(crate) fn rev(v: i64) -> Revision {
    Revision(v.max(0) as u64)
}

/// Convert a domain parse error inside a row mapper into a rusqlite error so
/// `?` works in `query_map` closures.
pub(crate) fn col_err(
    idx: usize,
    e: impl std::error::Error + Send + Sync + 'static,
) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(idx, Type::Text, Box::new(e))
}

pub(crate) fn origin(idx: usize, s: &str) -> rusqlite::Result<Origin> {
    Origin::parse(s).map_err(|e| col_err(idx, e))
}

pub(crate) fn json_err(table: &'static str, e: serde_json::Error) -> StoreError {
    StoreError::corrupt(table, e.to_string())
}
