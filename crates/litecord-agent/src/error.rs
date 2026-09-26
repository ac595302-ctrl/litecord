use litecord_actions::ActionError;
use litecord_core::{Error, ErrorKind};
use litecord_store::StoreError;

/// Errors surfaced to agents. Messages are safe to show to a model: no
/// secrets, no content from hidden conversations.
#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("unknown tool `{0}`")]
    UnknownTool(String),
    #[error("invalid arguments: {0}")]
    InvalidArguments(String),
    #[error("{0} not found")]
    NotFound(String),
    #[error("not permitted: {0}")]
    Denied(String),
    #[error("{0}")]
    Action(#[from] ActionError),
    #[error("storage error")]
    Store(#[from] StoreError),
    #[error("{0}")]
    Core(#[from] Error),
}

impl ToolError {
    /// Whether the failure is the caller's fault (vs an internal error).
    pub fn is_client_error(&self) -> bool {
        matches!(
            self,
            ToolError::UnknownTool(_)
                | ToolError::InvalidArguments(_)
                | ToolError::NotFound(_)
                | ToolError::Denied(_)
                | ToolError::Action(_)
        )
    }
}

impl From<ToolError> for Error {
    fn from(e: ToolError) -> Self {
        let kind = match &e {
            ToolError::NotFound(_) => ErrorKind::NotFound,
            ToolError::Denied(_) => ErrorKind::ActionPolicy,
            ToolError::InvalidArguments(_) | ToolError::UnknownTool(_) => ErrorKind::Validation,
            _ => ErrorKind::Agent,
        };
        Error::with_source(kind, e.to_string(), e)
    }
}
