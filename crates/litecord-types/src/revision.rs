//! The monotonic revision model.
//!
//! Every committed write transaction against the Litecord store advances the
//! global revision by exactly one. Rows written in that transaction record the
//! revision, so any reader can answer "what did the world look like at N?" at
//! the granularity needed for agent consistency:
//!
//! * a `ContextPack` records the revision it was compiled from;
//! * an action proposal records the revision it was based on;
//! * before execution the Action Engine compares against the current revision
//!   and re-validates when state has advanced.
//!
//! Revisions are **not** tied to the AI layer; they exist for the UI and for
//! hydration as well.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::Timestamp;

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(transparent)]
pub struct Revision(pub u64);

impl Revision {
    pub const ZERO: Revision = Revision(0);

    pub const fn get(self) -> u64 {
        self.0
    }
    pub const fn next(self) -> Revision {
        Revision(self.0 + 1)
    }
    /// True when `self` is strictly newer than `other`.
    pub fn is_after(self, other: Revision) -> bool {
        self.0 > other.0
    }
}

impl fmt::Debug for Revision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "r{}", self.0)
    }
}

impl fmt::Display for Revision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A point-in-time handle on unified memory. Produced by a read transaction;
/// everything read within that transaction is consistent with `revision`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemorySnapshot {
    pub revision: Revision,
    pub created_at: Timestamp,
}

impl MemorySnapshot {
    /// Whether state has advanced since this snapshot was taken.
    pub fn is_stale_against(&self, current: Revision) -> bool {
        current.is_after(self.revision)
    }
}
