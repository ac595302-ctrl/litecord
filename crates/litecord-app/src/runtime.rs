//! Long-lived background tasks and their owners.
//!
//! | task | owner | stops on | error strategy | backpressure |
//! |---|---|---|---|---|
//! | `event-reactor` | [`crate::LitecordApp`] | cancellation / ingest closed | log per event, keep going | single consumer of the bounded ingest queue; overflow ⇒ `ResyncRequired` + reconciliation |
//! | `hydrator` | same | cancellation | backoff inside the scheduler | awaits ingest capacity |
//! | `reconciler` | same | cancellation | log | enqueues Background jobs only when stale |
//! | `reminder-ticker` | same | cancellation | log | bounded batch per tick |
//! | `maintenance` | same | cancellation | log | hourly |
//! | `action-watcher` | same | cancellation | log | polls pending proposals (incl. ones written by the MCP process) |

use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use litecord_core::bus::{AppEventBus, IngestReceiver, IngestSender};
use litecord_core::config::LitecordConfig;
use litecord_core::events::ApplicationEvent;
use litecord_core::metrics::Metrics;
use litecord_hydrator::{HydrationReason, HydrationRequest, Hydrator};
use litecord_memory::{MemoryService, ReminderEngine};
use litecord_store::reducer::{self, Followup, ReducerConfig};
use litecord_store::{repos, Database};
use litecord_types::actions::ActionStatus;
use litecord_types::provenance::DiscordIdentity;

/// Everything the reactor needs; cheap clones of shared handles.
#[derive(Debug, Clone)]
pub(crate) struct ReactorCtx {
    pub db: Database,
    pub bus: AppEventBus,
    pub hydrator: Arc<Hydrator>,
    /// Hydrator of the optional application-bot source.
    pub bot_hydrator: Option<Arc<Hydrator>>,
    pub memory: MemoryService,
    pub metrics: Arc<Metrics>,
    pub ingest: IngestSender,
    pub reducer: ReducerConfig,
}

/// Conversations whose recent history is fetched after a conversation list
/// arrives (background priority, only when stale).
const WARM_CONVERSATIONS: usize = 8;

impl ReactorCtx {
    fn handle(&self, env: litecord_core::events::SourceEnvelope) {
        let kind = env.event.kind();
        // Follow-ups go back to the source that produced the event.
        let is_bot = env.source.identity() == DiscordIdentity::ApplicationBot;
        let hydrator = match (&self.bot_hydrator, is_bot) {
            (Some(bot), true) => bot,
            _ => &self.hydrator,
        };
        // Warm the most recent conversations' history in the background, so
        // lists show real previews without opening each conversation.
        if let litecord_core::events::DiscordEvent::ConversationsSnapshot { conversations } =
            &env.event
        {
            let mut recent: Vec<_> = conversations.iter().collect();
            recent.sort_by_key(|c| std::cmp::Reverse(c.last_activity_at));
            for c in recent.into_iter().take(WARM_CONVERSATIONS) {
                hydrator.request_if_stale(
                    litecord_core::events::HydrationKey::DmConversation {
                        conversation_id: c.id,
                    },
                    litecord_hydrator::Priority::Background,
                    HydrationReason::Startup,
                );
            }
        }
        let committed = match reducer::apply(&self.db, &env, &self.reducer) {
            Ok(c) => c,
            Err(e) => {
                tracing::error!(kind, error = %e, "reducer failed; event skipped");
                return;
            }
        };
        self.metrics.events_ingested.inc();
        self.metrics
            .event_queue_depth
            .set(self.ingest.depth() as u64);
        if committed.changed() {
            self.bus.publish(ApplicationEvent::StateChanged {
                revision: committed.revision,
                changes: committed.events.clone().into(),
            });
        }
        for followup in committed.value.followups {
            match followup {
                Followup::Hydrate(key) => {
                    hydrator.request(HydrationRequest::immediate(key, HydrationReason::Event));
                }
                Followup::ExtractMemory { message_id, .. } => {
                    // Cheap and deterministic (no LLM), so it runs inline;
                    // a model-backed extractor would get its own bounded queue.
                    match self.memory.on_message_created(message_id) {
                        Ok(outcomes) if !outcomes.is_empty() => {
                            if let Ok(revision) = self.db.current_revision() {
                                self.bus.publish(ApplicationEvent::StateChanged {
                                    revision,
                                    changes: Arc::from(Vec::new()),
                                });
                            }
                        }
                        Ok(_) => {}
                        Err(e) => tracing::warn!(error = %e, "memory extraction failed"),
                    }
                }
                Followup::SessionChanged(state) => {
                    hydrator.on_session_changed(&state);
                    // `SessionChanged` describes the *user's* session; bot
                    // session changes surface through diagnostics instead.
                    if !is_bot {
                        self.bus.publish(ApplicationEvent::SessionChanged { state });
                    }
                }
            }
        }
    }
}

pub(crate) async fn event_reactor(
    ctx: ReactorCtx,
    mut rx: IngestReceiver,
    token: CancellationToken,
) -> litecord_core::Result<()> {
    loop {
        let env = tokio::select! {
            _ = token.cancelled() => break,
            env = rx.recv() => env,
        };
        let Some(env) = env else { break };
        ctx.handle(env);
        if rx.take_overflow() {
            tracing::warn!("ingest overflow detected; requesting resync");
            ctx.bus.publish(ApplicationEvent::ResyncRequired {
                reason: "event queue overflow".into(),
            });
            ctx.hydrator.on_resync_required();
        }
    }
    // Drain what is already queued so shutdown does not lose observed state.
    while let Some(env) = rx.try_recv() {
        ctx.handle(env);
    }
    Ok(())
}

pub(crate) async fn reconciler(
    hydrator: Arc<Hydrator>,
    every: Duration,
    token: CancellationToken,
) -> litecord_core::Result<()> {
    let mut tick = tokio::time::interval(every);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = token.cancelled() => return Ok(()),
            _ = tick.tick() => {
                let n = hydrator.reconcile();
                if n > 0 {
                    tracing::debug!(requested = n, "staleness reconciliation");
                }
            }
        }
    }
}

pub(crate) async fn reminder_ticker(
    engine: ReminderEngine,
    db: Database,
    bus: AppEventBus,
    every: Duration,
    token: CancellationToken,
) -> litecord_core::Result<()> {
    let mut tick = tokio::time::interval(every);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = token.cancelled() => return Ok(()),
            _ = tick.tick() => {
                match engine.tick(db.now()) {
                    Ok(fired) => {
                        for f in fired {
                            bus.publish(ApplicationEvent::ReminderDue { reminder_id: f.reminder_id });
                        }
                    }
                    Err(e) => tracing::warn!(error = %e, "reminder tick failed"),
                }
            }
        }
    }
}

pub(crate) async fn maintenance(
    memory: MemoryService,
    db: Database,
    cfg: LitecordConfig,
    every: Duration,
    token: CancellationToken,
) -> litecord_core::Result<()> {
    let mut tick = tokio::time::interval(every);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = token.cancelled() => return Ok(()),
            _ = tick.tick() => {
                let now = db.now();
                if let Err(e) = memory.gc(now) {
                    tracing::warn!(error = %e, "memory gc failed");
                }
                if let Err(e) = memory.apply_retention(now, &cfg.retention) {
                    tracing::warn!(error = %e, "retention failed");
                }
                match memory.refresh_recent_summaries(now) {
                    Ok(n) if n > 0 => tracing::debug!(written = n, "summaries refreshed"),
                    Ok(_) => {}
                    Err(e) => tracing::warn!(error = %e, "summary refresh failed"),
                }
            }
        }
    }
}

/// Announces proposals that appear in the database, including those written
/// by a separate MCP process, so the UI can show approval prompts.
pub(crate) async fn action_watcher(
    db: Database,
    bus: AppEventBus,
    every: Duration,
    token: CancellationToken,
) -> litecord_core::Result<()> {
    let mut last_seen = 0i64;
    let mut tick = tokio::time::interval(every);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = token.cancelled() => return Ok(()),
            _ = tick.tick() => {
                let pending = db.read(|r| repos::actions::list(r, Some(&[ActionStatus::PendingApproval]), 50));
                match pending {
                    Ok(list) => {
                        for p in list.iter().rev().filter(|p| p.id.get() > last_seen) {
                            bus.publish(ApplicationEvent::ActionAwaitingApproval { action_id: p.id });
                        }
                        last_seen = list.iter().map(|p| p.id.get()).max().unwrap_or(last_seen).max(last_seen);
                    }
                    Err(e) => tracing::warn!(error = %e, "action watcher failed"),
                }
            }
        }
    }
}
