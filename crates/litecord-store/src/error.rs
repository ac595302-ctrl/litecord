use litecord_core::{Error, ErrorKind};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("serialization: {0}")]
    Json(#[from] serde_json::Error),
    #[error("corrupt row in {table}: {reason}")]
    Corrupt { table: &'static str, reason: String },
    #[error("{0} not found")]
    NotFound(String),
    #[error("invariant violated: {0}")]
    Invariant(String),
    #[error("migration {version} failed: {reason}")]
    Migration { version: u32, reason: String },
    #[error("database lock poisoned")]
    Poisoned,
}

pub type StoreResult<T> = Result<T, StoreError>;

impl StoreError {
    pub fn corrupt(table: &'static str, reason: impl Into<String>) -> Self {
        StoreError::Corrupt {
            table,
            reason: reason.into(),
        }
    }
}

impl From<litecord_types::ValidationError> for StoreError {
    fn from(e: litecord_types::ValidationError) -> Self {
        StoreError::Corrupt {
            table: "?",
            reason: e.to_string(),
        }
    }
}

impl From<StoreError> for Error {
    fn from(e: StoreError) -> Self {
        let kind = match &e {
            StoreError::NotFound(_) => ErrorKind::NotFound,
            StoreError::Invariant(_) => ErrorKind::Validation,
            _ => ErrorKind::Storage,
        };
        Error::with_source(kind, e.to_string(), e)
    }
}
