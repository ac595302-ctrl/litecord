use litecord_core::{Error, ErrorKind};
use litecord_store::StoreError;
use litecord_types::actions::ActionStatus;
use litecord_types::ids::ActionId;

/// Typed Action Engine failures. Every variant is safe to show to the user
/// and to agents (no secrets, no message content).
#[derive(Debug, thiserror::Error)]
pub enum ActionError {
    #[error("action {0} not found")]
    NotFound(ActionId),
    #[error("policy denied: {0}")]
    PolicyDenied(String),
    #[error("invalid action: {0}")]
    Invalid(String),
    #[error("action {id} is {status}, expected {expected}")]
    WrongStatus {
        id: ActionId,
        status: ActionStatus,
        expected: &'static str,
    },
    #[error("approval token is invalid")]
    InvalidToken,
    #[error("approval token has expired")]
    TokenExpired,
    #[error("approval token was already used or revoked")]
    TokenConsumed,
    #[error("approved payload no longer matches the proposal")]
    PayloadMismatch,
    #[error("revalidation failed: {0}")]
    RevalidationFailed(String),
    #[error("execution failed: {0}")]
    Execution(String),
    #[error("Delivery uncertain: {0}")]
    Uncertain(String),
    #[error("storage: {0}")]
    Store(#[from] StoreError),
    #[error("internal: {0}")]
    Internal(String),
}

impl From<ActionError> for Error {
    fn from(e: ActionError) -> Self {
        let kind = match &e {
            ActionError::NotFound(_) => ErrorKind::NotFound,
            ActionError::Invalid(_) => ErrorKind::Validation,
            ActionError::Store(_) => ErrorKind::Storage,
            ActionError::Execution(_) | ActionError::Uncertain(_) => ErrorKind::Discord,
            ActionError::Internal(_) => ErrorKind::Internal,
            _ => ErrorKind::ActionPolicy,
        };
        Error::with_source(kind, e.to_string(), e)
    }
}
