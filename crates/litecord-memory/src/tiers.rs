//! Hot/warm/cold memory tiers (V2 §13).
//!
//! * **HOT** is the only tier that lives in RAM, in a bounded
//!   [`HotCache`]. It exists purely to skip a SQLite round-trip for the
//!   handful of things read on every turn (the active conversation window,
//!   recently used users, ...); nothing is ever *authoritative* there.
//! * **WARM** and **COLD** are not separate storage: they are read straight
//!   from SQLite on demand, filtered/ranked by age via [`classify`]. COLD
//!   items are excluded from default retrieval but never deleted by tiering
//!   alone (see [`crate::service::MemoryService::apply_retention`] for actual
//!   deletion policy).
//!
//! Because HOT is capacity-bounded by construction (an [`lru::LruCache`]
//! behind [`HotCache`]), it can never grow unbounded even under sustained
//! load; eviction is plain least-recently-used.

use std::hash::Hash;
use std::num::NonZeroUsize;
use std::sync::Arc;

use litecord_core::config::RetentionConfig;
use litecord_core::metrics::Gauge;
use litecord_types::memory::MemoryTier;
use litecord_types::Timestamp;

/// Classify an item's age into a [`MemoryTier`] relative to `now`.
///
/// `Hot` if younger than `cfg.hot_hours`, `Warm` if younger than
/// `cfg.warm_days`, otherwise `Cold`. An `at` in the future (clock skew,
/// racing writes) is treated as age zero, i.e. `Hot`.
pub fn classify(at: Timestamp, now: Timestamp, cfg: &RetentionConfig) -> MemoryTier {
    let age = now.since(at);
    if age.as_millis() < litecord_types::DurationMs::from_hours(cfg.hot_hours as u64).as_millis() {
        MemoryTier::Hot
    } else if age.as_millis()
        < litecord_types::DurationMs::from_days(cfg.warm_days as u64).as_millis()
    {
        MemoryTier::Warm
    } else {
        MemoryTier::Cold
    }
}

/// A bounded, in-memory LRU cache for the HOT tier.
///
/// This is the *only* place unified memory keeps state in RAM: everything
/// else is read from SQLite on demand. Capacity is fixed at construction (a
/// capacity of `0` is coerced to `1`, since [`lru::LruCache`] cannot be
/// zero-sized), so the cache can never grow past its budget regardless of
/// how much gets `put` into it. When a [`Gauge`] is supplied, it is kept in
/// sync with `len()` on every mutation, so the cache's live size is visible
/// as a metric without callers polling it themselves.
pub struct HotCache<K: Hash + Eq, V: Clone> {
    inner: lru::LruCache<K, V>,
    gauge: Option<Arc<Gauge>>,
}

impl<K: Hash + Eq, V: Clone> std::fmt::Debug for HotCache<K, V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HotCache")
            .field("len", &self.inner.len())
            .field("capacity", &self.inner.cap())
            .finish()
    }
}

impl<K: Hash + Eq, V: Clone> HotCache<K, V> {
    /// Create a cache holding at most `capacity` entries (coerced up to 1).
    /// `gauge`, if given, is set to `0` immediately and updated on every
    /// subsequent mutation.
    pub fn new(capacity: usize, gauge: Option<Arc<Gauge>>) -> Self {
        let cap = NonZeroUsize::new(capacity).unwrap_or(NonZeroUsize::MIN);
        if let Some(g) = &gauge {
            g.set(0);
        }
        Self {
            inner: lru::LruCache::new(cap),
            gauge,
        }
    }

    fn sync_gauge(&self) {
        if let Some(g) = &self.gauge {
            g.set(self.inner.len() as u64);
        }
    }

    /// Look up `key`, marking it most-recently-used on a hit. Returns a
    /// clone since the cache retains ownership.
    pub fn get(&mut self, key: &K) -> Option<V> {
        // `get` alone does not change the entry count, so no gauge update is
        // needed, but promotion is still a mutation of internal order.
        self.inner.get(key).cloned()
    }

    /// Insert or update `key`, evicting the least-recently-used entry first
    /// if the cache is at capacity. Returns the evicted value's old value if
    /// `key` was already present, matching [`lru::LruCache::put`].
    pub fn put(&mut self, key: K, value: V) -> Option<V> {
        let old = self.inner.put(key, value);
        self.sync_gauge();
        old
    }

    /// Remove `key` if present, returning its value.
    pub fn remove(&mut self, key: &K) -> Option<V> {
        let removed = self.inner.pop(key);
        self.sync_gauge();
        removed
    }

    /// Current number of entries.
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Whether the cache currently holds no entries.
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Remove every entry.
    pub fn clear(&mut self) {
        self.inner.clear();
        self.sync_gauge();
    }

    /// The fixed capacity this cache was constructed with.
    pub fn capacity(&self) -> usize {
        self.inner.cap().get()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> RetentionConfig {
        RetentionConfig {
            hot_hours: 24,
            warm_days: 14,
            ..RetentionConfig::default()
        }
    }

    #[test]
    fn classify_boundaries() {
        let cfg = cfg();
        let now = Timestamp::from_millis(1_000_000_000);
        let hot = now.saturating_sub(litecord_types::DurationMs::from_hours(1));
        let warm = now.saturating_sub(litecord_types::DurationMs::from_days(2));
        let cold = now.saturating_sub(litecord_types::DurationMs::from_days(30));
        assert_eq!(classify(hot, now, &cfg), MemoryTier::Hot);
        assert_eq!(classify(warm, now, &cfg), MemoryTier::Warm);
        assert_eq!(classify(cold, now, &cfg), MemoryTier::Cold);
        // exactly at the hot boundary is no longer hot
        let at_boundary = now.saturating_sub(litecord_types::DurationMs::from_hours(24));
        assert_eq!(classify(at_boundary, now, &cfg), MemoryTier::Warm);
        let future = now.saturating_add(litecord_types::DurationMs::from_hours(1));
        assert_eq!(classify(future, now, &cfg), MemoryTier::Hot);
    }

    #[test]
    fn hot_cache_evicts_lru_and_tracks_gauge() {
        let gauge = Arc::new(Gauge::default());
        let mut cache: HotCache<u32, &'static str> = HotCache::new(2, Some(gauge.clone()));
        assert_eq!(gauge.get(), 0);

        cache.put(1, "a");
        cache.put(2, "b");
        assert_eq!(gauge.get(), 2);

        // touch 1 so it becomes most-recently-used
        assert_eq!(cache.get(&1), Some("a"));
        cache.put(3, "c"); // evicts 2 (LRU), not 1
        assert_eq!(cache.len(), 2);
        assert_eq!(gauge.get(), 2);
        assert_eq!(cache.get(&2), None);
        assert_eq!(cache.get(&1), Some("a"));
        assert_eq!(cache.get(&3), Some("c"));

        cache.remove(&1);
        assert_eq!(gauge.get(), 1);

        cache.clear();
        assert!(cache.is_empty());
        assert_eq!(gauge.get(), 0);
    }

    #[test]
    fn zero_capacity_is_coerced_to_one() {
        let mut cache: HotCache<u32, u32> = HotCache::new(0, None);
        assert_eq!(cache.capacity(), 1);
        cache.put(1, 1);
        cache.put(2, 2);
        assert_eq!(cache.len(), 1);
    }
}
