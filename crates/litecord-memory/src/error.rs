//! Error types for the unified-memory services.

use litecord_core::{Error, ErrorKind};
use litecord_store::StoreError;

/// Everything a `litecord-memory` service call can fail with.
#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    /// A lower-level storage failure (SQL error, invariant violation raised
    /// by a `repos::*` function, corrupt row, ...).
    #[error("store: {0}")]
    Store(#[from] StoreError),
    /// The caller named an id that does not exist (or no longer exists in the
    /// state the operation requires, e.g. confirming an item that was never
    /// inserted).
    #[error("{0} not found")]
    NotFound(String),
    /// The request was well-formed but violates a memory-service invariant
    /// (e.g. confirming a candidate task that has already been dismissed).
    #[error("invalid request: {0}")]
    Invalid(String),
}

/// Convenience alias used throughout this crate.
pub type MemoryResult<T> = Result<T, MemoryError>;

impl From<MemoryError> for Error {
    fn from(e: MemoryError) -> Self {
        let kind = match &e {
            MemoryError::NotFound(_) => ErrorKind::NotFound,
            MemoryError::Invalid(_) => ErrorKind::Validation,
            MemoryError::Store(_) => ErrorKind::Storage,
        };
        Error::with_source(kind, e.to_string(), e)
    }
}
