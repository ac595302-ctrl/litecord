use thiserror::Error;

/// A domain-level validation failure. Carries no secrets and is safe to log.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ValidationError {
    #[error("field `{field}` is invalid: {reason}")]
    InvalidField { field: &'static str, reason: String },
    #[error("could not parse {what} from `{input}`")]
    Parse { what: &'static str, input: String },
    #[error("value out of range for `{field}`")]
    OutOfRange { field: &'static str },
}

impl ValidationError {
    pub fn invalid(field: &'static str, reason: impl Into<String>) -> Self {
        Self::InvalidField {
            field,
            reason: reason.into(),
        }
    }
}
