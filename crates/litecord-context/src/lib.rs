//! The context compiler.
//!
//! ```text
//! AgentRequest ─► intent analysis (keywords, time range, wants)
//!              ─► entity resolution (users by name/alias, focus conversation)
//!              ─► retrieval (FTS + filters, visibility enforced)
//!              ─► relevance gate + ranking (explainable RetrievalScore)
//!              ─► budgeting (TokenBudget, per-section caps)
//!              ─► ContextPack { as_of_revision, trust-labelled items, stats }
//! ```
//!
//! The whole compilation runs inside **one** read transaction, so the pack is
//! consistent with `as_of_revision`. The compiler never serializes "the
//! database": irrelevant conversations, hidden conversations and over-budget
//! items are excluded and counted in `ContextStats` for the debugger view.

pub mod budget;
pub mod compiler;
pub mod intent;
pub mod pack;

pub use compiler::{CompilerConfig, ContextCompiler};
pub use intent::analyze;
pub use pack::*;
