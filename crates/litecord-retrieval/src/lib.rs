//! Retrieval over unified memory.
//!
//! Works with **no AI provider configured**: lexical relevance comes from
//! SQLite FTS5 (bm25), filters are structured SQL, temporal phrases are parsed
//! deterministically. Semantic relevance is an optional re-ranking signal
//! from an [`embedding::EmbeddingProvider`].
//!
//! Retrieval never bypasses agent visibility: every result is checked against
//! a [`VisibilityPolicy`]; hidden and metadata-only conversations never
//! contribute content.

pub mod embedding;
pub mod error;
pub mod retriever;
pub mod score;
pub mod temporal;
pub mod visibility;

pub use error::RetrievalError;
pub use retriever::{
    DocKind, RetrievalFilters, RetrievalQuery, RetrievedDoc, RetrievedItem, Retriever,
};
pub use score::{RetrievalScore, ScoringWeights};
pub use visibility::VisibilityPolicy;
