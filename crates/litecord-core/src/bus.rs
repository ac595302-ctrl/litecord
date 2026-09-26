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

use tokio::sync::{broadcast, mpsc};

use crate::events::{ApplicationEvent, SourceEnvelope};

#[derive(Debug, Default)]
pub struct IngestStats {
    enqueued: AtomicU64,
    dropped: AtomicU64,
    overflowed: AtomicBool,
}

impl IngestStats {
    pub fn enqueued(&self) -> u64 {
        self.enqueued.load(Ordering::Relaxed)
    }
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

/// Outcome of a non-blocking enqueue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrySendOutcome {
    Sent,
    /// Queue full; event dropped and resync flagged.
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
    tx: mpsc::Sender<SourceEnvelope>,
    stats: Arc<IngestStats>,
    capacity: usize,
}

impl IngestSender {
    pub async fn send(&self, env: SourceEnvelope) -> Result<(), IngestClosed> {
        self.tx.send(env).await.map_err(|_| IngestClosed)?;
        self.stats.enqueued.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    pub fn try_send(&self, env: SourceEnvelope) -> TrySendOutcome {
        match self.tx.try_send(env) {
            Ok(()) => {
                self.stats.enqueued.fetch_add(1, Ordering::Relaxed);
                TrySendOutcome::Sent
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.stats.dropped.fetch_add(1, Ordering::Relaxed);
                self.stats.overflowed.store(true, Ordering::Release);
                tracing::warn!(capacity = self.capacity, "ingest queue full; event dropped");
                TrySendOutcome::DroppedFull
            }
            Err(mpsc::error::TrySendError::Closed(_)) => TrySendOutcome::Closed,
        }
    }

    /// Current number of queued events.
    pub fn depth(&self) -> usize {
        self.capacity.saturating_sub(self.tx.capacity())
    }

    pub fn stats(&self) -> &Arc<IngestStats> {
        &self.stats
    }
}

/// Single consumer owned by the event reactor.
#[derive(Debug)]
pub struct IngestReceiver {
    rx: mpsc::Receiver<SourceEnvelope>,
    stats: Arc<IngestStats>,
}

impl IngestReceiver {
    pub async fn recv(&mut self) -> Option<SourceEnvelope> {
        self.rx.recv().await
    }

    pub fn try_recv(&mut self) -> Option<SourceEnvelope> {
        self.rx.try_recv().ok()
    }

    /// Returns `true` once after any overflow, then resets.
    pub fn take_overflow(&self) -> bool {
        self.stats.overflowed.swap(false, Ordering::AcqRel)
    }
}

/// Create the ingest queue. `capacity` must be > 0.
pub fn ingest_channel(capacity: usize) -> (IngestSender, IngestReceiver) {
    let capacity = capacity.max(1);
    let (tx, rx) = mpsc::channel(capacity);
    let stats = Arc::new(IngestStats::default());
    (
        IngestSender {
            tx,
            stats: stats.clone(),
            capacity,
        },
        IngestReceiver { rx, stats },
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

    #[tokio::test]
    async fn closed_receiver_reports_closed() {
        let (tx, rx) = ingest_channel(1);
        drop(rx);
        assert_eq!(tx.try_send(env()), TrySendOutcome::Closed);
        assert_eq!(tx.send(env()).await, Err(IngestClosed));
    }
}
