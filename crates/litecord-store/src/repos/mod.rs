//! Repositories: all SQL lives here, grouped by aggregate.
//!
//! Conventions:
//! * Read functions take `&Connection` (works with both [`crate::ReadTx`] and
//!   [`crate::WriteTx`] via `Deref`).
//! * Write functions take `&WriteTx` so they run inside a revisioned
//!   transaction, stamp `tx.revision()` on rows and `tx.emit(..)` the
//!   corresponding `UnifiedEvent` **only when a row actually changed**.
//! * Upserts use `ON CONFLICT .. DO UPDATE .. WHERE <something differs>` so
//!   identical re-observations are no-ops (no revision churn).

// Canonical social state
pub mod accounts;
pub mod app_state;
pub mod attachments;
pub mod channels;
pub mod conversations;
pub mod guilds;
pub mod lobbies;
pub mod messages;
pub mod relationships;
pub mod users;
pub mod voice;

// Event log and hydration bookkeeping
pub mod events;
pub mod hydration_jobs;
pub mod sync_state;

// Derived memory
pub mod edges;
pub mod embeddings;
pub mod local_entities;
pub mod memory;
pub mod summaries;

// Operational memory
pub mod drafts;
pub mod notes;
pub mod reminders;
pub mod settings;
pub mod tasks;

// Action engine and agents
pub mod actions;
pub mod agent_runs;

// Shared FTS helpers
pub mod fts;

/// Re-exported so dependent crates can name the connection type repository
/// functions accept without depending on rusqlite directly.
pub use rusqlite::Connection;
