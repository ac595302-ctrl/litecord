//! Time primitives. All persisted times are UTC milliseconds since the Unix
//! epoch; presentation-layer time zones are a UI concern.

use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Milliseconds since the Unix epoch (UTC).
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(transparent)]
pub struct Timestamp(pub i64);

/// A span of time in milliseconds. Kept separate from `std::time::Duration`
/// so it can be negative-free, serialized compactly, and stored in SQLite.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(transparent)]
pub struct DurationMs(pub u64);

impl Timestamp {
    pub const EPOCH: Timestamp = Timestamp(0);

    /// Wall-clock now. Prefer an injected clock (`litecord_core::clock`) in
    /// logic that needs to be deterministic under test.
    pub fn now() -> Self {
        let ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        Timestamp(ms)
    }

    pub const fn from_millis(ms: i64) -> Self {
        Timestamp(ms)
    }

    pub const fn as_millis(self) -> i64 {
        self.0
    }

    pub fn saturating_add(self, d: DurationMs) -> Self {
        Timestamp(self.0.saturating_add(d.0.min(i64::MAX as u64) as i64))
    }

    pub fn saturating_sub(self, d: DurationMs) -> Self {
        Timestamp(self.0.saturating_sub(d.0.min(i64::MAX as u64) as i64))
    }

    /// Elapsed time from `earlier` to `self`, zero if `earlier` is later.
    pub fn since(self, earlier: Timestamp) -> DurationMs {
        DurationMs(self.0.saturating_sub(earlier.0).max(0) as u64)
    }
}

impl fmt::Debug for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Timestamp({}ms)", self.0)
    }
}

impl DurationMs {
    pub const ZERO: DurationMs = DurationMs(0);

    pub const fn from_millis(ms: u64) -> Self {
        DurationMs(ms)
    }
    pub const fn from_secs(s: u64) -> Self {
        DurationMs(s.saturating_mul(1_000))
    }
    pub const fn from_mins(m: u64) -> Self {
        DurationMs(m.saturating_mul(60_000))
    }
    pub const fn from_hours(h: u64) -> Self {
        DurationMs(h.saturating_mul(3_600_000))
    }
    pub const fn from_days(d: u64) -> Self {
        DurationMs(d.saturating_mul(86_400_000))
    }
    pub const fn as_millis(self) -> u64 {
        self.0
    }
    pub fn to_std(self) -> std::time::Duration {
        std::time::Duration::from_millis(self.0)
    }
}

impl fmt::Debug for DurationMs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}ms", self.0)
    }
}

impl From<std::time::Duration> for DurationMs {
    fn from(d: std::time::Duration) -> Self {
        DurationMs(d.as_millis().min(u64::MAX as u128) as u64)
    }
}
