//! Durable workspace preferences, independent of UI widgets and social data.

mod model;
mod operations;
mod profiles;
pub mod registry;
mod sizing;

pub use model::*;
pub use profiles::*;
pub use sizing::*;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct LayoutError(pub String);

pub type LayoutResult<T> = Result<T, LayoutError>;
