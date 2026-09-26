use litecord_core::{Error, ErrorKind};
use litecord_store::StoreError;

#[derive(Debug, thiserror::Error)]
pub enum RetrievalError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("embedding: {0}")]
    Embedding(String),
}

impl From<RetrievalError> for Error {
    fn from(e: RetrievalError) -> Self {
        Error::with_source(ErrorKind::Retrieval, e.to_string(), e)
    }
}
