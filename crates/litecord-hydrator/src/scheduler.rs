//! Pure, synchronous, deterministic hydration scheduling.
//!
//! Nothing in this module performs I/O or reads the wall clock: every
//! operation that needs "now" takes a [`Timestamp`] parameter, so the whole
//! scheduler is unit-testable without a runtime and behaves identically under
//! test and in production.
//!
//! ## Data structures
//!
//! * A [`std::collections::BinaryHeap`] of [`Entry`] ordered by (priority
//!   desc, ready-at asc, sequence asc), used only to find the next candidate
//!   job cheaply. Entries are never mutated or removed in place; instead each
//!   key has a monotonically increasing generation counter, and an entry is
//!   only "live" if its generation matches the key's current generation in
//!   the `pending` index. Superseded entries are simply left in the heap and
//!   discarded (lazy deletion) the first time they are popped.
//! * A `pending` index (`HashMap<HydrationKey, PendingInfo>`) that is the
//!   single source of truth for what is currently queued, used for dedup,
//!   upgrade decisions and `pending_len`.
//! * An `active` index for jobs currently leased out to a worker via
//!   [`HydrationScheduler::next_ready`], used to detect and coalesce/rerun
//!   requests that arrive while a fetch for that key is already in flight.
//! * A `backoff` index recording retry state per key, consulted when
//!   computing a job's readiness time.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};

use serde::Serialize;

use litecord_core::config::HydrationConfig;
use litecord_core::events::HydrationKey;
use litecord_types::{DurationMs, Timestamp};

/// Urgency of a hydration request, ascending: [`Priority::Background`] is the
/// least urgent, [`Priority::Immediate`] the most.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    Background,
    Normal,
    High,
    Immediate,
}

/// Why a hydration was requested. Purely informational (logging, tracing,
/// deciding rerun-vs-coalesce behavior) — it never changes what gets fetched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HydrationReason {
    /// Application/session startup.
    Startup,
    /// A live event implied this key changed.
    Event,
    /// The UI has this key on screen right now.
    ActiveScreen,
    /// An agent asked to read this before acting.
    AgentRequest,
    /// The periodic staleness sweep.
    Reconciliation,
    /// The session just came back `Ready` after being offline/reconnecting.
    Reconnect,
    /// The user explicitly asked to refresh.
    UserRefresh,
    /// A backoff-scheduled retry of a previously failed job.
    Retry,
}

impl HydrationReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            HydrationReason::Startup => "startup",
            HydrationReason::Event => "event",
            HydrationReason::ActiveScreen => "active_screen",
            HydrationReason::AgentRequest => "agent_request",
            HydrationReason::Reconciliation => "reconciliation",
            HydrationReason::Reconnect => "reconnect",
            HydrationReason::UserRefresh => "user_refresh",
            HydrationReason::Retry => "retry",
        }
    }
}

/// A caller's request to (re)hydrate `key`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HydrationRequest {
    pub key: HydrationKey,
    pub priority: Priority,
    pub reason: HydrationReason,
    /// Do not consider this ready before this time (e.g. deliberate
    /// debouncing). `None` means "as soon as priority/backoff allow".
    pub not_before: Option<Timestamp>,
}

impl HydrationRequest {
    pub fn new(key: HydrationKey, priority: Priority, reason: HydrationReason) -> Self {
        Self {
            key,
            priority,
            reason,
            not_before: None,
        }
    }

    pub fn immediate(key: HydrationKey, reason: HydrationReason) -> Self {
        Self::new(key, Priority::Immediate, reason)
    }

    pub fn high(key: HydrationKey, reason: HydrationReason) -> Self {
        Self::new(key, Priority::High, reason)
    }

    pub fn normal(key: HydrationKey, reason: HydrationReason) -> Self {
        Self::new(key, Priority::Normal, reason)
    }

    pub fn background(key: HydrationKey, reason: HydrationReason) -> Self {
        Self::new(key, Priority::Background, reason)
    }

    pub fn with_not_before(mut self, not_before: Timestamp) -> Self {
        self.not_before = Some(not_before);
        self
    }
}

/// A request the scheduler has decided is ready to run right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HydrationJob {
    pub key: HydrationKey,
    pub priority: Priority,
    pub reason: HydrationReason,
    /// 1-based attempt number for this key (see [`BackoffPolicy`]).
    pub attempt: u32,
    /// When this job (or the request generation that produced it) was made.
    pub requested_at: Timestamp,
}

/// Exponential backoff, deterministic (no jitter here — jitter can be layered
/// on top by whoever calls [`BackoffPolicy::delay`], e.g. by perturbing the
/// resulting `Timestamp` before storing it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackoffPolicy {
    pub base: DurationMs,
    pub max: DurationMs,
    pub max_attempts: u32,
}

impl BackoffPolicy {
    /// `delay(attempt >= 1) = min(max, base * 2^(attempt - 1))`, saturating.
    pub fn delay(&self, attempt: u32) -> DurationMs {
        let attempt = attempt.max(1);
        let shift = (attempt - 1).min(63);
        let multiplier = 1u64.checked_shl(shift).unwrap_or(u64::MAX);
        let millis = self.base.as_millis().saturating_mul(multiplier);
        DurationMs::from_millis(millis.min(self.max.as_millis()))
    }
}

impl From<&HydrationConfig> for BackoffPolicy {
    fn from(cfg: &HydrationConfig) -> Self {
        Self {
            base: DurationMs::from_millis(cfg.backoff_base_ms),
            max: DurationMs::from_millis(cfg.backoff_max_ms),
            max_attempts: 8,
        }
    }
}

/// What [`HydrationScheduler::request`] did with a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestOutcome {
    /// Newly queued.
    Enqueued,
    /// Already pending at a lower priority/later `not_before`; raised in
    /// place.
    Upgraded,
    /// Already pending and this request did not improve on it.
    AlreadyPending,
    /// A fetch for this key is already in flight; it will be re-run once it
    /// completes.
    RerunScheduled,
    /// A fetch for this key is already in flight and this request does not
    /// warrant a rerun.
    Coalesced,
}

/// What [`HydrationScheduler::fail`] did with a failed job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailOutcome {
    /// Will be retried at this time.
    RetryAt(Timestamp),
    /// Not retryable, or attempts exhausted.
    GaveUp { attempts: u32 },
}

#[derive(Debug, Clone, Copy)]
struct BackoffState {
    attempts: u32,
    next_at: Timestamp,
}

#[derive(Debug, Clone, Copy)]
struct PendingInfo {
    priority: Priority,
    not_before: Option<Timestamp>,
    generation: u64,
}

#[derive(Debug, Clone, Copy)]
struct ActiveInfo {
    priority: Priority,
    reason: HydrationReason,
    attempt: u32,
    requested_at: Timestamp,
    /// Highest priority seen among requests that arrived while this key was
    /// active and warranted a rerun (`None` = no rerun requested).
    rerun: Option<Priority>,
}

/// One heap entry. May be stale (superseded by a later generation for the
/// same key); staleness is detected lazily on pop by comparing `generation`
/// against the key's current entry in `pending`.
#[derive(Debug, Clone)]
struct Entry {
    priority: Priority,
    ready_at: Timestamp,
    seq: u64,
    generation: u64,
    key: HydrationKey,
    reason: HydrationReason,
    requested_at: Timestamp,
}

impl PartialEq for Entry {
    fn eq(&self, other: &Self) -> bool {
        self.priority == other.priority && self.ready_at == other.ready_at && self.seq == other.seq
    }
}
impl Eq for Entry {}

impl Ord for Entry {
    fn cmp(&self, other: &Self) -> Ordering {
        // BinaryHeap is a max-heap; "greatest" == popped first == what we
        // want to run first: highest priority, then earliest ready_at, then
        // earliest seq (FIFO among otherwise-equal entries).
        self.priority
            .cmp(&other.priority)
            .then_with(|| other.ready_at.cmp(&self.ready_at))
            .then_with(|| other.seq.cmp(&self.seq))
    }
}
impl PartialOrd for Entry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Treats `None` as "earliest possible" (more ready than any `Some`).
fn not_before_key(nb: Option<Timestamp>) -> i64 {
    nb.map(Timestamp::as_millis).unwrap_or(i64::MIN)
}

/// Sentinel meaning "ready whenever priority/ordering allow" (no floor).
fn no_floor() -> Timestamp {
    Timestamp::from_millis(i64::MIN)
}

/// A pure, synchronous priority scheduler for hydration jobs.
///
/// Deduplicates by [`HydrationKey`]: at most one pending entry and one active
/// lease per key at any time. See the module docs for the internal indices.
#[derive(Debug)]
pub struct HydrationScheduler {
    heap: BinaryHeap<Entry>,
    pending: HashMap<HydrationKey, PendingInfo>,
    active: HashMap<HydrationKey, ActiveInfo>,
    backoff: HashMap<HydrationKey, BackoffState>,
    generations: HashMap<HydrationKey, u64>,
    policy: BackoffPolicy,
    paused: bool,
    seq: u64,
}

impl HydrationScheduler {
    pub fn new(policy: BackoffPolicy) -> Self {
        Self {
            heap: BinaryHeap::new(),
            pending: HashMap::new(),
            active: HashMap::new(),
            backoff: HashMap::new(),
            generations: HashMap::new(),
            policy,
            paused: false,
            seq: 0,
        }
    }

    fn next_generation(&mut self, key: HydrationKey) -> u64 {
        let g = self.generations.entry(key).or_insert(0);
        *g += 1;
        *g
    }

    fn ready_at_for(&self, key: &HydrationKey, not_before: Option<Timestamp>) -> Timestamp {
        let backoff_at = self.backoff.get(key).map(|b| b.next_at);
        match (not_before, backoff_at) {
            (Some(a), Some(b)) => a.max(b),
            (Some(a), None) => a,
            (None, Some(b)) => b,
            (None, None) => no_floor(),
        }
    }

    /// Push a fresh pending entry for `key`, replacing whatever was pending
    /// before (the caller is responsible for having decided this should
    /// happen).
    fn enqueue(
        &mut self,
        key: HydrationKey,
        priority: Priority,
        reason: HydrationReason,
        not_before: Option<Timestamp>,
        requested_at: Timestamp,
    ) {
        let generation = self.next_generation(key);
        let ready_at = self.ready_at_for(&key, not_before);
        self.seq += 1;
        let seq = self.seq;
        self.pending.insert(
            key,
            PendingInfo {
                priority,
                not_before,
                generation,
            },
        );
        self.heap.push(Entry {
            priority,
            ready_at,
            seq,
            generation,
            key,
            reason,
            requested_at,
        });
    }

    /// Request that `key` be hydrated. See the crate/module docs for the full
    /// dedup/upgrade/rerun rules.
    pub fn request(&mut self, req: HydrationRequest, now: Timestamp) -> RequestOutcome {
        let key = req.key;

        if let Some(active) = self.active.get_mut(&key) {
            if matches!(
                req.reason,
                HydrationReason::Event
                    | HydrationReason::UserRefresh
                    | HydrationReason::AgentRequest
            ) {
                active.rerun = Some(active.rerun.map_or(req.priority, |p| p.max(req.priority)));
                if req.reason == HydrationReason::UserRefresh {
                    self.backoff.remove(&key);
                }
                return RequestOutcome::RerunScheduled;
            }
            return RequestOutcome::Coalesced;
        }

        if req.reason == HydrationReason::UserRefresh {
            self.backoff.remove(&key);
        }

        if let Some(pending) = self.pending.get(&key).copied() {
            let higher_priority = req.priority > pending.priority;
            let earlier_not_before =
                not_before_key(req.not_before) < not_before_key(pending.not_before);
            if higher_priority || earlier_not_before {
                let new_priority = pending.priority.max(req.priority);
                let new_not_before =
                    if not_before_key(req.not_before) < not_before_key(pending.not_before) {
                        req.not_before
                    } else {
                        pending.not_before
                    };
                self.enqueue(key, new_priority, req.reason, new_not_before, now);
                return RequestOutcome::Upgraded;
            }
            return RequestOutcome::AlreadyPending;
        }

        self.enqueue(key, req.priority, req.reason, req.not_before, now);
        RequestOutcome::Enqueued
    }

    /// Pop the best ready job, if any, and lease it out as active. Returns
    /// `None` while paused, or when nothing is ready yet.
    pub fn next_ready(&mut self, now: Timestamp) -> Option<HydrationJob> {
        if self.paused {
            return None;
        }
        let mut stash = Vec::new();
        let mut result = None;
        while let Some(entry) = self.heap.pop() {
            let is_live =
                matches!(self.pending.get(&entry.key), Some(p) if p.generation == entry.generation);
            if !is_live {
                // Superseded or cancelled: lazily drop it.
                continue;
            }
            if entry.ready_at > now {
                stash.push(entry);
                continue;
            }
            self.pending.remove(&entry.key);
            let attempt = self.backoff.get(&entry.key).map_or(0, |b| b.attempts) + 1;
            self.active.insert(
                entry.key,
                ActiveInfo {
                    priority: entry.priority,
                    reason: entry.reason,
                    attempt,
                    requested_at: entry.requested_at,
                    rerun: None,
                },
            );
            result = Some(HydrationJob {
                key: entry.key,
                priority: entry.priority,
                reason: entry.reason,
                attempt,
                requested_at: entry.requested_at,
            });
            break;
        }
        for e in stash {
            self.heap.push(e);
        }
        result
    }

    /// Mark `key`'s active job as having succeeded: clears backoff, and if a
    /// rerun was requested while it was in flight, re-enqueues it (reason
    /// [`HydrationReason::Event`], at the highest priority any rerun request
    /// asked for).
    pub fn complete(&mut self, key: HydrationKey, now: Timestamp) {
        self.backoff.remove(&key);
        if let Some(active) = self.active.remove(&key) {
            if let Some(priority) = active.rerun {
                self.enqueue(key, priority, HydrationReason::Event, None, now);
            }
        }
    }

    /// Mark `key`'s active job as having failed. `retryable` should come from
    /// the backend error (see `BackendError::is_retryable`).
    pub fn fail(&mut self, key: HydrationKey, retryable: bool, now: Timestamp) -> FailOutcome {
        let Some(active) = self.active.remove(&key) else {
            return FailOutcome::GaveUp { attempts: 0 };
        };
        let attempts = active.attempt;
        if !retryable || attempts >= self.policy.max_attempts {
            self.backoff.remove(&key);
            return FailOutcome::GaveUp { attempts };
        }
        let next_at = now.saturating_add(self.policy.delay(attempts));
        self.backoff.insert(key, BackoffState { attempts, next_at });
        self.enqueue(key, active.priority, HydrationReason::Retry, None, now);
        FailOutcome::RetryAt(next_at)
    }

    /// The active job for `key` was aborted because the backend went
    /// offline: return it to pending without counting a failed attempt (and
    /// without disturbing any existing backoff state).
    pub fn release_offline(&mut self, key: HydrationKey) {
        if let Some(active) = self.active.remove(&key) {
            self.enqueue(
                key,
                active.priority,
                active.reason,
                None,
                active.requested_at,
            );
        }
    }

    /// Cancel a pending (not yet active) request. Returns `false` if `key`
    /// was not pending.
    pub fn cancel(&mut self, key: HydrationKey) -> bool {
        self.pending.remove(&key).is_some()
    }

    pub fn pause(&mut self) {
        self.paused = true;
    }

    pub fn resume(&mut self) {
        self.paused = false;
    }

    pub fn is_paused(&self) -> bool {
        self.paused
    }

    /// Number of live pending entries (stale heap entries are not counted).
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    pub fn active_len(&self) -> usize {
        self.active.len()
    }

    pub fn is_pending(&self, key: &HydrationKey) -> bool {
        self.pending.contains_key(key)
    }

    pub fn is_active(&self, key: &HydrationKey) -> bool {
        self.active.contains_key(key)
    }

    /// Earliest ready time among pending entries, or `None` if there is
    /// nothing pending or the scheduler is paused.
    pub fn next_wake(&self, _now: Timestamp) -> Option<Timestamp> {
        if self.paused || self.pending.is_empty() {
            return None;
        }
        self.pending
            .iter()
            .map(|(key, p)| self.ready_at_for(key, p.not_before))
            .min()
    }

    /// Snapshot of pending keys and their priority, highest priority first.
    pub fn pending_snapshot(&self) -> Vec<(HydrationKey, Priority)> {
        let mut v: Vec<_> = self.pending.iter().map(|(k, p)| (*k, p.priority)).collect();
        v.sort_by_key(|a| std::cmp::Reverse(a.1));
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sched() -> HydrationScheduler {
        HydrationScheduler::new(BackoffPolicy {
            base: DurationMs::from_secs(1),
            max: DurationMs::from_secs(300),
            max_attempts: 8,
        })
    }

    fn t(ms: i64) -> Timestamp {
        Timestamp::from_millis(ms)
    }

    #[test]
    fn dedup_two_requests_same_key_produce_one_job() {
        let mut s = sched();
        let key = HydrationKey::Guilds;
        assert_eq!(
            s.request(
                HydrationRequest::normal(key, HydrationReason::Startup),
                t(0)
            ),
            RequestOutcome::Enqueued
        );
        assert_eq!(
            s.request(
                HydrationRequest::normal(key, HydrationReason::Startup),
                t(0)
            ),
            RequestOutcome::AlreadyPending
        );
        assert_eq!(s.pending_len(), 1);
        assert!(s.next_ready(t(0)).is_some());
        assert!(s.next_ready(t(0)).is_none());
    }

    #[test]
    fn priority_ordering_ignores_insertion_order() {
        let mut s = sched();
        let bg = HydrationKey::Guilds;
        let imm = HydrationKey::CurrentUser;
        s.request(
            HydrationRequest::background(bg, HydrationReason::Reconciliation),
            t(0),
        );
        s.request(
            HydrationRequest::immediate(imm, HydrationReason::Startup),
            t(0),
        );
        let job = s.next_ready(t(0)).expect("a job should be ready");
        assert_eq!(job.key, imm);
        assert_eq!(job.priority, Priority::Immediate);
    }

    #[test]
    fn upgrade_background_then_high_runs_once_as_high() {
        let mut s = sched();
        let key = HydrationKey::Relationships;
        assert_eq!(
            s.request(
                HydrationRequest::background(key, HydrationReason::Reconciliation),
                t(0)
            ),
            RequestOutcome::Enqueued
        );
        assert_eq!(
            s.request(HydrationRequest::high(key, HydrationReason::Event), t(0)),
            RequestOutcome::Upgraded
        );
        let job = s.next_ready(t(0)).expect("ready");
        assert_eq!(job.priority, Priority::High);
        assert!(s.next_ready(t(0)).is_none(), "only one job for the key");
    }

    #[test]
    fn not_before_delays_without_blocking_other_ready_jobs() {
        let mut s = sched();
        let delayed = HydrationKey::Guilds;
        let ready = HydrationKey::Relationships;
        s.request(
            HydrationRequest::high(delayed, HydrationReason::Event).with_not_before(t(1_000)),
            t(0),
        );
        s.request(
            HydrationRequest::background(ready, HydrationReason::Reconciliation),
            t(0),
        );

        // Even though `delayed` is High and `ready` is Background, `delayed`
        // is not ready yet at t=0, so `ready` must be returned instead.
        let job = s.next_ready(t(0)).expect("background job is ready");
        assert_eq!(job.key, ready);
        assert!(s.next_ready(t(0)).is_none(), "delayed job still not ready");
        assert_eq!(s.pending_len(), 1);

        let job2 = s.next_ready(t(1_000)).expect("now ready");
        assert_eq!(job2.key, delayed);
    }

    #[test]
    fn active_plus_event_request_reruns_after_complete() {
        let mut s = sched();
        let key = HydrationKey::DmSummaries;
        s.request(
            HydrationRequest::normal(key, HydrationReason::Startup),
            t(0),
        );
        let job = s.next_ready(t(0)).expect("ready");
        assert!(s.is_active(&key));

        let outcome = s.request(HydrationRequest::normal(key, HydrationReason::Event), t(10));
        assert_eq!(outcome, RequestOutcome::RerunScheduled);
        assert!(!s.is_pending(&key), "rerun does not queue a second job yet");

        s.complete(job.key, t(20));
        assert!(!s.is_active(&key));
        assert!(s.is_pending(&key), "rerun was re-enqueued on completion");
        let rerun = s.next_ready(t(20)).expect("rerun ready");
        assert_eq!(rerun.reason, HydrationReason::Event);
    }

    #[test]
    fn active_plus_reconciliation_request_is_coalesced() {
        let mut s = sched();
        let key = HydrationKey::Guilds;
        s.request(
            HydrationRequest::normal(key, HydrationReason::Startup),
            t(0),
        );
        s.next_ready(t(0));
        let outcome = s.request(
            HydrationRequest::background(key, HydrationReason::Reconciliation),
            t(0),
        );
        assert_eq!(outcome, RequestOutcome::Coalesced);
        s.complete(key, t(0));
        assert!(!s.is_pending(&key), "coalesced request does not rerun");
    }

    #[test]
    fn backoff_delays_double_and_caps_then_gives_up() {
        let mut s = HydrationScheduler::new(BackoffPolicy {
            base: DurationMs::from_secs(1),
            max: DurationMs::from_secs(4),
            max_attempts: 3,
        });
        let key = HydrationKey::VoiceState;
        s.request(
            HydrationRequest::normal(key, HydrationReason::Startup),
            t(0),
        );

        let job1 = s.next_ready(t(0)).expect("attempt 1 ready");
        assert_eq!(job1.attempt, 1);
        let outcome1 = s.fail(key, true, t(0));
        assert_eq!(outcome1, FailOutcome::RetryAt(t(1_000)));

        assert!(
            s.next_ready(t(500)).is_none(),
            "not ready before backoff elapses"
        );
        let job2 = s.next_ready(t(1_000)).expect("attempt 2 ready");
        assert_eq!(job2.attempt, 2);
        let outcome2 = s.fail(key, true, t(1_000));
        // delay(2) = base * 2 = 2s, capped at max=4s.
        assert_eq!(outcome2, FailOutcome::RetryAt(t(3_000)));

        let job3 = s.next_ready(t(3_000)).expect("attempt 3 ready");
        assert_eq!(job3.attempt, 3);
        let outcome3 = s.fail(key, true, t(3_000));
        assert_eq!(outcome3, FailOutcome::GaveUp { attempts: 3 });
        assert!(!s.is_pending(&key));
        assert!(!s.is_active(&key));
    }

    #[test]
    fn non_retryable_failure_gives_up_immediately() {
        let mut s = sched();
        let key = HydrationKey::VoiceState;
        s.request(
            HydrationRequest::normal(key, HydrationReason::Startup),
            t(0),
        );
        s.next_ready(t(0));
        let outcome = s.fail(key, false, t(0));
        assert_eq!(outcome, FailOutcome::GaveUp { attempts: 1 });
        assert!(!s.is_pending(&key));
    }

    #[test]
    fn pause_hides_next_ready_but_keeps_pending() {
        let mut s = sched();
        let key = HydrationKey::Guilds;
        s.request(
            HydrationRequest::normal(key, HydrationReason::Startup),
            t(0),
        );
        s.pause();
        assert!(s.next_ready(t(0)).is_none());
        assert_eq!(s.pending_len(), 1);
        assert!(s.is_paused());
        s.resume();
        assert!(s.next_ready(t(0)).is_some());
    }

    #[test]
    fn release_offline_does_not_count_an_attempt() {
        let mut s = sched();
        let key = HydrationKey::Guilds;
        s.request(
            HydrationRequest::normal(key, HydrationReason::Startup),
            t(0),
        );
        let job = s.next_ready(t(0)).expect("ready");
        assert_eq!(job.attempt, 1);
        s.release_offline(key);
        assert!(s.is_pending(&key));
        assert!(!s.is_active(&key));
        let job2 = s.next_ready(t(0)).expect("ready again");
        assert_eq!(
            job2.attempt, 1,
            "no attempt was consumed by the offline release"
        );
    }

    #[test]
    fn user_refresh_clears_backoff() {
        let mut s = HydrationScheduler::new(BackoffPolicy {
            base: DurationMs::from_secs(10),
            max: DurationMs::from_secs(300),
            max_attempts: 8,
        });
        let key = HydrationKey::Relationships;
        s.request(
            HydrationRequest::normal(key, HydrationReason::Startup),
            t(0),
        );
        s.next_ready(t(0));
        s.fail(key, true, t(0));
        // Backoff would normally hold this until t=10_000.
        assert!(s.next_ready(t(1)).is_none());

        let outcome = s.request(
            HydrationRequest::new(key, Priority::High, HydrationReason::UserRefresh),
            t(1),
        );
        assert_eq!(outcome, RequestOutcome::Upgraded);
        let job = s.next_ready(t(1)).expect("user refresh bypasses backoff");
        assert_eq!(job.reason, HydrationReason::UserRefresh);
    }

    #[test]
    fn cancel_removes_pending_only() {
        let mut s = sched();
        let key = HydrationKey::Guilds;
        assert!(!s.cancel(key));
        s.request(
            HydrationRequest::normal(key, HydrationReason::Startup),
            t(0),
        );
        assert!(s.cancel(key));
        assert!(s.next_ready(t(0)).is_none());
    }

    #[test]
    fn next_wake_reflects_earliest_pending_ready_time() {
        let mut s = sched();
        assert_eq!(s.next_wake(t(0)), None);
        s.request(
            HydrationRequest::normal(HydrationKey::Guilds, HydrationReason::Startup)
                .with_not_before(t(500)),
            t(0),
        );
        s.request(
            HydrationRequest::normal(HydrationKey::Relationships, HydrationReason::Startup)
                .with_not_before(t(200)),
            t(0),
        );
        assert_eq!(s.next_wake(t(0)), Some(t(200)));
        s.pause();
        assert_eq!(s.next_wake(t(0)), None);
    }

    #[test]
    fn pending_snapshot_is_sorted_by_priority_desc() {
        let mut s = sched();
        s.request(
            HydrationRequest::background(HydrationKey::Guilds, HydrationReason::Reconciliation),
            t(0),
        );
        s.request(
            HydrationRequest::immediate(HydrationKey::CurrentUser, HydrationReason::Startup),
            t(0),
        );
        s.request(
            HydrationRequest::normal(HydrationKey::DmSummaries, HydrationReason::Startup),
            t(0),
        );
        let snap = s.pending_snapshot();
        let priorities: Vec<_> = snap.iter().map(|(_, p)| *p).collect();
        assert_eq!(
            priorities,
            vec![Priority::Immediate, Priority::Normal, Priority::Background]
        );
    }

    #[test]
    fn backoff_policy_delay_is_saturating_and_capped() {
        let p = BackoffPolicy {
            base: DurationMs::from_millis(1_000),
            max: DurationMs::from_secs(300),
            max_attempts: 8,
        };
        assert_eq!(p.delay(1), DurationMs::from_millis(1_000));
        assert_eq!(p.delay(2), DurationMs::from_millis(2_000));
        assert_eq!(p.delay(3), DurationMs::from_millis(4_000));
        assert_eq!(p.delay(100), DurationMs::from_secs(300), "capped at max");
    }
}
