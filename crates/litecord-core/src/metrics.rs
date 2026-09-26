//! Lightweight in-process metrics for the future performance overlay.
//!
//! Only real measurements are reported: gauges are set by their owners,
//! timings are recorded around real operations, RSS is read from the OS (and
//! is `None` where unsupported rather than estimated).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;

#[derive(Debug, Default)]
pub struct Gauge(AtomicU64);

impl Gauge {
    pub fn set(&self, v: u64) {
        self.0.store(v, Ordering::Relaxed);
    }
    pub fn get(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }
}

#[derive(Debug, Default)]
pub struct Counter(AtomicU64);

impl Counter {
    pub fn inc(&self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
    pub fn add(&self, n: u64) {
        self.0.fetch_add(n, Ordering::Relaxed);
    }
    pub fn get(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }
}

/// Count / total / max of an operation's duration.
#[derive(Debug, Default)]
pub struct Timing {
    count: AtomicU64,
    total_us: AtomicU64,
    max_us: AtomicU64,
}

impl Timing {
    pub fn record(&self, d: Duration) {
        let us = d.as_micros().min(u64::MAX as u128) as u64;
        self.count.fetch_add(1, Ordering::Relaxed);
        self.total_us.fetch_add(us, Ordering::Relaxed);
        self.max_us.fetch_max(us, Ordering::Relaxed);
    }

    /// Time a closure.
    pub fn time<T>(&self, f: impl FnOnce() -> T) -> T {
        let start = Instant::now();
        let out = f();
        self.record(start.elapsed());
        out
    }

    pub fn snapshot(&self) -> TimingSnapshot {
        let count = self.count.load(Ordering::Relaxed);
        let total = self.total_us.load(Ordering::Relaxed);
        TimingSnapshot {
            count,
            mean_us: total.checked_div(count).unwrap_or(0),
            max_us: self.max_us.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
pub struct TimingSnapshot {
    pub count: u64,
    pub mean_us: u64,
    pub max_us: u64,
}

/// Process-wide metrics registry. Share via `Arc<Metrics>`.
#[derive(Debug, Default)]
pub struct Metrics {
    pub event_queue_depth: Gauge,
    pub events_ingested: Counter,
    pub events_dropped: Counter,
    pub hydration_queue_depth: Gauge,
    pub hydration_active: Gauge,
    pub hydration_failures: Counter,
    pub db_write: Timing,
    pub db_read: Timing,
    pub retrieval: Timing,
    pub context_compile: Timing,
    pub agent_run: Timing,
    pub action_execute: Timing,
    caches: Mutex<BTreeMap<String, Arc<Gauge>>>,
}

impl Metrics {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Register (or fetch) a named cache-size gauge.
    pub fn cache_gauge(&self, name: &str) -> Arc<Gauge> {
        let mut caches = match self.caches.lock() {
            Ok(c) => c,
            Err(poisoned) => poisoned.into_inner(),
        };
        caches.entry(name.to_owned()).or_default().clone()
    }

    pub fn snapshot(&self) -> MetricsSnapshot {
        let caches = match self.caches.lock() {
            Ok(c) => c.iter().map(|(k, v)| (k.clone(), v.get())).collect(),
            Err(p) => p
                .into_inner()
                .iter()
                .map(|(k, v)| (k.clone(), v.get()))
                .collect(),
        };
        MetricsSnapshot {
            rss_bytes: resident_set_size(),
            event_queue_depth: self.event_queue_depth.get(),
            events_ingested: self.events_ingested.get(),
            events_dropped: self.events_dropped.get(),
            hydration_queue_depth: self.hydration_queue_depth.get(),
            hydration_active: self.hydration_active.get(),
            hydration_failures: self.hydration_failures.get(),
            db_write: self.db_write.snapshot(),
            db_read: self.db_read.snapshot(),
            retrieval: self.retrieval.snapshot(),
            context_compile: self.context_compile.snapshot(),
            agent_run: self.agent_run.snapshot(),
            action_execute: self.action_execute.snapshot(),
            cache_sizes: caches,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MetricsSnapshot {
    /// `None` on platforms where RSS is not implemented.
    pub rss_bytes: Option<u64>,
    pub event_queue_depth: u64,
    pub events_ingested: u64,
    pub events_dropped: u64,
    pub hydration_queue_depth: u64,
    pub hydration_active: u64,
    pub hydration_failures: u64,
    pub db_write: TimingSnapshot,
    pub db_read: TimingSnapshot,
    pub retrieval: TimingSnapshot,
    pub context_compile: TimingSnapshot,
    pub agent_run: TimingSnapshot,
    pub action_execute: TimingSnapshot,
    pub cache_sizes: BTreeMap<String, u64>,
}

/// Resident set size of this process, if measurable.
pub fn resident_set_size() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        // /proc/self/statm: size resident shared text lib data dt (in pages).
        let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
        let resident_pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
        // Page size is 4 KiB on every Linux target we ship; querying sysconf
        // would require libc/unsafe for no practical gain.
        Some(resident_pages * 4096)
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timing_tracks_count_mean_max() {
        let t = Timing::default();
        t.record(Duration::from_micros(10));
        t.record(Duration::from_micros(30));
        let s = t.snapshot();
        assert_eq!(s.count, 2);
        assert_eq!(s.mean_us, 20);
        assert_eq!(s.max_us, 30);
    }

    #[test]
    fn cache_gauges_are_shared_by_name() {
        let m = Metrics::new();
        m.cache_gauge("users").set(5);
        assert_eq!(m.cache_gauge("users").get(), 5);
        assert_eq!(m.snapshot().cache_sizes.get("users"), Some(&5));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn rss_is_measured_on_linux() {
        assert!(resident_set_size().unwrap_or(0) > 0);
    }
}
