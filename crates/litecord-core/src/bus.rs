//! Bounded event transport.
//!
//! ## Ingest (sources → reducer)
//!
//! A single bounded `mpsc` queue. Two ways to enqueue:
//!
//! * [`IngestSender::send`] — async, waits for capacity. Used by hydration and
//!   async adapters: backpressure naturally slows fetching.
//! * [`IngestSender::try_send`] — never blocks. Used from SDK callback threads
//!   that must not stall. On a full queue the event is **dropped**, a counter is
//!   incremented and an overflow flag is raised; the reactor observes the flag
//!   ([`IngestReceiver::take_overflow`]), broadcasts
//!   [`ApplicationEvent::ResyncRequired`] and schedules reconciliation
//!   hydration. Dropping + reconciling keeps memory bounded without losing
//!   correctness, because canonical state is re-fetchable from Discord.
//!
//! ## Application broadcast (reducer → subscribers)
//!
//! `tokio::sync::broadcast` with bounded capacity. A lagging subscriber gets
//! `RecvError::Lagged` and must reload from the store (revision-based).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use tokio::sync::{broadcast, mpsc, OwnedSemaphorePermit, Semaphore, TryAcquireError};

use crate::events::{ApplicationEvent, SourceEnvelope};
use crate::metrics::Gauge;

/// Default byte budget of the ingest queue (8 MiB).
pub const DEFAULT_EVENT_QUEUE_MAX_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Default)]
pub struct IngestStats {
    enqueued: AtomicU64,
    dropped: AtomicU64,
    overflowed: AtomicBool,
    /// Approximate bytes of envelopes currently queued (or admitted and
    /// about to be queued).
    bytes: AtomicU64,
    bytes_gauge: Option<Arc<Gauge>>,
}

impl IngestStats {
    pub fn enqueued(&self) -> u64 {
        self.enqueued.load(Ordering::Relaxed)
    }
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
    /// Approximate bytes of queued envelopes (see
    /// [`SourceEnvelope::approx_bytes`]).
    pub fn bytes_in_flight(&self) -> u64 {
        self.bytes.load(Ordering::Relaxed)
    }

    fn add_bytes(&self, n: u64) {
        let now = self.bytes.fetch_add(n, Ordering::AcqRel).saturating_add(n);
        if let Some(g) = &self.bytes_gauge {
            g.set(now);
        }
    }

    fn sub_bytes(&self, n: u64) {
        let now = self.bytes.fetch_sub(n, Ordering::AcqRel).saturating_sub(n);
        if let Some(g) = &self.bytes_gauge {
            g.set(now);
        }
    }
}

/// Byte-budget reservation that travels with an envelope through the queue;
/// dropping it (when the receiver takes the item, or the item is discarded)
/// releases the KiB permits and the byte accounting.
#[derive(Debug)]
struct BytesTicket {
    _permit: OwnedSemaphorePermit,
    bytes: u64,
    stats: Arc<IngestStats>,
}

impl BytesTicket {
    fn new(permit: OwnedSemaphorePermit, bytes: u64, stats: Arc<IngestStats>) -> Self {
        stats.add_bytes(bytes);
        Self {
            _permit: permit,
            bytes,
            stats,
        }
    }
}

impl Drop for BytesTicket {
    fn drop(&mut self) {
        self.stats.sub_bytes(self.bytes);
    }
}

#[derive(Debug)]
struct Queued {
    env: SourceEnvelope,
    _ticket: BytesTicket,
    guard: Option<(Arc<AtomicU64>, u64)>,
    acknowledgement:
        Option<tokio::sync::oneshot::Sender<Result<litecord_types::Revision, IngestCommitError>>>,
}

#[derive(Debug, Clone, Copy, thiserror::Error)]
#[error("ingest was rejected or could not be committed")]
pub struct IngestCommitError;

/// Ephemeral delivery metadata stays outside serialized Discord events.
#[derive(Debug)]
pub struct IngestDelivery {
    pub envelope: SourceEnvelope,
    acknowledgement:
        Option<tokio::sync::oneshot::Sender<Result<litecord_types::Revision, IngestCommitError>>>,
}
impl IngestDelivery {
    pub fn reduce(
        self,
        reducer: impl FnOnce(SourceEnvelope) -> Result<litecord_types::Revision, IngestCommitError>,
    ) {
        let result = reducer(self.envelope);
        if let Some(tx) = self.acknowledgement {
            let _ = tx.send(result);
        }
    }
    pub fn finish(self, result: Result<litecord_types::Revision, IngestCommitError>) {
        if let Some(tx) = self.acknowledgement {
            let _ = tx.send(result);
        }
    }
}

/// Outcome of a non-blocking enqueue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrySendOutcome {
    Sent,
    /// Queue full (by count or by bytes); event dropped and resync flagged.
    DroppedFull,
    /// Receiver gone (shutting down).
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("ingest channel closed")]
pub struct IngestClosed;

/// Cloneable producer handle given to backends and the hydrator.
#[derive(Debug, Clone)]
pub struct IngestSender {
    tx: mpsc::Sender<Queued>,
    stats: Arc<IngestStats>,
    capacity: usize,
    /// Byte budget, one permit per KiB.
    budget: Arc<Semaphore>,
    budget_kib: u32,
    guard: Option<(Arc<AtomicU64>, u64)>,
}

impl IngestSender {
    pub fn with_session_guard(&self, generation: Arc<AtomicU64>, epoch: u64) -> Self {
        let mut sender = self.clone();
        sender.guard = Some((generation, epoch));
        sender
    }
    pub async fn send_committed(
        &self,
        env: SourceEnvelope,
    ) -> Result<litecord_types::Revision, IngestCommitError> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.enqueue(env, Some(tx))
            .await
            .map_err(|_| IngestCommitError)?;
        rx.await.map_err(|_| IngestCommitError)?
    }
    /// KiB permits for an envelope of `bytes`: `max(1, ceil(bytes / 1024))`,
    /// capped at the whole budget so an oversized envelope is still admitted
    /// once the queue is empty instead of deadlocking.
    fn permits_for(&self, bytes: usize) -> u32 {
        let kib = bytes.div_ceil(1024).max(1);
        u32::try_from(kib).unwrap_or(u32::MAX).min(self.budget_kib)
    }

    /// Enqueue, waiting for both a free slot and enough byte budget.
    pub async fn send(&self, env: SourceEnvelope) -> Result<(), IngestClosed> {
        self.enqueue(env, None).await
    }
    async fn enqueue(
        &self,
        env: SourceEnvelope,
        acknowledgement: Option<
            tokio::sync::oneshot::Sender<Result<litecord_types::Revision, IngestCommitError>>,
        >,
    ) -> Result<(), IngestClosed> {
        let bytes = env.approx_bytes();
        let permits = self.permits_for(bytes);
        let permit = tokio::select! {
            p = self.budget.clone().acquire_many_owned(permits) => p.map_err(|_| IngestClosed)?,
            _ = self.tx.closed() => return Err(IngestClosed),
        };
        let ticket = BytesTicket::new(permit, bytes as u64, self.stats.clone());
        self.tx
            .send(Queued {
                env,
                _ticket: ticket,
                guard: self.guard.clone(),
                acknowledgement,
            })
            .await
            .map_err(|_| IngestClosed)?;
        self.stats.enqueued.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    /// Enqueue without blocking. Drops the event (and flags a resync) when
    /// either the slot capacity or the byte budget is exhausted.
    pub fn try_send(&self, env: SourceEnvelope) -> TrySendOutcome {
        let bytes = env.approx_bytes();
        let permit = match self
            .budget
            .clone()
            .try_acquire_many_owned(self.permits_for(bytes))
        {
            Ok(p) => p,
            Err(TryAcquireError::Closed) => return TrySendOutcome::Closed,
            Err(TryAcquireError::NoPermits) => return self.overflow("bytes"),
        };
        let ticket = BytesTicket::new(permit, bytes as u64, self.stats.clone());
        match self.tx.try_send(Queued {
            env,
            _ticket: ticket,
            guard: self.guard.clone(),
            acknowledgement: None,
        }) {
            Ok(()) => {
                self.stats.enqueued.fetch_add(1, Ordering::Relaxed);
                TrySendOutcome::Sent
            }
            Err(mpsc::error::TrySendError::Full(_)) => self.overflow("count"),
            Err(mpsc::error::TrySendError::Closed(_)) => TrySendOutcome::Closed,
        }
    }

    fn overflow(&self, limit: &'static str) -> TrySendOutcome {
        self.stats.dropped.fetch_add(1, Ordering::Relaxed);
        self.stats.overflowed.store(true, Ordering::Release);
        tracing::warn!(
            capacity = self.capacity,
            max_kib = self.budget_kib,
            limit,
            "ingest queue full; event dropped"
        );
        TrySendOutcome::DroppedFull
    }

    /// Current number of queued events.
    pub fn depth(&self) -> usize {
        self.capacity.saturating_sub(self.tx.capacity())
    }

    /// Approximate bytes of currently queued events.
    pub fn bytes_in_flight(&self) -> u64 {
        self.stats.bytes_in_flight()
    }

    /// The byte budget, rounded up to whole KiB.
    pub fn max_bytes(&self) -> usize {
        self.budget_kib as usize * 1024
    }

    pub fn stats(&self) -> &Arc<IngestStats> {
        &self.stats
    }
}

/// Single consumer owned by the event reactor.
#[derive(Debug)]
pub struct IngestReceiver {
    rx: mpsc::Receiver<Queued>,
    stats: Arc<IngestStats>,
    budget: Arc<Semaphore>,
}

impl IngestReceiver {
    /// Take the next envelope; its byte budget is released immediately.
    pub async fn recv(&mut self) -> Option<SourceEnvelope> {
        self.recv_delivery().await.map(|d| d.envelope)
    }

    pub fn try_recv(&mut self) -> Option<SourceEnvelope> {
        self.try_recv_delivery().map(|d| d.envelope)
    }

    fn delivery(q: Queued) -> Option<IngestDelivery> {
        if q.guard
            .as_ref()
            .is_some_and(|(generation, epoch)| generation.load(Ordering::Acquire) != *epoch)
        {
            return None;
        }
        Some(IngestDelivery {
            envelope: q.env,
            acknowledgement: q.acknowledgement,
        })
    }
    pub async fn recv_delivery(&mut self) -> Option<IngestDelivery> {
        loop {
            let q = self.rx.recv().await?;
            if let Some(delivery) = Self::delivery(q) {
                return Some(delivery);
            }
        }
    }
    pub fn try_recv_delivery(&mut self) -> Option<IngestDelivery> {
        loop {
            let q = self.rx.try_recv().ok()?;
            if let Some(delivery) = Self::delivery(q) {
                return Some(delivery);
            }
        }
    }

    /// Returns `true` once after any overflow, then resets.
    pub fn take_overflow(&self) -> bool {
        self.stats.overflowed.swap(false, Ordering::AcqRel)
    }

    /// Approximate bytes of currently queued events.
    pub fn bytes_in_flight(&self) -> u64 {
        self.stats.bytes_in_flight()
    }
}

impl Drop for IngestReceiver {
    fn drop(&mut self) {
        // Wake producers waiting on byte budget so they observe the close.
        self.budget.close();
    }
}

/// Create the ingest queue with the default byte budget
/// ([`DEFAULT_EVENT_QUEUE_MAX_BYTES`]). `capacity` is coerced to >= 1.
pub fn ingest_channel(capacity: usize) -> (IngestSender, IngestReceiver) {
    ingest_channel_with_budget(capacity, DEFAULT_EVENT_QUEUE_MAX_BYTES, None)
}

/// Create the ingest queue bounded by `capacity` events **and** `max_bytes`
/// (approximate, rounded up to whole KiB; coerced to >= 1 KiB). `bytes_gauge`,
/// if given, tracks the bytes in flight.
pub fn ingest_channel_with_budget(
    capacity: usize,
    max_bytes: usize,
    bytes_gauge: Option<Arc<Gauge>>,
) -> (IngestSender, IngestReceiver) {
    let capacity = capacity.max(1);
    let budget_kib = u32::try_from(max_bytes.div_ceil(1024))
        .unwrap_or(u32::MAX)
        .min(u32::try_from(Semaphore::MAX_PERMITS).unwrap_or(u32::MAX))
        .max(1);
    let budget = Arc::new(Semaphore::new(budget_kib as usize));
    if let Some(g) = &bytes_gauge {
        g.set(0);
    }
    let (tx, rx) = mpsc::channel(capacity);
    let stats = Arc::new(IngestStats {
        bytes_gauge,
        ..IngestStats::default()
    });
    (
        IngestSender {
            tx,
            stats: stats.clone(),
            capacity,
            budget: budget.clone(),
            budget_kib,
            guard: None,
        },
        IngestReceiver { rx, stats, budget },
    )
}

/// Application-wide broadcast of [`ApplicationEvent`]s.
#[derive(Debug, Clone)]
pub struct AppEventBus {
    tx: broadcast::Sender<ApplicationEvent>,
}

impl AppEventBus {
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity.max(1));
        Self { tx }
    }

    /// Publish; having no subscribers is not an error.
    pub fn publish(&self, event: ApplicationEvent) {
        let _ = self.tx.send(event);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ApplicationEvent> {
        self.tx.subscribe()
    }

    pub fn subscriber_count(&self) -> usize {
        self.tx.receiver_count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::DiscordEvent;
    use litecord_types::provenance::DiscordSource;
    use litecord_types::social::SessionState;
    use litecord_types::Timestamp;

    fn env() -> SourceEnvelope {
        SourceEnvelope::new(
            DiscordSource::Synthetic,
            Timestamp(0),
            DiscordEvent::SessionChanged {
                state: SessionState::Ready,
            },
        )
    }

    #[tokio::test]
    async fn try_send_drops_and_flags_overflow_when_full() {
        let (tx, mut rx) = ingest_channel(2);
        assert_eq!(tx.try_send(env()), TrySendOutcome::Sent);
        assert_eq!(tx.try_send(env()), TrySendOutcome::Sent);
        assert_eq!(tx.depth(), 2);
        assert_eq!(tx.try_send(env()), TrySendOutcome::DroppedFull);
        assert_eq!(tx.stats().dropped(), 1);
        assert!(rx.take_overflow());
        assert!(!rx.take_overflow(), "flag resets after being taken");
        assert!(rx.recv().await.is_some());
        assert_eq!(tx.depth(), 1);
    }

    /// An envelope carrying a message with `content_len` bytes of content.
    fn msg_env(content_len: usize) -> SourceEnvelope {
        use litecord_types::ids::{ConversationId, MessageId, UserId};
        use litecord_types::social::Message;
        SourceEnvelope::new(
            DiscordSource::Synthetic,
            Timestamp(0),
            DiscordEvent::MessageCreated {
                message: Message {
                    id: MessageId(1),
                    conversation_id: ConversationId(1),
                    author_id: UserId(1),
                    content: "x".repeat(content_len).into(),
                    sent_at: Timestamp(0),
                    edited_at: None,
                    reply_to: None,
                    extras: Vec::new(),
                },
            },
        )
    }

    /// ~1.5 KiB envelope, i.e. exactly 2 KiB permits.
    fn two_kib_env() -> SourceEnvelope {
        let e = msg_env(1_500);
        assert!(
            (1025..=2048).contains(&e.approx_bytes()),
            "{}",
            e.approx_bytes()
        );
        e
    }

    #[tokio::test]
    async fn send_waits_for_byte_budget_and_wakes_when_drained() {
        let gauge = Arc::new(Gauge::default());
        let (tx, mut rx) = ingest_channel_with_budget(16, 4 * 1024, Some(gauge.clone()));
        tx.send(two_kib_env()).await.unwrap();
        tx.send(two_kib_env()).await.unwrap();
        assert_eq!(tx.depth(), 2);
        let queued = tx.bytes_in_flight();
        assert!(queued > 2_048);
        assert_eq!(gauge.get(), queued);

        let tx2 = tx.clone();
        let mut blocked = tokio::spawn(async move { tx2.send(two_kib_env()).await });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut blocked)
                .await
                .is_err(),
            "third send must wait for bytes although slots are free"
        );
        assert!(rx.recv().await.is_some());
        tokio::time::timeout(std::time::Duration::from_secs(5), blocked)
            .await
            .expect("send wakes after drain")
            .unwrap()
            .unwrap();
        assert_eq!(tx.depth(), 2);
        assert_eq!(tx.bytes_in_flight(), queued);
        assert!(rx.recv().await.is_some());
        assert!(rx.recv().await.is_some());
        assert_eq!(tx.bytes_in_flight(), 0);
        assert_eq!(gauge.get(), 0);
    }

    #[tokio::test]
    async fn try_send_drops_when_byte_budget_exhausted() {
        let (tx, mut rx) = ingest_channel_with_budget(16, 4 * 1024, None);
        assert_eq!(tx.try_send(two_kib_env()), TrySendOutcome::Sent);
        assert_eq!(tx.try_send(two_kib_env()), TrySendOutcome::Sent);
        assert_eq!(tx.try_send(two_kib_env()), TrySendOutcome::DroppedFull);
        assert_eq!(tx.stats().dropped(), 1);
        assert_eq!(tx.depth(), 2);
        assert!(rx.take_overflow());
        assert!(rx.try_recv().is_some());
        assert_eq!(tx.try_send(two_kib_env()), TrySendOutcome::Sent);
    }

    #[tokio::test]
    async fn oversized_envelope_is_admitted_only_when_empty() {
        let (tx, mut rx) = ingest_channel_with_budget(16, 1024, None);
        // Larger than the whole budget: admitted because the queue is empty.
        assert_eq!(tx.try_send(msg_env(10_000)), TrySendOutcome::Sent);
        assert!(tx.bytes_in_flight() > 10_000);
        // It holds every permit, so even a tiny event is refused now.
        assert_eq!(tx.try_send(env()), TrySendOutcome::DroppedFull);
        assert!(rx.recv().await.is_some());
        assert_eq!(tx.bytes_in_flight(), 0);

        // Async path: an oversized send waits until the queue drains.
        tx.send(env()).await.unwrap();
        let tx2 = tx.clone();
        let mut big = tokio::spawn(async move { tx2.send(msg_env(10_000)).await });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut big)
                .await
                .is_err()
        );
        assert!(rx.recv().await.is_some());
        tokio::time::timeout(std::time::Duration::from_secs(5), big)
            .await
            .expect("oversized send admitted once empty")
            .unwrap()
            .unwrap();
        assert!(rx.recv().await.is_some());
    }

    #[tokio::test]
    async fn dropping_receiver_wakes_byte_waiters() {
        let (tx, rx) = ingest_channel_with_budget(16, 1024, None);
        tx.send(msg_env(10_000)).await.unwrap();
        let tx2 = tx.clone();
        let waiter = tokio::spawn(async move { tx2.send(env()).await });
        tokio::task::yield_now().await;
        drop(rx);
        let res = tokio::time::timeout(std::time::Duration::from_secs(5), waiter)
            .await
            .expect("waiter woken")
            .unwrap();
        assert_eq!(res, Err(IngestClosed));
    }

    #[tokio::test]
    async fn closed_receiver_reports_closed() {
        let (tx, rx) = ingest_channel(1);
        drop(rx);
        assert_eq!(tx.try_send(env()), TrySendOutcome::Closed);
        assert_eq!(tx.send(env()).await, Err(IngestClosed));
    }

    #[tokio::test]
    async fn checkpoint_producer_waits_for_commit_and_observes_failure() {
        let (tx, mut rx) = ingest_channel(2);
        let sender = tx.clone();
        let pending = tokio::spawn(async move { sender.send_committed(env()).await });
        let delivery = rx.recv_delivery().await.unwrap();
        assert!(
            !pending.is_finished(),
            "queue admission is not durable acknowledgement"
        );
        delivery.reduce(|_| Err(IngestCommitError));
        assert!(pending.await.unwrap().is_err());
        let sender = tx.clone();
        let pending = tokio::spawn(async move { sender.send_committed(env()).await });
        rx.recv_delivery()
            .await
            .unwrap()
            .reduce(|_| Ok(litecord_types::Revision(9)));
        assert_eq!(pending.await.unwrap().unwrap(), litecord_types::Revision(9));
    }

    #[tokio::test]
    async fn logout_epoch_rejects_queued_old_session_and_releases_budget() {
        let (tx, mut rx) = ingest_channel(4);
        let generation = Arc::new(AtomicU64::new(1));
        tx.with_session_guard(generation.clone(), 1)
            .send(env())
            .await
            .unwrap();
        generation.store(2, Ordering::Release);
        assert!(rx.try_recv_delivery().is_none());
        assert_eq!(tx.bytes_in_flight(), 0);
        tx.with_session_guard(generation, 2)
            .send(env())
            .await
            .unwrap();
        assert!(rx.recv_delivery().await.is_some());
    }
}
