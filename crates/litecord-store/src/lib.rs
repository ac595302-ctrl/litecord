//! SQLite persistence for Litecord.
//!
//! * [`Database`] owns connections and enforces the revision model: every
//!   [`Database::write`] runs in one `BEGIN IMMEDIATE` transaction that
//!   advances the global revision by exactly one and appends any emitted
//!   [`litecord_core::events::UnifiedEvent`]s to the event log. A failed
//!   closure rolls back — including the revision bump.
//! * [`Database::read`] runs in a deferred read transaction: everything read
//!   inside it is consistent with [`ReadTx::revision`] (WAL snapshot).
//! * [`repos`] contains all SQL. Application logic calls typed repository
//!   functions and never writes SQL itself.
//! * [`reducer`] is the single path from [`litecord_core::events::SourceEnvelope`]
//!   to canonical tables.
//!
//! Revisions are read from and written to the database (not cached), so a
//! separate process (e.g. the MCP server) writing to the same file keeps the
//! revision sequence consistent.

mod db;
mod error;
pub mod migrations;
pub mod reducer;
pub mod repos;
mod sql;

pub use db::{Committed, Database, ReadTx, WriteTx};
pub use error::{StoreError, StoreResult};
