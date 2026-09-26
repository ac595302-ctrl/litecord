//! Hydration errors.

use litecord_core::error::{Error, ErrorKind};
use litecord_core::ports::BackendError;

/// Errors produced by the hydration scheduler, freshness store and worker.
#[derive(Debug, thiserror::Error)]
pub enum HydrationError {
    /// The [`crate::freshness::FreshnessStore`] backing implementation failed.
    #[error("hydration store error: {0}")]
    Store(String),
    /// The backend fetch itself failed.
    #[error(transparent)]
    Backend(#[from] BackendError),
    /// The canonical ingest queue is closed; the hydrator should stop.
    #[error("ingest queue closed")]
    IngestClosed,
}

impl From<HydrationError> for Error {
    fn from(e: HydrationError) -> Self {
        Error::with_source(ErrorKind::Hydration, e.to_string(), e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_into_core_error_with_hydration_kind() {
        let e: Error = HydrationError::IngestClosed.into();
        assert_eq!(e.kind(), ErrorKind::Hydration);
        assert!(e.to_string().contains("ingest queue closed"));
    }

    #[test]
    fn backend_error_converts_via_from() {
        let e: HydrationError = BackendError::Offline.into();
        assert!(matches!(e, HydrationError::Backend(BackendError::Offline)));
    }
}
