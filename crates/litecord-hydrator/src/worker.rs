//! The async hydration worker.
//!
//! [`Hydrator`] owns a [`HydrationScheduler`] plus a [`FreshnessStore`] and
//! drives them against a [`SocialBackend`]: it pulls ready jobs, calls the
//! backend, and sends the result into the canonical ingest queue as a
//! [`DiscordEvent`] snapshot — never writing to any store directly.

use std::sync::Arc;

use tokio::sync::Notify;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

use litecord_core::bus::IngestSender;
use litecord_core::clock::SharedClock;
use litecord_core::config::HydrationConfig;
use litecord_core::events::{DiscordEvent, HydrationKey, SourceEnvelope};
use litecord_core::metrics::Metrics;
use litecord_core::ports::{BackendError, SocialBackend};
use litecord_types::ids::GuildId;
use litecord_types::social::SessionState;

use crate::error::HydrationError;
use crate::freshness::{FreshnessStore, StalenessPolicy};
use crate::scheduler::{
    BackoffPolicy, FailOutcome, HydrationJob, HydrationReason, HydrationRequest,
    HydrationScheduler, Priority, RequestOutcome,
};

/// Internal: distinguishes "the backend call failed" from "the ingest queue
/// is closed", which need different handling in [`Hydrator::execute`].
enum ExecuteError {
    Backend(BackendError),
    IngestClosed,
}

impl From<BackendError> for ExecuteError {
    fn from(e: BackendError) -> Self {
        ExecuteError::Backend(e)
    }
}

/// Drives hydration: pulls ready jobs off a [`HydrationScheduler`], calls the
/// [`SocialBackend`], and feeds results into the canonical ingest queue.
#[derive(Debug)]
pub struct Hydrator {
    backend: Arc<dyn SocialBackend>,
    sink: IngestSender,
    scheduler: std::sync::Mutex<HydrationScheduler>,
    freshness: Arc<dyn FreshnessStore>,
    policy: StalenessPolicy,
    clock: SharedClock,
    metrics: Arc<Metrics>,
    notify: Notify,
    max_concurrent: usize,
    recent_messages_limit: u32,
}

impl Hydrator {
    pub fn new(
        backend: Arc<dyn SocialBackend>,
        sink: IngestSender,
        freshness: Arc<dyn FreshnessStore>,
        config: &HydrationConfig,
        clock: SharedClock,
        metrics: Arc<Metrics>,
    ) -> Arc<Self> {
        let scheduler = HydrationScheduler::new(BackoffPolicy::from(config));
        Arc::new(Self {
            backend,
            sink,
            scheduler: std::sync::Mutex::new(scheduler),
            freshness,
            policy: StalenessPolicy::from(config),
            clock,
            metrics,
            notify: Notify::new(),
            max_concurrent: config.max_concurrent_jobs.max(1),
            recent_messages_limit: config.recent_messages_limit,
        })
    }

    fn lock_scheduler(&self) -> std::sync::MutexGuard<'_, HydrationScheduler> {
        match self.scheduler.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn sync_queue_metrics(&self) {
        let (pending, active) = {
            let sched = self.lock_scheduler();
            (sched.pending_len(), sched.active_len())
        };
        self.metrics.hydration_queue_depth.set(pending as u64);
        self.metrics.hydration_active.set(active as u64);
    }

    /// Queue a hydration request. Notifies [`Hydrator::run`] so it can pick
    /// it up without waiting for its next poll.
    pub fn request(&self, req: HydrationRequest) -> RequestOutcome {
        let now = self.clock.now();
        let outcome = self.lock_scheduler().request(req, now);
        self.sync_queue_metrics();
        self.notify.notify_one();
        outcome
    }

    /// Request `key` only if the freshness store says it is stale. Used by
    /// active-screen hydration and agent reads. Returns whether a request was
    /// made.
    pub fn request_if_stale(
        &self,
        key: HydrationKey,
        priority: Priority,
        reason: HydrationReason,
    ) -> bool {
        let now = self.clock.now();
        let stale = match self.freshness.get(&key) {
            Ok(Some(freshness)) => freshness.is_stale(now),
            Ok(None) => true,
            Err(e) => {
                tracing::warn!(error = %e, key = ?key, "freshness lookup failed; treating as stale");
                true
            }
        };
        if stale {
            self.request(HydrationRequest::new(key, priority, reason));
        }
        stale
    }

    /// The initial set of fetches once a session becomes usable.
    pub fn initial_hydration(&self) {
        self.request(HydrationRequest::immediate(
            HydrationKey::CurrentUser,
            HydrationReason::Startup,
        ));
        self.request(HydrationRequest::high(
            HydrationKey::Relationships,
            HydrationReason::Startup,
        ));
        self.request(HydrationRequest::high(
            HydrationKey::DmSummaries,
            HydrationReason::Startup,
        ));
        self.request(HydrationRequest::normal(
            HydrationKey::Guilds,
            HydrationReason::Startup,
        ));
        self.request(HydrationRequest::normal(
            HydrationKey::VoiceState,
            HydrationReason::Startup,
        ));
    }

    /// Background staleness sweep: requests (at [`Priority::Background`])
    /// every known key that is currently stale. Returns how many were
    /// requested.
    pub fn reconcile(&self) -> usize {
        let now = self.clock.now();
        let entries = match self.freshness.list() {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "freshness list failed during reconciliation");
                return 0;
            }
        };
        let mut count = 0;
        for (key, freshness) in entries {
            if freshness.is_stale(now) {
                self.request(HydrationRequest::background(
                    key,
                    HydrationReason::Reconciliation,
                ));
                count += 1;
            }
        }
        count
    }

    fn request_core_set(&self, reason: HydrationReason) {
        self.request(HydrationRequest::new(
            HydrationKey::CurrentUser,
            Priority::Immediate,
            reason,
        ));
        self.request(HydrationRequest::new(
            HydrationKey::Relationships,
            Priority::High,
            reason,
        ));
        self.request(HydrationRequest::new(
            HydrationKey::DmSummaries,
            Priority::High,
            reason,
        ));
        self.request(HydrationRequest::new(
            HydrationKey::Guilds,
            Priority::Normal,
            reason,
        ));
    }

    /// React to a session lifecycle change: pause on anything not `Ready`,
    /// and on a `Ready` transition out of a paused state, resume, mark
    /// everything dirty, and re-request the core set.
    pub fn on_session_changed(&self, state: &SessionState) {
        match state {
            SessionState::Offline
            | SessionState::Reconnecting
            | SessionState::LoggedOut
            | SessionState::Error { .. } => {
                self.lock_scheduler().pause();
            }
            SessionState::Ready => {
                let was_paused = self.lock_scheduler().is_paused();
                if was_paused {
                    self.lock_scheduler().resume();
                    if let Err(e) = self.freshness.mark_all_dirty() {
                        tracing::warn!(error = %e, "mark_all_dirty failed on reconnect");
                    }
                    self.request_core_set(HydrationReason::Reconnect);
                }
            }
            SessionState::Authorizing | SessionState::Connecting | SessionState::Hydrating => {}
        }
        self.notify.notify_one();
    }

    /// Force a full reconciliation, e.g. after an ingest queue overflow.
    pub fn on_resync_required(&self) {
        if let Err(e) = self.freshness.mark_all_dirty() {
            tracing::warn!(error = %e, "mark_all_dirty failed on resync");
        }
        self.request_core_set(HydrationReason::Reconciliation);
    }

    pub fn pending_len(&self) -> usize {
        self.lock_scheduler().pending_len()
    }

    pub fn active_len(&self) -> usize {
        self.lock_scheduler().active_len()
    }

    /// Run the hydration loop until `token` is cancelled.
    pub async fn run(self: Arc<Self>, token: CancellationToken) -> litecord_core::Result<()> {
        let mut joinset: JoinSet<()> = JoinSet::new();
        loop {
            while joinset.len() < self.max_concurrent {
                let now = self.clock.now();
                let job = self.lock_scheduler().next_ready(now);
                let Some(job) = job else { break };
                self.sync_queue_metrics();
                let this = Arc::clone(&self);
                joinset.spawn(async move {
                    if let Err(e) = this.execute(job).await {
                        tracing::info!(error = %e, "hydrator stopping job");
                    }
                });
            }

            let sleep_dur = {
                let now = self.clock.now();
                self.lock_scheduler().next_wake(now).map(|wake| {
                    let ms = wake.since(now).as_millis().max(10);
                    std::time::Duration::from_millis(ms)
                })
            };

            tokio::select! {
                () = token.cancelled() => {
                    joinset.abort_all();
                    while joinset.join_next().await.is_some() {}
                    return Ok(());
                }
                joined = joinset.join_next(), if !joinset.is_empty() => {
                    if let Some(Err(e)) = joined {
                        if e.is_panic() {
                            tracing::error!("hydration job task panicked");
                        }
                    }
                }
                () = self.notify.notified() => {}
                () = sleep_or_pending(sleep_dur) => {}
            }
        }
    }

    /// Execute every currently-ready job to completion, sequentially.
    /// Deterministic and easy to drive from tests; [`Hydrator::run`] is the
    /// concurrent, long-running counterpart for production use. Returns how
    /// many jobs were executed.
    pub async fn run_once(&self) -> usize {
        let mut count = 0;
        loop {
            let now = self.clock.now();
            let job = self.lock_scheduler().next_ready(now);
            let Some(job) = job else { break };
            if let Err(e) = self.execute(job).await {
                tracing::info!(error = %e, "hydrator stopping job");
            }
            count += 1;
        }
        count
    }

    async fn execute(&self, job: HydrationJob) -> Result<(), HydrationError> {
        let span = tracing::info_span!("hydrate", key = ?job.key, reason = job.reason.as_str(), attempt = job.attempt);
        let result = async {
            match self.fetch_and_emit(&job).await {
                Ok(()) => {
                    let now = self.clock.now();
                    let stale_after = self.policy.stale_after(&job.key);
                    if let Err(e) = self.freshness.mark_fresh(&job.key, now, stale_after) {
                        tracing::warn!(error = %e, "mark_fresh failed");
                    }
                    self.lock_scheduler().complete(job.key, now);
                    Ok(())
                }
                Err(ExecuteError::IngestClosed) => Err(HydrationError::IngestClosed),
                Err(ExecuteError::Backend(err)) => {
                    self.handle_backend_error(&job, err);
                    Ok(())
                }
            }
        }
        .instrument(span)
        .await;
        self.sync_queue_metrics();
        result
    }

    fn handle_backend_error(&self, job: &HydrationJob, err: BackendError) {
        let now = self.clock.now();
        match err {
            BackendError::Offline | BackendError::NotConnected => {
                let mut sched = self.lock_scheduler();
                sched.release_offline(job.key);
                sched.pause();
            }
            BackendError::Unsupported { .. } => {
                tracing::debug!(key = ?job.key, error = %err, "hydration capability unsupported; not retrying");
                let stale_after = self.policy.stale_after(&job.key);
                if let Err(e) =
                    self.freshness
                        .record_failure(&job.key, &err.to_string(), stale_after)
                {
                    tracing::warn!(error = %e, "record_failure failed");
                }
                let _ = self.lock_scheduler().fail(job.key, false, now);
            }
            other => {
                let stale_after = self.policy.stale_after(&job.key);
                if let Err(e) =
                    self.freshness
                        .record_failure(&job.key, &other.to_string(), stale_after)
                {
                    tracing::warn!(error = %e, "record_failure failed");
                }
                let outcome = self
                    .lock_scheduler()
                    .fail(job.key, other.is_retryable(), now);
                self.metrics.hydration_failures.inc();
                if let FailOutcome::GaveUp { attempts } = outcome {
                    tracing::warn!(key = ?job.key, attempts, error = %other, "hydration gave up after repeated failures");
                }
            }
        }
    }

    async fn fetch_and_emit(&self, job: &HydrationJob) -> Result<(), ExecuteError> {
        let event = match job.key {
            HydrationKey::CurrentUser => {
                let user = self.backend.current_user().await?;
                DiscordEvent::CurrentUser { user }
            }
            HydrationKey::Relationships => {
                let entries = self.backend.relationships().await?;
                DiscordEvent::RelationshipsSnapshot { entries }
            }
            HydrationKey::Guilds => {
                let guilds = self.backend.guilds().await?;
                let guild_ids: Vec<GuildId> = guilds.iter().map(|g| g.id).collect();
                self.emit(DiscordEvent::GuildsSnapshot { guilds }).await?;
                for guild_id in guild_ids {
                    self.request_if_stale(
                        HydrationKey::GuildChannels { guild_id },
                        Priority::Normal,
                        HydrationReason::Event,
                    );
                }
                return Ok(());
            }
            HydrationKey::GuildChannels { guild_id } => {
                let channels = self.backend.guild_channels(guild_id).await?;
                DiscordEvent::GuildChannelsSnapshot { guild_id, channels }
            }
            HydrationKey::DmSummaries => {
                let conversations = self.backend.conversations().await?;
                DiscordEvent::ConversationsSnapshot { conversations }
            }
            HydrationKey::DmConversation { conversation_id } => {
                let messages = self
                    .backend
                    .messages(conversation_id, self.recent_messages_limit)
                    .await?;
                DiscordEvent::MessagesSnapshot {
                    conversation_id,
                    messages,
                }
            }
            HydrationKey::Lobby { lobby_id } => {
                let lobby = self.backend.lobby(lobby_id).await?;
                DiscordEvent::LobbyUpserted { lobby }
            }
            HydrationKey::User { user_id } => {
                let user = self.backend.user(user_id).await?;
                DiscordEvent::UserUpserted { user }
            }
            HydrationKey::VoiceState => {
                let voice = self.backend.voice_state().await?;
                DiscordEvent::VoiceStateChanged { voice }
            }
        };
        self.emit(event).await
    }

    async fn emit(&self, event: DiscordEvent) -> Result<(), ExecuteError> {
        let envelope = SourceEnvelope::new(self.backend.source(), self.clock.now(), event);
        self.sink
            .send(envelope)
            .await
            .map_err(|_| ExecuteError::IngestClosed)
    }
}

/// Sleeps for `dur` if given, otherwise never resolves (so the surrounding
/// `select!` relies solely on its other branches).
async fn sleep_or_pending(dur: Option<std::time::Duration>) {
    match dur {
        Some(d) => tokio::time::sleep(d).await,
        None => std::future::pending::<()>().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Mutex as StdMutex;

    use async_trait::async_trait;

    use litecord_core::bus::ingest_channel;
    use litecord_core::clock::{Clock, ManualClock};
    use litecord_types::capability::{BackendMode, CapabilitySet};
    use litecord_types::ids::{ConversationId, UserId};
    use litecord_types::provenance::DiscordSource;
    use litecord_types::social::{
        Conversation, ConversationKind, Guild, Relationship, RelationshipKind, User,
    };
    use litecord_types::DurationMs as Dur;

    use crate::freshness::InMemoryFreshnessStore;

    #[derive(Debug, Default)]
    struct Calls {
        current_user: AtomicU32,
        relationships: AtomicU32,
        guilds: AtomicU32,
        guild_channels: AtomicU32,
        conversations: AtomicU32,
        voice_state: AtomicU32,
    }

    #[derive(Debug)]
    struct FakeBackend {
        calls: Calls,
        offline: StdMutex<bool>,
        fail_guilds_times: AtomicU32,
        unsupported_voice: bool,
    }

    impl FakeBackend {
        fn new() -> Self {
            Self {
                calls: Calls::default(),
                offline: StdMutex::new(false),
                fail_guilds_times: AtomicU32::new(0),
                unsupported_voice: false,
            }
        }

        fn set_offline(&self, offline: bool) {
            *self.offline.lock().unwrap_or_else(|p| p.into_inner()) = offline;
        }

        fn is_offline(&self) -> bool {
            *self.offline.lock().unwrap_or_else(|p| p.into_inner())
        }
    }

    #[async_trait]
    impl SocialBackend for FakeBackend {
        fn source(&self) -> DiscordSource {
            DiscordSource::Synthetic
        }

        fn mode(&self) -> BackendMode {
            BackendMode::Demo
        }

        fn capabilities(&self) -> CapabilitySet {
            CapabilitySet::default()
        }

        async fn connect(&self, _sink: IngestSender) -> Result<(), BackendError> {
            Ok(())
        }

        async fn disconnect(&self) -> Result<(), BackendError> {
            Ok(())
        }

        async fn current_user(&self) -> Result<User, BackendError> {
            self.calls.current_user.fetch_add(1, Ordering::SeqCst);
            if self.is_offline() {
                return Err(BackendError::Offline);
            }
            Ok(User {
                id: UserId(1),
                username: "demo".into(),
                global_name: None,
                avatar_url: None,
                is_bot: false,
                is_provisional: false,
            })
        }

        async fn relationships(&self) -> Result<Vec<(Relationship, Option<User>)>, BackendError> {
            self.calls.relationships.fetch_add(1, Ordering::SeqCst);
            if self.is_offline() {
                return Err(BackendError::Offline);
            }
            Ok(vec![(
                Relationship {
                    user_id: UserId(2),
                    discord: RelationshipKind::Friend,
                    game: RelationshipKind::None,
                    since: None,
                },
                None,
            )])
        }

        async fn guilds(&self) -> Result<Vec<Guild>, BackendError> {
            self.calls.guilds.fetch_add(1, Ordering::SeqCst);
            if self.fail_guilds_times.load(Ordering::SeqCst) > 0 {
                self.fail_guilds_times.fetch_sub(1, Ordering::SeqCst);
                return Err(BackendError::Sdk("transient".into()));
            }
            Ok(vec![Guild {
                id: GuildId(10),
                name: "guild".into(),
                icon_url: None,
            }])
        }

        async fn guild_channels(
            &self,
            _guild_id: GuildId,
        ) -> Result<Vec<litecord_types::social::Channel>, BackendError> {
            self.calls.guild_channels.fetch_add(1, Ordering::SeqCst);
            Ok(vec![])
        }

        async fn conversations(&self) -> Result<Vec<Conversation>, BackendError> {
            self.calls.conversations.fetch_add(1, Ordering::SeqCst);
            if self.is_offline() {
                return Err(BackendError::Offline);
            }
            Ok(vec![Conversation {
                id: ConversationId(5),
                kind: ConversationKind::DirectMessage,
                recipient_id: Some(UserId(2)),
                guild_id: None,
                lobby_id: None,
                title: None,
                last_message_id: None,
                last_activity_at: None,
            }])
        }

        async fn voice_state(&self) -> Result<litecord_types::social::VoiceState, BackendError> {
            self.calls.voice_state.fetch_add(1, Ordering::SeqCst);
            if self.unsupported_voice {
                return Err(BackendError::Unsupported {
                    capability: litecord_types::capability::Capability::Voice,
                });
            }
            Ok(litecord_types::social::VoiceState::default())
        }
    }

    fn hydrator_for_test(
        backend: Arc<FakeBackend>,
    ) -> (
        Arc<Hydrator>,
        Arc<InMemoryFreshnessStore>,
        ManualClock,
        litecord_core::bus::IngestReceiver,
    ) {
        let (sink, rx) = ingest_channel(64);
        let freshness = Arc::new(InMemoryFreshnessStore::new());
        let clock = ManualClock::new(litecord_types::Timestamp::from_millis(0));
        let cfg = HydrationConfig {
            max_concurrent_jobs: 2,
            backoff_base_ms: 1_000,
            backoff_max_ms: 10_000,
            recent_messages_limit: 50,
            ..HydrationConfig::default()
        };
        let metrics = Metrics::new();
        let hydrator = Hydrator::new(
            backend,
            sink,
            freshness.clone(),
            &cfg,
            Arc::new(clock.clone()) as SharedClock,
            metrics,
        );
        (hydrator, freshness, clock, rx)
    }

    #[tokio::test]
    async fn initial_hydration_produces_expected_envelopes_and_marks_freshness() {
        let backend = Arc::new(FakeBackend::new());
        let (hydrator, freshness, clock, mut rx) = hydrator_for_test(backend.clone());

        hydrator.initial_hydration();
        // CurrentUser, Relationships, DmSummaries, Guilds, VoiceState, plus
        // the GuildChannels fetch that Guilds hydration cascades into for the
        // one guild it saw (never observed before, so it is stale) — all
        // ready in the same instant, so a single `run_once` drains them all.
        let n = hydrator.run_once().await;
        assert_eq!(n, 6);
        assert_eq!(backend.calls.guild_channels.load(Ordering::SeqCst), 1);
        assert_eq!(hydrator.run_once().await, 0, "nothing left to do");

        let mut kinds = Vec::new();
        while let Some(env) = rx.try_recv() {
            kinds.push(env.event.kind().to_string());
        }
        assert!(kinds.contains(&"current_user".to_string()));
        assert!(kinds.contains(&"relationships_snapshot".to_string()));
        assert!(kinds.contains(&"conversations_snapshot".to_string()));
        assert!(kinds.contains(&"guilds_snapshot".to_string()));
        assert!(kinds.contains(&"guild_channels_snapshot".to_string()));

        assert!(freshness
            .get(&HydrationKey::CurrentUser)
            .unwrap()
            .unwrap()
            .observed_at
            .is_some());
        assert!(freshness
            .get(&HydrationKey::Relationships)
            .unwrap()
            .unwrap()
            .observed_at
            .is_some());
        assert!(freshness
            .get(&HydrationKey::DmSummaries)
            .unwrap()
            .unwrap()
            .observed_at
            .is_some());
        assert!(freshness
            .get(&HydrationKey::Guilds)
            .unwrap()
            .unwrap()
            .observed_at
            .is_some());
        let _ = clock; // silence unused warning if the compiler ever thinks so
    }

    #[tokio::test]
    async fn duplicate_pending_requests_lead_to_one_backend_call() {
        let backend = Arc::new(FakeBackend::new());
        let (hydrator, _freshness, _clock, _rx) = hydrator_for_test(backend.clone());

        hydrator.request(HydrationRequest::normal(
            HydrationKey::Guilds,
            HydrationReason::Startup,
        ));
        hydrator.request(HydrationRequest::normal(
            HydrationKey::Guilds,
            HydrationReason::Startup,
        ));
        hydrator.request(HydrationRequest::normal(
            HydrationKey::Guilds,
            HydrationReason::Startup,
        ));

        // The Guilds fetch succeeds and cascades into one GuildChannels job,
        // so two jobs run in total, but `guilds()` itself is called once:
        // proof that the three duplicate requests were deduplicated rather
        // than each producing their own fetch.
        let n = hydrator.run_once().await;
        assert_eq!(n, 2);
        assert_eq!(backend.calls.guilds.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn retryable_failure_is_retried_after_backoff() {
        let backend = Arc::new(FakeBackend::new());
        backend.fail_guilds_times.store(1, Ordering::SeqCst);
        let (hydrator, _freshness, clock, mut rx) = hydrator_for_test(backend.clone());

        hydrator.request(HydrationRequest::normal(
            HydrationKey::Guilds,
            HydrationReason::Startup,
        ));
        let n = hydrator.run_once().await;
        assert_eq!(n, 1, "the failing attempt still counts as an executed job");
        assert_eq!(backend.calls.guilds.load(Ordering::SeqCst), 1);
        assert!(rx.try_recv().is_none(), "no envelope on failure");

        // Not ready yet: backoff has not elapsed.
        let n_none = hydrator.run_once().await;
        assert_eq!(n_none, 0);

        clock.advance(Dur::from_secs(2));
        let n2 = hydrator.run_once().await;
        // The retry succeeds and cascades into one GuildChannels job.
        assert_eq!(n2, 2);
        assert_eq!(backend.calls.guilds.load(Ordering::SeqCst), 2);
        let env = rx.try_recv().expect("envelope after successful retry");
        assert_eq!(env.event.kind(), "guilds_snapshot");
    }

    #[tokio::test]
    async fn offline_error_pauses_and_reconnect_resumes_and_requests_core_set() {
        let backend = Arc::new(FakeBackend::new());
        backend.set_offline(true);
        let (hydrator, _freshness, _clock, _rx) = hydrator_for_test(backend.clone());

        hydrator.request(HydrationRequest::immediate(
            HydrationKey::CurrentUser,
            HydrationReason::Startup,
        ));
        let n = hydrator.run_once().await;
        assert_eq!(n, 1);
        assert!(hydrator.lock_scheduler().is_paused());
        assert!(
            hydrator
                .lock_scheduler()
                .is_pending(&HydrationKey::CurrentUser),
            "returned to pending, not lost"
        );

        backend.set_offline(false);
        hydrator.on_session_changed(&SessionState::Ready);
        assert!(!hydrator.lock_scheduler().is_paused());

        let n2 = hydrator.run_once().await;
        // CurrentUser (already pending) + Relationships/DmSummaries/Guilds
        // from the reconnect core set = 4 jobs, plus the GuildChannels fetch
        // that the now-successful Guilds hydration cascades into = 5.
        assert_eq!(n2, 5);
    }

    #[tokio::test]
    async fn unsupported_capability_is_not_retried() {
        let mut backend = FakeBackend::new();
        backend.unsupported_voice = true;
        let backend = Arc::new(backend);
        let (hydrator, freshness, _clock, _rx) = hydrator_for_test(backend.clone());

        hydrator.request(HydrationRequest::normal(
            HydrationKey::VoiceState,
            HydrationReason::Startup,
        ));
        let n = hydrator.run_once().await;
        assert_eq!(n, 1);
        assert_eq!(backend.calls.voice_state.load(Ordering::SeqCst), 1);
        assert!(!hydrator
            .lock_scheduler()
            .is_pending(&HydrationKey::VoiceState));
        assert!(!hydrator
            .lock_scheduler()
            .is_active(&HydrationKey::VoiceState));

        let n2 = hydrator.run_once().await;
        assert_eq!(n2, 0, "never retried");
        assert_eq!(backend.calls.voice_state.load(Ordering::SeqCst), 1);
        assert_eq!(
            freshness
                .get(&HydrationKey::VoiceState)
                .unwrap()
                .unwrap()
                .failure_count,
            1
        );
    }

    #[tokio::test]
    async fn reconcile_only_requests_stale_keys() {
        let backend = Arc::new(FakeBackend::new());
        let (hydrator, freshness, clock, _rx) = hydrator_for_test(backend.clone());

        freshness
            .mark_fresh(&HydrationKey::CurrentUser, clock.now(), Dur::from_secs(60))
            .unwrap();
        freshness
            .mark_fresh(&HydrationKey::Guilds, clock.now(), Dur::from_secs(5))
            .unwrap();

        clock.advance(Dur::from_secs(10));
        let requested = hydrator.reconcile();
        assert_eq!(requested, 1, "only Guilds has gone stale");
        assert!(hydrator.lock_scheduler().is_pending(&HydrationKey::Guilds));
        assert!(!hydrator
            .lock_scheduler()
            .is_pending(&HydrationKey::CurrentUser));
    }
}
