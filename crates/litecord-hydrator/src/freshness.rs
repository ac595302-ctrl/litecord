//! Staleness model and the [`FreshnessStore`] port.
//!
//! This module intentionally has no persistence of its own beyond
//! [`InMemoryFreshnessStore`] (for tests and ephemeral/demo use). A
//! SQLite-backed implementation over `litecord-store`'s `sync_state`
//! repository is expected to be added later by whoever wires this crate into
//! the app; this crate does not depend on `litecord-store`.

use std::collections::HashMap;
use std::sync::Mutex;

use litecord_core::config::HydrationConfig;
use litecord_core::events::HydrationKey;
use litecord_types::{DurationMs, Timestamp};

use crate::error::HydrationError;

/// What is known about how fresh a [`HydrationKey`]'s data is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Freshness {
    /// When this key was last successfully hydrated. `None` means never.
    pub observed_at: Option<Timestamp>,
    /// How long an observation stays fresh.
    pub stale_after: DurationMs,
    /// Forced stale regardless of `observed_at` (e.g. after a resync).
    pub dirty: bool,
    /// Consecutive hydration failures recorded since the last success.
    pub failure_count: u32,
}

impl Freshness {
    /// `dirty`, never observed, or the staleness window has elapsed.
    pub fn is_stale(&self, now: Timestamp) -> bool {
        if self.dirty {
            return true;
        }
        match self.observed_at {
            None => true,
            Some(observed_at) => now.since(observed_at) >= self.stale_after,
        }
    }
}

/// Per-[`HydrationKey`]-kind staleness windows, derived from
/// [`HydrationConfig`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StalenessPolicy {
    current_user: DurationMs,
    relationships: DurationMs,
    guilds: DurationMs,
    channels: DurationMs,
    conversations: DurationMs,
    lobby_and_voice: DurationMs,
}

impl StalenessPolicy {
    pub fn stale_after(&self, key: &HydrationKey) -> DurationMs {
        match key {
            HydrationKey::CurrentUser => self.current_user,
            HydrationKey::Relationships => self.relationships,
            HydrationKey::Guilds => self.guilds,
            HydrationKey::GuildChannels { .. } => self.channels,
            HydrationKey::DmSummaries | HydrationKey::DmConversation { .. } => self.conversations,
            // A single user's profile is refreshed on the same cadence as
            // the relationships list that surfaces it.
            HydrationKey::User { .. } => self.relationships,
            HydrationKey::Lobby { .. } | HydrationKey::VoiceState => self.lobby_and_voice,
        }
    }
}

impl From<&HydrationConfig> for StalenessPolicy {
    fn from(cfg: &HydrationConfig) -> Self {
        Self {
            current_user: DurationMs::from_secs(cfg.stale_after_current_user_secs),
            relationships: DurationMs::from_secs(cfg.stale_after_relationships_secs),
            guilds: DurationMs::from_secs(cfg.stale_after_guilds_secs),
            channels: DurationMs::from_secs(cfg.stale_after_channels_secs),
            conversations: DurationMs::from_secs(cfg.stale_after_conversations_secs),
            lobby_and_voice: DurationMs::from_mins(5),
        }
    }
}

/// Persistence port for [`Freshness`] records, keyed by [`HydrationKey`].
pub trait FreshnessStore: Send + Sync + std::fmt::Debug {
    fn get(&self, key: &HydrationKey) -> Result<Option<Freshness>, HydrationError>;
    fn list(&self) -> Result<Vec<(HydrationKey, Freshness)>, HydrationError>;
    fn mark_fresh(
        &self,
        key: &HydrationKey,
        at: Timestamp,
        stale_after: DurationMs,
    ) -> Result<(), HydrationError>;
    fn mark_dirty(&self, key: &HydrationKey, stale_after: DurationMs)
        -> Result<(), HydrationError>;
    fn mark_all_dirty(&self) -> Result<(), HydrationError>;
    fn record_failure(
        &self,
        key: &HydrationKey,
        error: &str,
        stale_after: DurationMs,
    ) -> Result<(), HydrationError>;
}

fn empty(stale_after: DurationMs) -> Freshness {
    Freshness {
        observed_at: None,
        stale_after,
        dirty: false,
        failure_count: 0,
    }
}

/// In-memory [`FreshnessStore`], for tests and ephemeral/demo use.
#[derive(Debug, Default)]
pub struct InMemoryFreshnessStore {
    inner: Mutex<HashMap<HydrationKey, Freshness>>,
}

impl InMemoryFreshnessStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<HydrationKey, Freshness>> {
        match self.inner.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

impl FreshnessStore for InMemoryFreshnessStore {
    fn get(&self, key: &HydrationKey) -> Result<Option<Freshness>, HydrationError> {
        Ok(self.lock().get(key).copied())
    }

    fn list(&self) -> Result<Vec<(HydrationKey, Freshness)>, HydrationError> {
        Ok(self.lock().iter().map(|(k, v)| (*k, *v)).collect())
    }

    fn mark_fresh(
        &self,
        key: &HydrationKey,
        at: Timestamp,
        stale_after: DurationMs,
    ) -> Result<(), HydrationError> {
        let mut guard = self.lock();
        let entry = guard.entry(*key).or_insert_with(|| empty(stale_after));
        entry.observed_at = Some(at);
        entry.stale_after = stale_after;
        entry.dirty = false;
        entry.failure_count = 0;
        Ok(())
    }

    fn mark_dirty(
        &self,
        key: &HydrationKey,
        stale_after: DurationMs,
    ) -> Result<(), HydrationError> {
        let mut guard = self.lock();
        let entry = guard.entry(*key).or_insert_with(|| empty(stale_after));
        entry.dirty = true;
        entry.stale_after = stale_after;
        Ok(())
    }

    fn mark_all_dirty(&self) -> Result<(), HydrationError> {
        let mut guard = self.lock();
        for freshness in guard.values_mut() {
            freshness.dirty = true;
        }
        Ok(())
    }

    fn record_failure(
        &self,
        key: &HydrationKey,
        error: &str,
        stale_after: DurationMs,
    ) -> Result<(), HydrationError> {
        let mut guard = self.lock();
        let entry = guard.entry(*key).or_insert_with(|| empty(stale_after));
        entry.failure_count += 1;
        entry.stale_after = stale_after;
        tracing::debug!(key = ?key, error, failure_count = entry.failure_count, "hydration failure recorded");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dirty_is_always_stale() {
        let f = Freshness {
            observed_at: Some(Timestamp::from_millis(1_000)),
            stale_after: DurationMs::from_secs(3_600),
            dirty: true,
            failure_count: 0,
        };
        assert!(f.is_stale(Timestamp::from_millis(1_001)));
    }

    #[test]
    fn never_observed_is_stale() {
        let f = Freshness {
            observed_at: None,
            stale_after: DurationMs::from_secs(3_600),
            dirty: false,
            failure_count: 0,
        };
        assert!(f.is_stale(Timestamp::from_millis(0)));
    }

    #[test]
    fn elapsed_window_determines_staleness() {
        let f = Freshness {
            observed_at: Some(Timestamp::from_millis(0)),
            stale_after: DurationMs::from_secs(10),
            dirty: false,
            failure_count: 0,
        };
        assert!(!f.is_stale(Timestamp::from_millis(9_999)));
        assert!(f.is_stale(Timestamp::from_millis(10_000)));
    }

    #[test]
    fn in_memory_store_roundtrips() {
        let store = InMemoryFreshnessStore::new();
        let key = HydrationKey::Guilds;
        assert_eq!(store.get(&key).unwrap(), None);

        store
            .mark_fresh(&key, Timestamp::from_millis(100), DurationMs::from_secs(60))
            .unwrap();
        let f = store.get(&key).unwrap().unwrap();
        assert_eq!(f.observed_at, Some(Timestamp::from_millis(100)));
        assert!(!f.dirty);

        store.mark_dirty(&key, DurationMs::from_secs(60)).unwrap();
        assert!(store.get(&key).unwrap().unwrap().dirty);

        store
            .record_failure(&key, "boom", DurationMs::from_secs(60))
            .unwrap();
        assert_eq!(store.get(&key).unwrap().unwrap().failure_count, 1);

        store
            .mark_fresh(&key, Timestamp::from_millis(200), DurationMs::from_secs(60))
            .unwrap();
        assert_eq!(store.get(&key).unwrap().unwrap().failure_count, 0);

        let other = HydrationKey::Relationships;
        store
            .mark_fresh(&other, Timestamp::from_millis(1), DurationMs::from_secs(60))
            .unwrap();
        store.mark_all_dirty().unwrap();
        assert!(store.get(&key).unwrap().unwrap().dirty);
        assert!(store.get(&other).unwrap().unwrap().dirty);
        assert_eq!(store.list().unwrap().len(), 2);
    }

    #[test]
    fn staleness_policy_maps_keys_to_configured_windows() {
        let cfg = HydrationConfig {
            stale_after_current_user_secs: 10,
            stale_after_relationships_secs: 20,
            stale_after_guilds_secs: 30,
            stale_after_channels_secs: 40,
            stale_after_conversations_secs: 50,
            ..HydrationConfig::default()
        };
        let policy = StalenessPolicy::from(&cfg);
        assert_eq!(
            policy.stale_after(&HydrationKey::CurrentUser),
            DurationMs::from_secs(10)
        );
        assert_eq!(
            policy.stale_after(&HydrationKey::Relationships),
            DurationMs::from_secs(20)
        );
        assert_eq!(
            policy.stale_after(&HydrationKey::Guilds),
            DurationMs::from_secs(30)
        );
        assert_eq!(
            policy.stale_after(&HydrationKey::GuildChannels {
                guild_id: litecord_types::ids::GuildId(1)
            }),
            DurationMs::from_secs(40)
        );
        assert_eq!(
            policy.stale_after(&HydrationKey::DmSummaries),
            DurationMs::from_secs(50)
        );
        assert_eq!(
            policy.stale_after(&HydrationKey::User {
                user_id: litecord_types::ids::UserId(1)
            }),
            DurationMs::from_secs(20)
        );
        assert_eq!(
            policy.stale_after(&HydrationKey::VoiceState),
            DurationMs::from_mins(5)
        );
    }
}
