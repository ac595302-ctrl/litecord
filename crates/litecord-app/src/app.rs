//! The application object: owns the database, runtime tasks and services.

use std::sync::{Arc, RwLock};
use std::time::Duration;

use tokio::sync::broadcast;

use discord_adapter::MockBackend;
use litecord_actions::{ActionEngine, DefaultExecutor};
use litecord_core::bus::{ingest_channel, AppEventBus};
use litecord_core::clock::{SharedClock, SystemClock};
use litecord_core::config::{BackendKind, LitecordConfig};
use litecord_core::events::ApplicationEvent;
use litecord_core::metrics::Metrics;
use litecord_core::ports::SocialBackend;
use litecord_core::runtime::{ShutdownReport, TaskSupervisor};
use litecord_core::{Error, ErrorKind, Result};
use litecord_features::builtin::builtin_registry;
use litecord_features::command::CommandRegistry;
use litecord_features::feature::FeatureRegistry;
use litecord_hydrator::Hydrator;
use litecord_memory::{MemoryService, ReminderEngine, TaskService};
use litecord_store::reducer::ReducerConfig;
use litecord_store::Database;

use crate::freshness::StoreFreshness;
use crate::runtime;

/// Configures and starts a [`LitecordApp`].
#[derive(Debug)]
pub struct AppBuilder {
    cfg: LitecordConfig,
    backend: Option<Arc<dyn SocialBackend>>,
    clock: SharedClock,
    in_memory: bool,
}

impl AppBuilder {
    pub fn new(cfg: LitecordConfig) -> Self {
        Self {
            cfg,
            backend: None,
            clock: Arc::new(SystemClock),
            in_memory: false,
        }
    }

    /// Use a specific backend instead of the one selected by configuration.
    pub fn backend(mut self, backend: Arc<dyn SocialBackend>) -> Self {
        self.backend = Some(backend);
        self
    }

    pub fn clock(mut self, clock: SharedClock) -> Self {
        self.clock = clock;
        self
    }

    /// Use a private in-memory database (tests, demos).
    pub fn in_memory(mut self) -> Self {
        self.in_memory = true;
        self
    }

    /// Open storage, spawn runtime tasks, connect the backend and start the
    /// initial hydration. Must be called inside a Tokio runtime.
    pub async fn start(self) -> Result<LitecordApp> {
        let span = tracing::info_span!("startup");
        let _g = span.enter();
        let cfg = self.cfg;
        cfg.validate()?;
        let metrics = Metrics::new();
        let db = if self.in_memory {
            Database::open_in_memory_with(self.clock.clone(), Some(metrics.clone()))?
        } else {
            Database::open_with(
                cfg.database_path(),
                &cfg.database,
                self.clock.clone(),
                Some(metrics.clone()),
            )?
        };
        let backend: Arc<dyn SocialBackend> = match self.backend {
            Some(b) => b,
            None => match cfg.backend.kind {
                BackendKind::Demo => Arc::new(MockBackend::with_clock(
                    discord_adapter::fixtures::generate(cfg.backend.demo_seed, self.clock.now()),
                    self.clock.clone(),
                )),
                BackendKind::SocialSdk => return Err(Error::new(
                    ErrorKind::Unsupported,
                    "the Social SDK backend is not wired yet; see docs/IMPLEMENTATION_STATUS.md",
                )),
            },
        };
        drop(_g);

        let bus = AppEventBus::new(cfg.runtime.broadcast_capacity);
        let (ingest, rx) = ingest_channel(cfg.runtime.event_queue_capacity);
        let hydrator = Hydrator::new(
            backend.clone(),
            ingest.clone(),
            Arc::new(StoreFreshness::new(db.clone())),
            &cfg.hydration,
            self.clock.clone(),
            metrics.clone(),
        );
        let memory = MemoryService::with_heuristics(db.clone());
        let executor = Arc::new(DefaultExecutor::new(db.clone(), Some(backend.clone())));
        let actions = ActionEngine::new(db.clone(), &cfg.agent, executor)?;
        let features =
            builtin_registry().map_err(|e| Error::internal(format!("feature registry: {e}")))?;
        let mut commands = CommandRegistry::new();
        features
            .install_commands(&mut commands)
            .map_err(|e| Error::internal(format!("command registry: {e}")))?;

        let supervisor = TaskSupervisor::new();
        let ctx = runtime::ReactorCtx {
            db: db.clone(),
            bus: bus.clone(),
            hydrator: hydrator.clone(),
            memory: memory.clone(),
            metrics: metrics.clone(),
            ingest: ingest.clone(),
            reducer: ReducerConfig {
                purge_deleted_messages: cfg.retention.purge_deleted_messages,
            },
        };
        supervisor.spawn("event-reactor", |t| runtime::event_reactor(ctx, rx, t));
        {
            let h = hydrator.clone();
            supervisor.spawn("hydrator", move |t| h.run(t));
        }
        {
            let h = hydrator.clone();
            let every = Duration::from_secs(cfg.hydration.reconcile_interval_secs.max(1));
            supervisor.spawn("reconciler", move |t| runtime::reconciler(h, every, t));
        }
        {
            let engine = ReminderEngine::new(db.clone());
            let (db2, bus2) = (db.clone(), bus.clone());
            supervisor.spawn("reminder-ticker", move |t| {
                runtime::reminder_ticker(engine, db2, bus2, Duration::from_secs(15), t)
            });
        }
        {
            let (m, db2, c) = (memory.clone(), db.clone(), cfg.clone());
            supervisor.spawn("maintenance", move |t| {
                runtime::maintenance(m, db2, c, Duration::from_secs(3_600), t)
            });
        }
        {
            let (db2, bus2) = (db.clone(), bus.clone());
            supervisor.spawn("action-watcher", move |t| {
                runtime::action_watcher(db2, bus2, Duration::from_secs(3), t)
            });
        }

        backend.connect(ingest.clone()).await?;
        if let Err(e) = crate::recovery::restore(&db, &hydrator) {
            tracing::warn!(error = %e, "could not restore hydration queue");
        }
        hydrator.initial_hydration();
        tracing::info!(mode = ?backend.mode(), "litecord started");

        Ok(LitecordApp {
            inner: Arc::new(AppInner {
                tasks: TaskService::new(db.clone()),
                cfg,
                db,
                bus,
                backend,
                hydrator,
                memory,
                actions,
                metrics,
                supervisor,
                features: RwLock::new(features),
                commands: RwLock::new(commands),
            }),
        })
    }
}

pub(crate) struct AppInner {
    pub cfg: LitecordConfig,
    pub db: Database,
    pub bus: AppEventBus,
    pub backend: Arc<dyn SocialBackend>,
    pub hydrator: Arc<Hydrator>,
    pub memory: MemoryService,
    pub tasks: TaskService,
    pub actions: ActionEngine,
    pub metrics: Arc<Metrics>,
    pub supervisor: TaskSupervisor,
    pub features: RwLock<FeatureRegistry>,
    pub commands: RwLock<CommandRegistry>,
}

/// Handle to a running Litecord instance. Cheap to clone; all methods are
/// safe to call from any thread. This is the object a UI binds to.
#[derive(Clone)]
pub struct LitecordApp {
    pub(crate) inner: Arc<AppInner>,
}

impl std::fmt::Debug for LitecordApp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LitecordApp")
            .field("db", &self.inner.db)
            .finish_non_exhaustive()
    }
}

impl LitecordApp {
    pub fn builder(cfg: LitecordConfig) -> AppBuilder {
        AppBuilder::new(cfg)
    }

    /// Subscribe to application events (state changes, session, reminders,
    /// approvals). On `RecvError::Lagged`, reload views from the store.
    pub fn subscribe(&self) -> broadcast::Receiver<ApplicationEvent> {
        self.inner.bus.subscribe()
    }

    pub fn config(&self) -> &LitecordConfig {
        &self.inner.cfg
    }

    /// The database, for composing additional services (e.g. the agent
    /// gateway). UI code should use the view-model methods instead.
    pub fn database(&self) -> &Database {
        &self.inner.db
    }

    /// Full-authority Action Engine. Only the application/UI layer may hold
    /// this; agents receive `actions().proposer()`.
    pub fn actions(&self) -> &ActionEngine {
        &self.inner.actions
    }

    pub fn memory(&self) -> &MemoryService {
        &self.inner.memory
    }

    pub fn hydrator(&self) -> &Arc<Hydrator> {
        &self.inner.hydrator
    }

    pub fn metrics(&self) -> &Arc<Metrics> {
        &self.inner.metrics
    }

    /// Stop all runtime tasks and disconnect the backend.
    pub async fn shutdown(&self) -> ShutdownReport {
        if let Err(e) = self.inner.backend.disconnect().await {
            tracing::warn!(error = %e, "backend disconnect failed");
        }
        let grace = Duration::from_millis(self.inner.cfg.runtime.shutdown_grace_ms);
        let report = self.inner.supervisor.shutdown(grace).await;
        // Tasks are stopped, so the queue is stable now.
        match crate::recovery::persist(&self.inner.db, &self.inner.hydrator) {
            Ok(n) if n > 0 => tracing::info!(persisted = n, "hydration queue saved"),
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "could not persist hydration queue"),
        }
        report
    }
}

impl LitecordApp {
    /// Build the harness-neutral agent gateway over this app's memory. The
    /// gateway receives only a propose-only handle to the Action Engine.
    pub fn agent_gateway(&self) -> litecord_agent::AgentGateway {
        agent_gateway_for(
            &self.inner.db,
            &self.inner.cfg,
            self.inner.actions.proposer(),
            Some(self.inner.metrics.clone()),
        )
    }
}

/// Build an agent gateway over a database without a running app — used by
/// the standalone MCP process, which has no Discord backend: local writes
/// execute, Discord writes can only be proposed (the app executes them after
/// user approval).
pub fn standalone_gateway(
    db: Database,
    cfg: &LitecordConfig,
) -> Result<litecord_agent::AgentGateway> {
    let executor = Arc::new(DefaultExecutor::new(db.clone(), None));
    let engine = ActionEngine::new(db.clone(), &cfg.agent, executor)?;
    Ok(agent_gateway_for(&db, cfg, engine.proposer(), None))
}

fn agent_gateway_for(
    db: &Database,
    cfg: &LitecordConfig,
    proposer: litecord_actions::ActionProposer,
    metrics: Option<Arc<Metrics>>,
) -> litecord_agent::AgentGateway {
    use litecord_context::{CompilerConfig, ContextCompiler};
    use litecord_retrieval::{Retriever, ScoringWeights};
    let mut retriever = Retriever::new(ScoringWeights::default());
    if let Some(m) = &metrics {
        retriever = retriever.with_metrics(m.clone());
    }
    let mut compiler = ContextCompiler::new(
        db.clone(),
        retriever.clone(),
        CompilerConfig {
            default_visibility: cfg.agent.default_visibility,
            ..CompilerConfig::default()
        },
    );
    if let Some(m) = metrics {
        compiler = compiler.with_metrics(m);
    }
    litecord_agent::AgentGateway::new(
        db.clone(),
        Arc::new(compiler),
        Arc::new(retriever),
        proposer,
        &cfg.agent,
    )
}
