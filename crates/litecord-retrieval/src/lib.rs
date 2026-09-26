//! Retrieval over unified memory.
//!
//! Works with **no AI provider configured**: lexical relevance comes from
//! SQLite FTS5 (bm25), filters are structured SQL, temporal phrases are parsed
//! deterministically. Semantic relevance is an optional re-ranking signal
//! from an [`embedding::EmbeddingProvider`].
//!
//! Retrieval never bypasses agent visibility: callers pass the conversations
//! that are hidden or metadata-only and those are excluded from content
//! results.

pub mod embedding;
pub mod score;
pub mod temporal;
