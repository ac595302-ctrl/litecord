//! Canonical domain types for Litecord.
//!
//! This crate is the bottom of the dependency graph. It contains **no I/O**, no
//! async runtime and no Discord SDK types. Everything else in the workspace
//! speaks in these types, and adapters convert *into* them at the boundary.
//!
//! Key invariants encoded here:
//!
//! * Identifiers are strong newtypes ([`ids`]); raw integers never cross crate
//!   boundaries.
//! * Every piece of knowledge carries an [`provenance::Origin`]. Canonical
//!   Discord state may only be produced from a [`provenance::DiscordSource`];
//!   there is deliberately no conversion from `AgentDerived` into a Discord
//!   source.
//! * [`trust::TrustLevel`] distinguishes instructions from external content;
//!   only [`trust::TrustLevel::can_authorize_actions`] levels may authorize.

#[macro_use]
mod macros;

pub mod actions;
pub mod capability;
pub mod entity;
pub mod error;
pub mod ids;
pub mod memory;
pub mod notes;
pub mod provenance;
pub mod revision;
pub mod social;
pub mod tasks;
pub mod time;
pub mod trust;

pub use error::ValidationError;
pub use ids::*;
pub use revision::{MemorySnapshot, Revision};
pub use time::{DurationMs, Timestamp};
