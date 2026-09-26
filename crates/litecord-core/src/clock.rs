//! Injectable time source so schedulers and policies are deterministic under
//! test.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use litecord_types::{DurationMs, Timestamp};

pub trait Clock: Send + Sync + std::fmt::Debug + 'static {
    fn now(&self) -> Timestamp;
}

/// Wall clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        Timestamp::now()
    }
}

/// Manually advanced clock for tests. Cloning shares the same time.
#[derive(Debug, Clone, Default)]
pub struct ManualClock(Arc<AtomicI64>);

impl ManualClock {
    pub fn new(start: Timestamp) -> Self {
        Self(Arc::new(AtomicI64::new(start.as_millis())))
    }
    pub fn set(&self, t: Timestamp) {
        self.0.store(t.as_millis(), Ordering::SeqCst);
    }
    pub fn advance(&self, d: DurationMs) {
        self.0.fetch_add(d.as_millis() as i64, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(self.0.load(Ordering::SeqCst))
    }
}

/// Shared clock handle.
pub type SharedClock = Arc<dyn Clock>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_clock_advances() {
        let c = ManualClock::new(Timestamp(1_000));
        let c2 = c.clone();
        c.advance(DurationMs::from_secs(1));
        assert_eq!(c2.now(), Timestamp(2_000));
    }
}
