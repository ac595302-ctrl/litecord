//! Categorized errors for architectural boundaries.
//!
//! Crates with rich failure modes (store, actions, backends) define their own
//! typed errors and convert into [`Error`] at the boundary. Messages must never
//! contain secrets or raw message content.

use std::fmt;

/// Broad failure category. UIs and agents can branch on this without parsing
/// strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    Configuration,
    Authentication,
    Discord,
    Storage,
    Hydration,
    Retrieval,
    Agent,
    ActionPolicy,
    Validation,
    NotFound,
    Unsupported,
    Offline,
    Shutdown,
    Internal,
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            ErrorKind::Configuration => "configuration error",
            ErrorKind::Authentication => "authentication error",
            ErrorKind::Discord => "discord error",
            ErrorKind::Storage => "storage error",
            ErrorKind::Hydration => "hydration error",
            ErrorKind::Retrieval => "retrieval error",
            ErrorKind::Agent => "agent error",
            ErrorKind::ActionPolicy => "action policy error",
            ErrorKind::Validation => "validation error",
            ErrorKind::NotFound => "not found",
            ErrorKind::Unsupported => "unsupported",
            ErrorKind::Offline => "offline",
            ErrorKind::Shutdown => "shutting down",
            ErrorKind::Internal => "internal error",
        };
        f.write_str(s)
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{kind}: {message}")]
pub struct Error {
    kind: ErrorKind,
    message: String,
    #[source]
    source: Option<Box<dyn std::error::Error + Send + Sync + 'static>>,
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

impl Error {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            source: None,
        }
    }

    pub fn with_source(
        kind: ErrorKind,
        message: impl Into<String>,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            kind,
            message: message.into(),
            source: Some(Box::new(source)),
        }
    }

    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn config(msg: impl Into<String>) -> Self {
        Self::new(ErrorKind::Configuration, msg)
    }
    pub fn validation(msg: impl Into<String>) -> Self {
        Self::new(ErrorKind::Validation, msg)
    }
    pub fn not_found(msg: impl Into<String>) -> Self {
        Self::new(ErrorKind::NotFound, msg)
    }
    pub fn internal(msg: impl Into<String>) -> Self {
        Self::new(ErrorKind::Internal, msg)
    }
}

impl From<litecord_types::ValidationError> for Error {
    fn from(e: litecord_types::ValidationError) -> Self {
        Error::with_source(ErrorKind::Validation, e.to_string(), e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_includes_category() {
        let e = Error::not_found("conversation 5");
        assert_eq!(e.to_string(), "not found: conversation 5");
        assert_eq!(e.kind(), ErrorKind::NotFound);
    }
}
