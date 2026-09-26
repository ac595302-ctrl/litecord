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

use litecord_core::config::{CacheConfig, RetentionConfig};
use litecord_core::events::message_approx_bytes;
use litecord_core::metrics::{Gauge, Metrics};
use litecord_types::ids::MessageId;
use litecord_types::memory::MemoryTier;
use litecord_types::social::Message;
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

/// Size estimate for a cached value, in bytes.
pub type SizeFn<V> = fn(&V) -> usize;

/// A bounded, in-memory LRU cache for the HOT tier.
///
/// This is the *only* place unified memory keeps state in RAM: everything
/// else is read from SQLite on demand. Capacity is fixed at construction (a
/// capacity of `0` is coerced to `1`, since [`lru::LruCache`] cannot be
/// zero-sized), so the cache can never grow past its budget regardless of
/// how much gets `put` into it. When a [`Gauge`] is supplied, it is kept in
/// sync with `len()` on every mutation, so the cache's live size is visible
/// as a metric without callers polling it themselves.
///
/// Optionally ([`HotCache::with_byte_limit`]) the cache is also bounded by an
/// approximate byte ceiling: after every insert, least-recently-used entries
/// are evicted until it is under both the item and the byte limit. A single
/// value larger than the whole ceiling is therefore not retained.
pub struct HotCache<K: Hash + Eq, V: Clone> {
    inner: lru::LruCache<K, V>,
    gauge: Option<Arc<Gauge>>,
    max_bytes: Option<usize>,
    size_of: Option<SizeFn<V>>,
    bytes: usize,
    bytes_gauge: Option<Arc<Gauge>>,
}

impl<K: Hash + Eq, V: Clone> std::fmt::Debug for HotCache<K, V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HotCache")
            .field("len", &self.inner.len())
            .field("capacity", &self.inner.cap())
            .field("bytes", &self.bytes)
            .field("max_bytes", &self.max_bytes)
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
            max_bytes: None,
            size_of: None,
            bytes: 0,
            bytes_gauge: None,
        }
    }

    /// Additionally bound the cache by `max_bytes` as measured by `size_of`.
    /// `bytes_gauge`, if given, tracks [`HotCache::bytes`]. Call on an empty
    /// cache (typically right after [`HotCache::new`]).
    pub fn with_byte_limit(
        mut self,
        max_bytes: usize,
        size_of: SizeFn<V>,
        bytes_gauge: Option<Arc<Gauge>>,
    ) -> Self {
        self.max_bytes = Some(max_bytes);
        self.size_of = Some(size_of);
        self.bytes_gauge = bytes_gauge;
        self.bytes = self.inner.iter().map(|(_, v)| size_of(v)).sum();
        self.evict_over_budget();
        self.sync_gauge();
        self
    }

    fn size(&self, v: &V) -> usize {
        self.size_of.map_or(0, |f| f(v))
    }

    fn sync_gauge(&self) {
        if let Some(g) = &self.gauge {
            g.set(self.inner.len() as u64);
        }
        if let Some(g) = &self.bytes_gauge {
            g.set(self.bytes as u64);
        }
    }

    fn evict_over_budget(&mut self) {
        let Some(max) = self.max_bytes else { return };
        while self.bytes > max {
            match self.inner.pop_lru() {
                Some((_, v)) => self.bytes = self.bytes.saturating_sub(self.size(&v)),
                None => {
                    self.bytes = 0;
                    break;
                }
            }
        }
    }

    /// Look up `key`, marking it most-recently-used on a hit. Returns a
    /// clone since the cache retains ownership.
    pub fn get(&mut self, key: &K) -> Option<V> {
        // `get` alone does not change the entry count, so no gauge update is
        // needed, but promotion is still a mutation of internal order.
        self.inner.get(key).cloned()
    }

    /// Insert or update `key`, evicting least-recently-used entries if the
    /// cache is over its item or byte limit. Returns the previous value if
    /// `key` was already present, matching [`lru::LruCache::put`].
    pub fn put(&mut self, key: K, value: V) -> Option<V> {
        let replacing = self.inner.contains(&key);
        self.bytes = self.bytes.saturating_add(self.size(&value));
        let old = match self.inner.push(key, value) {
            Some((_, displaced)) => {
                self.bytes = self.bytes.saturating_sub(self.size(&displaced));
                // Without `replacing`, `displaced` is the evicted LRU entry.
                replacing.then_some(displaced)
            }
            None => None,
        };
        self.evict_over_budget();
        self.sync_gauge();
        old
    }

    /// Remove `key` if present, returning its value.
    pub fn remove(&mut self, key: &K) -> Option<V> {
        let removed = self.inner.pop(key);
        if let Some(v) = &removed {
            self.bytes = self.bytes.saturating_sub(self.size(v));
        }
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
        self.bytes = 0;
        self.sync_gauge();
    }

    /// The fixed capacity this cache was constructed with.
    pub fn capacity(&self) -> usize {
        self.inner.cap().get()
    }

    /// Approximate bytes currently held (always `0` without a byte limit).
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// The byte ceiling, if any.
    pub fn max_bytes(&self) -> Option<usize> {
        self.max_bytes
    }
}

/// Size estimate for a cached [`Message`] value (content, extras and a fixed
/// overhead); see [`litecord_core::events::message_approx_bytes`].
pub fn message_bytes(message: &Message) -> usize {
    message_approx_bytes(message)
}

/// Build the message hot cache from [`CacheConfig`]: bounded by
/// `max_messages` entries and `max_message_cache_bytes`, reporting its entry
/// count as the `messages` cache gauge and its bytes as
/// [`Metrics::hot_cache_bytes`].
pub fn message_hot_cache(cfg: &CacheConfig, metrics: &Metrics) -> HotCache<MessageId, Message> {
    HotCache::new(cfg.max_messages, Some(metrics.cache_gauge("messages"))).with_byte_limit(
        cfg.max_message_cache_bytes,
        message_bytes,
        Some(metrics.hot_cache_bytes.clone()),
    )
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
    fn hot_cache_evicts_lru_by_bytes() {
        let bytes_gauge = Arc::new(Gauge::default());
        let mut cache: HotCache<u32, String> = HotCache::new(100, None).with_byte_limit(
            10,
            |s: &String| s.len(),
            Some(bytes_gauge.clone()),
        );
        cache.put(1, "aaaa".into());
        cache.put(2, "bbbb".into());
        assert_eq!(cache.bytes(), 8);
        assert_eq!(bytes_gauge.get(), 8);
        assert!(cache.get(&1).is_some()); // 2 is now LRU
        cache.put(3, "cccc".into()); // 12 > 10: evict 2
        assert_eq!(cache.bytes(), 8);
        assert_eq!(cache.get(&2), None);
        assert!(cache.get(&1).is_some() && cache.get(&3).is_some());

        // Replacing a key accounts for the old value's size.
        assert_eq!(cache.put(3, "c".into()), Some("cccc".into()));
        assert_eq!(cache.bytes(), 5);

        // A value larger than the whole ceiling is not retained.
        cache.put(4, "x".repeat(11));
        assert!(cache.is_empty());
        assert_eq!(cache.bytes(), 0);
        assert_eq!(bytes_gauge.get(), 0);

        cache.put(5, "abc".into());
        cache.remove(&5);
        assert_eq!(cache.bytes(), 0);
    }

    #[test]
    fn hot_cache_item_eviction_releases_bytes() {
        let mut cache: HotCache<u32, String> =
            HotCache::new(2, None).with_byte_limit(1_000, |s: &String| s.len(), None);
        cache.put(1, "aa".into());
        cache.put(2, "bbb".into());
        cache.put(3, "c".into()); // evicts 1 by count
        assert_eq!(cache.len(), 2);
        assert_eq!(cache.bytes(), 4);
        cache.clear();
        assert_eq!(cache.bytes(), 0);
    }

    #[test]
    fn message_hot_cache_uses_config_and_metrics() {
        let metrics = Metrics::new();
        let cfg = CacheConfig::default();
        let mut cache = message_hot_cache(&cfg, &metrics);
        assert_eq!(cache.max_bytes(), Some(cfg.max_message_cache_bytes));
        let m = Message {
            id: MessageId(1),
            conversation_id: litecord_types::ids::ConversationId(1),
            author_id: litecord_types::ids::UserId(1),
            content: "hello".into(),
            sent_at: Timestamp::from_millis(0),
            edited_at: None,
            reply_to: None,
            extras: Vec::new(),
        };
        cache.put(m.id, m.clone());
        assert_eq!(cache.bytes(), message_bytes(&m));
        let snap = metrics.snapshot();
        assert_eq!(snap.hot_cache_bytes, message_bytes(&m) as u64);
        assert_eq!(snap.cache_sizes.get("messages"), Some(&1));
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
