//! Omni: the in-app assistant, powered by the user's own harness
//! (docs/AGENT_HARNESS.md).
//!
//! The service owns the harness sidecar lifecycle (lazy start, idle stop),
//! Omni sessions and their capped transcripts, the harness approval bridge,
//! and "remember this" into unified memory. Streaming text travels on an
//! ephemeral channel and is never written to the database; only completed
//! items are stored.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde::Serialize;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

use litecord_core::config::OmniConfig;
use litecord_core::error::{Error, ErrorKind, Result};
use litecord_harness::{
    Decision, HarnessDriver, HarnessError, HarnessEvent, HarnessKind, HarnessLauncher,
    HarnessRequest, ItemKind, LaunchContext, LoginState, OmniMode, RequestKind, SessionConfig,
};
use litecord_memory::MemoryService;
use litecord_store::repos;
use litecord_store::repos::omni::NewSession;
pub use litecord_store::repos::omni::{OmniItem, OmniSession};
use litecord_store::Database;
use litecord_types::entity::{EntityId, LocalEntityKind};
use litecord_types::memory::{MemoryKind, NewMemory};
use litecord_types::provenance::{Origin, SourceRef};
use litecord_types::MemoryId;

const SELECTED_KEY: &str = "omni.harness";
/// Items loaded into the view for the active session.
const VIEW_ITEMS: u32 = 200;
const HB_ENABLED: &str = "omni.heartbeat.enabled";
const HB_REVISION: &str = "omni.heartbeat.revision";
const HB_TIMES: &str = "omni.heartbeat.times";
const HB_DISMISSED: &str = "omni.heartbeat.dismissed_at";

/// Result of [`OmniService::heartbeat`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum HeartbeatOutcome {
    Skipped(&'static str),
    Sent { session_id: i64 },
}

/// A check-in result worth the user's attention (not `HEARTBEAT_OK`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OmniCheckin {
    pub session_id: i64,
    pub seq: u32,
    pub text: String,
    pub created_at: litecord_types::Timestamp,
}

/// Quiet hours wrap midnight when `start > end`; equal values disable them.
pub fn in_quiet_hours(hour: u8, start: u8, end: u8) -> bool {
    match start.cmp(&end) {
        std::cmp::Ordering::Equal => false,
        std::cmp::Ordering::Less => hour >= start && hour < end,
        std::cmp::Ordering::Greater => hour >= start || hour < end,
    }
}

/// Ephemeral events for the UI. Reload [`OmniService::view`] on `Changed`.
#[derive(Debug, Clone, PartialEq)]
pub enum OmniEvent {
    /// Streaming reply text for a session (append to the live bubble).
    Delta { session_id: i64, text: String },
    /// Something persisted or the status changed.
    Changed { session_id: Option<i64> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HarnessInfo {
    pub kind: HarnessKind,
    pub label: &'static str,
    pub installed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OmniStatus {
    pub harnesses: Vec<HarnessInfo>,
    pub selected: Option<HarnessKind>,
    pub login: LoginState,
    /// The sidecar process is running.
    pub running: bool,
    /// Why Omni cannot start at all (e.g. in-memory database), if so.
    pub unavailable: Option<String>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OmniSessionRow {
    pub id: i64,
    pub title: String,
    pub kind: String,
    pub mode: OmniMode,
    pub harness: String,
    pub turns: u32,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub last_active_at: litecord_types::Timestamp,
    pub running: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OmniRequestRow {
    pub id: String,
    pub session_id: i64,
    pub request: RequestKind,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OmniViewModel {
    pub status: OmniStatus,
    pub sessions: Vec<OmniSessionRow>,
    pub active: Option<OmniSessionRow>,
    pub items: Vec<OmniItem>,
    /// Reply text streamed so far for the active session's running turn.
    pub streaming: Option<String>,
    pub requests: Vec<OmniRequestRow>,
    /// Recent check-in results for the inbox.
    pub checkins: Vec<OmniCheckin>,
    pub heartbeat_enabled: bool,
}

#[derive(Default)]
struct Live {
    by_external: HashMap<String, i64>,
    /// Sessions opened on the current sidecar.
    attached: HashSet<i64>,
    streaming: HashMap<i64, String>,
    running: HashSet<i64>,
    pending: Vec<OmniRequestRow>,
    login: Option<LoginState>,
    last_used: Option<Instant>,
    last_error: Option<String>,
}

#[derive(Default)]
struct Runtime {
    driver: Option<Arc<dyn HarnessDriver>>,
    pump: Option<JoinHandle<()>>,
}

struct Shared {
    db: Database,
    memory: MemoryService,
    cfg: OmniConfig,
    launchers: Vec<Arc<dyn HarnessLauncher>>,
    ctx: Option<LaunchContext>,
    tx: broadcast::Sender<OmniEvent>,
    rt: tokio::sync::Mutex<Runtime>,
    live: Mutex<Live>,
}

/// Cheap to clone.
#[derive(Clone)]
pub struct OmniService {
    shared: Arc<Shared>,
}

impl std::fmt::Debug for OmniService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OmniService").finish_non_exhaustive()
    }
}

fn harness_err(e: HarnessError) -> Error {
    let kind = match &e {
        HarnessError::NotInstalled(_) | HarnessError::Unsupported(_) => ErrorKind::Unsupported,
        HarnessError::SignedOut => ErrorKind::Authentication,
        HarnessError::NotRunning | HarnessError::Timeout => ErrorKind::Offline,
        HarnessError::Protocol(_) | HarnessError::Harness(_) => ErrorKind::Agent,
    };
    Error::new(kind, e.to_string())
}

/// Instructions for a session: the Omni prompt plus the mode's scope.
pub fn session_instructions(mode: OmniMode) -> String {
    let scope = match mode {
        OmniMode::Assistant => {
            "Mode: Assistant. Only the `litecord` tools are available; shell, file and \
             computer-use requests will be declined."
        }
        OmniMode::Workspace => {
            "Mode: Workspace. Shell and file tools work inside the current directory \
             (sandboxed); each command may need the user's approval."
        }
        OmniMode::ComputerUse => {
            "Mode: Computer use. The user enabled computer-use tools for this session; \
             every action is visible to them and can be stopped."
        }
    };
    format!("{}\n\n{scope}", litecord_agent::prompts::OMNI.trim())
}

impl OmniService {
    pub(crate) fn new(
        db: Database,
        memory: MemoryService,
        cfg: OmniConfig,
        launchers: Vec<Arc<dyn HarnessLauncher>>,
        ctx: Option<LaunchContext>,
    ) -> Self {
        let (tx, _) = broadcast::channel(512);
        Self {
            shared: Arc::new(Shared {
                db,
                memory,
                cfg,
                launchers,
                ctx,
                tx,
                rt: tokio::sync::Mutex::new(Runtime::default()),
                live: Mutex::new(Live::default()),
            }),
        }
    }

    fn live(&self) -> MutexGuard<'_, Live> {
        match self.shared.live.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        }
    }

    fn notify(&self, session_id: Option<i64>) {
        let _ = self.shared.tx.send(OmniEvent::Changed { session_id });
    }

    pub fn subscribe(&self) -> broadcast::Receiver<OmniEvent> {
        self.shared.tx.subscribe()
    }

    /// The selected harness: the stored choice if still available,
    /// otherwise the first installed one.
    pub fn selected(&self) -> Option<HarnessKind> {
        let stored = self
            .shared
            .db
            .read(|r| repos::app_state::get(r, SELECTED_KEY))
            .ok()
            .flatten()
            .and_then(|s| HarnessKind::parse(&s));
        let installed = |k: HarnessKind| {
            self.shared
                .launchers
                .iter()
                .any(|l| l.kind() == k && l.installed())
        };
        stored.filter(|k| installed(*k)).or_else(|| {
            self.shared
                .launchers
                .iter()
                .find(|l| l.installed())
                .map(|l| l.kind())
        })
    }

    pub fn status(&self) -> OmniStatus {
        let harnesses = self
            .shared
            .launchers
            .iter()
            .map(|l| HarnessInfo {
                kind: l.kind(),
                label: l.kind().label(),
                installed: l.installed(),
            })
            .collect();
        let selected = self.selected();
        let running = self
            .shared
            .rt
            .try_lock()
            .map(|rt| rt.driver.is_some())
            .unwrap_or(true);
        let live = self.live();
        let login = match (selected, &live.login) {
            (None, _) => LoginState::NotInstalled,
            (Some(_), Some(l)) => l.clone(),
            (Some(_), None) => LoginState::Stopped,
        };
        OmniStatus {
            harnesses,
            selected,
            login,
            running,
            unavailable: self.shared.ctx.is_none().then(|| {
                "Omni needs Litecord's database on disk (it is running in memory)".to_owned()
            }),
            last_error: live.last_error.clone(),
        }
    }

    /// Choose the harness. Stops a running sidecar of another kind.
    pub async fn select(&self, kind: HarnessKind) -> Result<()> {
        if !self
            .shared
            .launchers
            .iter()
            .any(|l| l.kind() == kind && l.installed())
        {
            return Err(Error::new(
                ErrorKind::Unsupported,
                format!("{} is not installed", kind.label()),
            ));
        }
        self.shared
            .db
            .write(|tx| repos::app_state::set(tx, SELECTED_KEY, kind.as_str()))?;
        let running_other = {
            let rt = self.shared.rt.lock().await;
            rt.driver.as_ref().is_some_and(|d| d.kind() != kind)
        };
        if running_other {
            self.stop().await;
        }
        self.notify(None);
        Ok(())
    }

    /// Start the sidecar if needed.
    async fn driver(&self) -> Result<Arc<dyn HarnessDriver>> {
        let mut rt = self.shared.rt.lock().await;
        if let Some(d) = &rt.driver {
            return Ok(d.clone());
        }
        let ctx = self.shared.ctx.as_ref().ok_or_else(|| {
            Error::new(
                ErrorKind::Unsupported,
                "Omni needs Litecord's database on disk",
            )
        })?;
        let kind = self.selected().ok_or_else(|| {
            Error::new(
                ErrorKind::Unsupported,
                "no agent harness installed (Codex or OpenCode)",
            )
        })?;
        let launcher = self
            .shared
            .launchers
            .iter()
            .find(|l| l.kind() == kind)
            .ok_or_else(|| Error::internal("selected harness has no launcher"))?;
        let driver = launcher.launch(ctx).await.map_err(|e| {
            self.live().last_error = Some(e.to_string());
            harness_err(e)
        })?;
        let rx = driver.subscribe();
        let me = self.clone();
        let d2 = driver.clone();
        rt.pump = Some(tokio::spawn(async move { me.pump(d2, rx).await }));
        rt.driver = Some(driver.clone());
        {
            let mut live = self.live();
            live.attached.clear();
            live.by_external.clear();
            live.last_error = None;
            live.last_used = Some(Instant::now());
        }
        drop(rt);
        match driver.login_state().await {
            Ok(l) => self.live().login = Some(l),
            Err(e) => self.live().last_error = Some(e.to_string()),
        }
        self.notify(None);
        Ok(driver)
    }

    /// Query the harness for its sign-in state (starts the sidecar).
    pub async fn refresh_login(&self) -> Result<LoginState> {
        let d = self.driver().await?;
        let l = d.login_state().await.map_err(harness_err)?;
        self.live().login = Some(l.clone());
        self.notify(None);
        Ok(l)
    }

    /// Start the harness's own sign-in. Open `SigningIn.url` in a browser.
    pub async fn sign_in(&self) -> Result<LoginState> {
        let d = self.driver().await?;
        let l = d.begin_login().await.map_err(harness_err)?;
        self.live().login = Some(l.clone());
        self.notify(None);
        Ok(l)
    }

    pub async fn sign_out(&self) -> Result<()> {
        let d = self.driver().await?;
        d.logout().await.map_err(harness_err)?;
        self.live().login = Some(LoginState::SignedOut);
        self.notify(None);
        Ok(())
    }

    /// Create a chat session (the harness side opens on the first message).
    pub fn new_session(&self, mode: OmniMode, title: &str) -> Result<i64> {
        let harness = self.selected().map_or("none", |k| k.as_str());
        let id = self
            .shared
            .db
            .write(|tx| {
                repos::omni::create_session(
                    tx,
                    &NewSession {
                        harness,
                        kind: "chat",
                        mode: mode.as_str(),
                        profile: None,
                        title,
                        parent_id: None,
                    },
                )
            })?
            .value;
        self.notify(Some(id));
        Ok(id)
    }

    fn session(&self, id: i64) -> Result<OmniSession> {
        self.shared
            .db
            .read(|r| repos::omni::get(r, id))?
            .ok_or_else(|| Error::not_found(format!("omni session {id}")))
    }

    fn session_config(&self, mode: OmniMode) -> Result<SessionConfig> {
        let ctx = self
            .shared
            .ctx
            .as_ref()
            .ok_or_else(|| Error::new(ErrorKind::Unsupported, "Omni is unavailable"))?;
        Ok(SessionConfig {
            instructions: session_instructions(mode),
            mode,
            model: self.shared.cfg.model.clone(),
            cwd: ctx.workspace.clone(),
            protected_dir: ctx.protected_dir.clone(),
            mcp: ctx.mcp.clone(),
        })
    }

    /// Open the harness side of a session on the current sidecar.
    async fn attach(&self, d: &Arc<dyn HarnessDriver>, s: &OmniSession) -> Result<String> {
        if self.live().attached.contains(&s.id) {
            if let Some(ext) = &s.external_id {
                return Ok(ext.clone());
            }
        }
        let mode = OmniMode::parse(&s.mode).unwrap_or_default();
        let cfg = self.session_config(mode)?;
        let resumed = match &s.external_id {
            Some(ext) => d.resume_session(ext, &cfg).await.ok(),
            None => None,
        };
        let ext = match resumed {
            Some(ext) => ext,
            None => d.start_session(&cfg).await.map_err(harness_err)?,
        };
        let id = s.id;
        self.shared
            .db
            .write(|tx| repos::omni::set_external_id(tx, id, Some(&ext)))?;
        let mut live = self.live();
        live.attached.insert(id);
        live.by_external.insert(ext.clone(), id);
        Ok(ext)
    }

    /// Send a message to Omni. Creates a session when `session_id` is None.
    pub async fn send(&self, session_id: Option<i64>, text: &str) -> Result<i64> {
        let text = text.trim();
        if text.is_empty() {
            return Err(Error::validation("message is empty"));
        }
        let id = match session_id {
            Some(id) => id,
            None => {
                let title: String = text.chars().take(60).collect();
                self.new_session(OmniMode::Assistant, &title)?
            }
        };
        self.send_as(id, "user", text).await?;
        Ok(id)
    }

    async fn send_as(&self, id: i64, role: &str, text: &str) -> Result<()> {
        let session = self.session(id)?;
        if session.status != "active" {
            return Err(Error::validation("session is archived"));
        }
        if self.live().running.contains(&id) {
            return Err(Error::validation(
                "Omni is still working; wait or stop the turn",
            ));
        }
        self.shared
            .db
            .write(|tx| repos::omni::append_item(tx, id, role, ItemKind::Message.as_str(), text))?;
        self.notify(Some(id));
        let d = self.driver().await?;
        let ext = self.attach(&d, &session).await?;
        {
            let mut live = self.live();
            live.running.insert(id);
            live.streaming.remove(&id);
            live.last_used = Some(Instant::now());
        }
        if let Err(e) = d.send(&ext, text).await {
            self.live().running.remove(&id);
            return Err(harness_err(e));
        }
        self.notify(Some(id));
        Ok(())
    }

    // ---- heartbeats (docs/AGENT_HARNESS.md §8) ----

    /// Whether scheduled check-ins are on (user toggle, else config).
    pub fn heartbeat_enabled(&self) -> bool {
        self.shared
            .db
            .read(|r| repos::app_state::get(r, HB_ENABLED))
            .ok()
            .flatten()
            .map_or(self.shared.cfg.heartbeat.enabled, |v| v == "true")
    }

    pub fn set_heartbeat_enabled(&self, enabled: bool) -> Result<()> {
        self.shared.db.write(|tx| {
            repos::app_state::set(tx, HB_ENABLED, if enabled { "true" } else { "false" })
        })?;
        self.notify(None);
        Ok(())
    }

    /// Hide current check-in results from the inbox.
    pub fn dismiss_checkins(&self) -> Result<()> {
        let now = self.shared.db.now().as_millis().to_string();
        self.shared
            .db
            .write(|tx| repos::app_state::set(tx, HB_DISMISSED, &now))?;
        self.notify(None);
        Ok(())
    }

    /// Run a check-in if it is due. `force` (the "Check now" command)
    /// bypasses the schedule and the "nothing changed" skip, but not the
    /// sign-in or busy checks.
    pub async fn heartbeat(&self, force: bool) -> Result<HeartbeatOutcome> {
        use HeartbeatOutcome::Skipped;
        let hb = self.shared.cfg.heartbeat.clone();
        if !force && !self.heartbeat_enabled() {
            return Ok(Skipped("disabled"));
        }
        if self.shared.ctx.is_none() || self.selected().is_none() {
            return Ok(Skipped("unavailable"));
        }
        let now = self.shared.db.now().as_millis();
        let hour = ((now / 3_600_000).rem_euclid(24)) as u8;
        if !force && in_quiet_hours(hour, hb.quiet_start_hour, hb.quiet_end_hour) {
            return Ok(Skipped("quiet hours"));
        }
        let db = &self.shared.db;
        let (last_rev, mut times) = db.read(|r| -> Result<(u64, Vec<i64>)> {
            let rev = repos::app_state::get(r, HB_REVISION)?
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            let times = repos::app_state::get(r, HB_TIMES)?
                .and_then(|v| serde_json::from_str(&v).ok())
                .unwrap_or_default();
            Ok((rev, times))
        })?;
        times.retain(|t: &i64| now - t < 3_600_000);
        if !force {
            let min_gap = i64::from(hb.min_interval_mins) * 60_000;
            if times.iter().any(|t| now - t < min_gap) {
                return Ok(Skipped("too soon"));
            }
            if times.len() as u32 >= hb.max_per_hour {
                return Ok(Skipped("hourly limit"));
            }
        }
        let current = db.current_revision()?.get();
        let counts = db.read(|r| {
            repos::events::kind_counts_since(
                r,
                litecord_types::Revision(last_rev),
                Origin::AgentDerived.as_str(),
            )
        })?;
        if !force && counts.is_empty() {
            return Ok(Skipped("nothing changed"));
        }
        if !self.live().running.is_empty() {
            return Ok(Skipped("busy"));
        }
        if !self.refresh_login().await?.is_ready() {
            return Ok(Skipped("signed out"));
        }
        let session_id = self.heartbeat_session()?;
        let delta = if counts.is_empty() {
            "- no new events".to_owned()
        } else {
            counts
                .iter()
                .map(|(k, n)| format!("- {k}: {n}"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let at = format!("{:02}:{:02} UTC", hour, (now / 60_000).rem_euclid(60));
        let prompt = litecord_agent::prompts::HeartbeatPrompt {
            at: &at,
            from_revision: last_rev,
            to_revision: current,
            delta: &delta,
            max_actions: hb.max_actions,
            allow_proposals: hb.allow_proposals,
        }
        .render();
        times.push(now);
        let times_json = serde_json::to_string(&times).unwrap_or_else(|_| "[]".into());
        db.write(|tx| -> litecord_store::StoreResult<()> {
            repos::app_state::set(tx, HB_REVISION, &current.to_string())?;
            repos::app_state::set(tx, HB_TIMES, &times_json)?;
            Ok(())
        })?;
        self.send_as(session_id, "system", &prompt).await?;
        Ok(HeartbeatOutcome::Sent { session_id })
    }

    /// The rolling check-in session (always Assistant mode). Rotated after
    /// a number of turns so each check-in stays cheap.
    fn heartbeat_session(&self) -> Result<i64> {
        const ROTATE_AFTER_TURNS: u32 = 20;
        let current = self
            .shared
            .db
            .read(|r| repos::omni::list(r, Some("heartbeat"), false, 1))?
            .into_iter()
            .next();
        match current {
            Some(s) if s.turns < ROTATE_AFTER_TURNS => Ok(s.id),
            other => {
                if let Some(old) = other {
                    self.archive(old.id)?;
                }
                let harness = self.selected().map_or("none", |k| k.as_str());
                Ok(self
                    .shared
                    .db
                    .write(|tx| {
                        repos::omni::create_session(
                            tx,
                            &NewSession {
                                harness,
                                kind: "heartbeat",
                                mode: OmniMode::Assistant.as_str(),
                                profile: None,
                                title: "Check-ins",
                                parent_id: None,
                            },
                        )
                    })?
                    .value)
            }
        }
    }

    fn checkins(&self) -> Result<Vec<OmniCheckin>> {
        let db = &self.shared.db;
        db.read(|r| -> Result<Vec<OmniCheckin>> {
            let dismissed: i64 = repos::app_state::get(r, HB_DISMISSED)?
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            let mut out = Vec::new();
            for s in repos::omni::list(r, Some("heartbeat"), true, 3)? {
                for i in repos::omni::items(r, s.id, 40)? {
                    if i.role == "omni"
                        && i.kind == ItemKind::Message.as_str()
                        && i.created_at.as_millis() > dismissed
                        && !litecord_agent::prompts::is_heartbeat_ok(&i.text)
                    {
                        out.push(OmniCheckin {
                            session_id: s.id,
                            seq: i.seq,
                            text: i.text,
                            created_at: i.created_at,
                        });
                    }
                }
            }
            out.sort_by_key(|c| std::cmp::Reverse(c.created_at));
            out.truncate(5);
            Ok(out)
        })
    }

    fn external_of(&self, session_id: i64) -> Result<String> {
        self.session(session_id)?
            .external_id
            .filter(|_| self.live().attached.contains(&session_id))
            .ok_or_else(|| Error::validation("session is not open in the harness"))
    }

    pub async fn interrupt(&self, session_id: i64) -> Result<()> {
        let ext = self.external_of(session_id)?;
        let d = self.driver().await?;
        d.interrupt(&ext).await.map_err(harness_err)?;
        self.live().running.remove(&session_id);
        self.notify(Some(session_id));
        Ok(())
    }

    /// Ask the harness to compact the session's context.
    pub async fn compact(&self, session_id: i64) -> Result<()> {
        let d = self.driver().await?;
        let session = self.session(session_id)?;
        let ext = self.attach(&d, &session).await?;
        d.compact(&ext).await.map_err(harness_err)
    }

    pub fn archive(&self, session_id: i64) -> Result<()> {
        let keep = self.shared.cfg.keep_archived_sessions;
        self.shared
            .db
            .write(|tx| -> litecord_store::StoreResult<()> {
                repos::omni::archive(tx, session_id)?;
                repos::omni::prune_archived(tx, keep)?;
                Ok(())
            })?;
        self.notify(Some(session_id));
        Ok(())
    }

    /// Answer a harness approval request (local command / file change).
    pub async fn answer(&self, request_id: &str, decision: Decision) -> Result<()> {
        let removed = {
            let mut live = self.live();
            let before = live.pending.len();
            live.pending.retain(|p| p.id != request_id);
            before != live.pending.len()
        };
        if !removed {
            return Err(Error::not_found("request already answered or expired"));
        }
        let d = self.driver().await?;
        d.answer(request_id, decision).await.map_err(harness_err)?;
        self.notify(None);
        Ok(())
    }

    /// Save an Omni transcript item as a memory candidate. It is
    /// `AgentDerived`, cites its session, and waits for the user to confirm
    /// it in Memory like any other extracted fact.
    pub fn remember(&self, session_id: i64, seq: u32) -> Result<MemoryId> {
        let item = self
            .shared
            .db
            .read(|r| repos::omni::items(r, session_id, u32::MAX))?
            .into_iter()
            .find(|i| i.seq == seq)
            .ok_or_else(|| Error::not_found("transcript item"))?;
        let content: String = item.text.chars().take(600).collect();
        let mut m = NewMemory::new(MemoryKind::Observation, content, Origin::AgentDerived);
        m.source_refs = vec![SourceRef {
            entity: EntityId::Local(
                LocalEntityKind::OmniSession,
                litecord_types::LocalEntityId(session_id),
            ),
            note: Some(format!("Omni item {seq}")),
        }];
        m.observed_at = Some(item.created_at);
        let outcome = self
            .shared
            .memory
            .record(m)
            .map_err(|e| Error::internal(e.to_string()))?;
        Ok(match outcome {
            litecord_memory::RecordOutcome::Inserted(id)
            | litecord_memory::RecordOutcome::Reinforced(id)
            | litecord_memory::RecordOutcome::Superseded { new: id, .. } => id,
        })
    }

    pub fn view(&self, session_id: Option<i64>) -> Result<OmniViewModel> {
        let status = self.status();
        let (running, streaming_all, requests) = {
            let live = self.live();
            (
                live.running.clone(),
                live.streaming.clone(),
                live.pending.clone(),
            )
        };
        let row = |s: &OmniSession| OmniSessionRow {
            id: s.id,
            title: s.title.clone(),
            kind: s.kind.clone(),
            mode: OmniMode::parse(&s.mode).unwrap_or_default(),
            harness: s.harness.clone(),
            turns: s.turns,
            tokens_in: s.tokens_in,
            tokens_out: s.tokens_out,
            last_active_at: s.last_active_at,
            running: running.contains(&s.id),
        };
        let (sessions, active, items) = self.shared.db.read(|r| -> Result<_> {
            let sessions = repos::omni::list(r, None, false, 50)?;
            let active = match session_id {
                Some(id) => repos::omni::get(r, id)?,
                None => sessions.iter().find(|s| s.kind == "chat").cloned(),
            };
            let items = match &active {
                Some(s) => repos::omni::items(r, s.id, VIEW_ITEMS)?,
                None => Vec::new(),
            };
            Ok((sessions, active, items))
        })?;
        let streaming = active
            .as_ref()
            .and_then(|s| streaming_all.get(&s.id).cloned());
        Ok(OmniViewModel {
            status,
            sessions: sessions.iter().map(row).collect(),
            active: active.as_ref().map(row),
            items,
            streaming,
            requests,
            checkins: self.checkins()?,
            heartbeat_enabled: self.heartbeat_enabled(),
        })
    }

    /// Stop the sidecar (sessions stay resumable).
    pub async fn stop(&self) {
        let (driver, pump) = {
            let mut rt = self.shared.rt.lock().await;
            (rt.driver.take(), rt.pump.take())
        };
        if let Some(d) = driver {
            d.shutdown().await;
        }
        if let Some(p) = pump {
            p.abort();
        }
        {
            let mut live = self.live();
            live.attached.clear();
            live.by_external.clear();
            live.running.clear();
            live.streaming.clear();
            live.pending.clear();
        }
        self.notify(None);
    }

    /// Stop an idle sidecar. Called periodically by the app.
    pub async fn stop_if_idle(&self) {
        let idle = Duration::from_secs(self.shared.cfg.idle_shutdown_secs);
        let should = {
            let live = self.live();
            live.running.is_empty()
                && live.pending.is_empty()
                && !matches!(live.login, Some(LoginState::SigningIn { .. }))
                && live.last_used.is_some_and(|t| t.elapsed() >= idle)
        };
        let running = self.shared.rt.lock().await.driver.is_some();
        if should && running {
            tracing::info!("stopping idle Omni sidecar");
            self.stop().await;
        }
    }

    fn append(&self, session_id: i64, role: &str, kind: ItemKind, text: &str) {
        if let Err(e) = self
            .shared
            .db
            .write(|tx| repos::omni::append_item(tx, session_id, role, kind.as_str(), text))
        {
            tracing::warn!(error = %e, "could not store Omni item");
        }
    }

    async fn pump(self, driver: Arc<dyn HarnessDriver>, mut rx: broadcast::Receiver<HarnessEvent>) {
        loop {
            let ev = match rx.recv().await {
                Ok(ev) => ev,
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(skipped = n, "Omni event stream lagged");
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => break,
            };
            let session_of = |ext: &str| self.live().by_external.get(ext).copied();
            match ev {
                HarnessEvent::TurnStarted { session } => {
                    if let Some(id) = session_of(&session) {
                        self.live().running.insert(id);
                        self.notify(Some(id));
                    }
                }
                HarnessEvent::Delta { session, text } => {
                    if let Some(id) = session_of(&session) {
                        self.live().streaming.entry(id).or_default().push_str(&text);
                        let _ = self.shared.tx.send(OmniEvent::Delta {
                            session_id: id,
                            text,
                        });
                    }
                }
                HarnessEvent::ItemCompleted { session, item } => {
                    if let Some(id) = session_of(&session) {
                        if item.kind == ItemKind::Message {
                            self.live().streaming.remove(&id);
                        }
                        self.append(id, "omni", item.kind, &item.text);
                        self.notify(Some(id));
                    }
                }
                HarnessEvent::TurnCompleted { session, usage } => {
                    if let Some(id) = session_of(&session) {
                        {
                            let mut live = self.live();
                            live.running.remove(&id);
                            live.streaming.remove(&id);
                            live.last_used = Some(Instant::now());
                        }
                        let u = usage.unwrap_or_default();
                        if let Err(e) = self
                            .shared
                            .db
                            .write(|tx| repos::omni::record_turn(tx, id, u.input, u.output))
                        {
                            tracing::warn!(error = %e, "could not record Omni turn");
                        }
                        self.notify(Some(id));
                    }
                }
                HarnessEvent::TurnFailed { session, message } => {
                    if let Some(id) = session_of(&session) {
                        {
                            let mut live = self.live();
                            live.running.remove(&id);
                            live.streaming.remove(&id);
                        }
                        self.append(
                            id,
                            "system",
                            ItemKind::Message,
                            &format!("Error: {message}"),
                        );
                        self.notify(Some(id));
                    }
                }
                HarnessEvent::Request(req) => self.on_request(&driver, req).await,
                HarnessEvent::RequestResolved { id } => {
                    self.live().pending.retain(|p| p.id != id);
                    self.notify(None);
                }
                HarnessEvent::Compacted { session } => {
                    if let Some(id) = session_of(&session) {
                        self.append(id, "system", ItemKind::Compaction, "Context compacted");
                        self.notify(Some(id));
                    }
                }
                HarnessEvent::Login(l) => {
                    self.live().login = Some(l);
                    self.notify(None);
                }
                HarnessEvent::Exited { message } => {
                    {
                        let mut live = self.live();
                        live.last_error = message;
                        live.attached.clear();
                        live.by_external.clear();
                        live.running.clear();
                        live.streaming.clear();
                        live.pending.clear();
                    }
                    let mut rt = self.shared.rt.lock().await;
                    if rt.driver.as_ref().is_some_and(|d| Arc::ptr_eq(d, &driver)) {
                        rt.driver = None;
                        rt.pump = None;
                    }
                    drop(rt);
                    self.notify(None);
                    break;
                }
            }
        }
    }

    async fn on_request(&self, driver: &Arc<dyn HarnessDriver>, req: HarnessRequest) {
        let Some(id) = self.live().by_external.get(&req.session).copied() else {
            let _ = driver.answer(&req.id, Decision::Decline).await;
            return;
        };
        let mode = self
            .session(id)
            .ok()
            .and_then(|s| OmniMode::parse(&s.mode))
            .unwrap_or_default();
        if !mode.allows_local_tools() {
            // Assistant mode: Litecord tools only. Declined without asking.
            let _ = driver.answer(&req.id, Decision::Decline).await;
            self.append(
                id,
                "system",
                ItemKind::Command,
                "Declined a local command: this session is in Assistant mode",
            );
            self.notify(Some(id));
            return;
        }
        self.live().pending.push(OmniRequestRow {
            id: req.id.clone(),
            session_id: id,
            request: req.request,
            reason: req.reason,
        });
        self.notify(Some(id));
        // Unanswered requests are declined after the timeout.
        let me = self.clone();
        let d = driver.clone();
        let timeout = Duration::from_secs(self.shared.cfg.approval_timeout_secs);
        tokio::spawn(async move {
            tokio::time::sleep(timeout).await;
            let still = {
                let mut live = me.live();
                let before = live.pending.len();
                live.pending.retain(|p| p.id != req.id);
                before != live.pending.len()
            };
            if still {
                let _ = d.answer(&req.id, Decision::Decline).await;
                me.notify(Some(id));
            }
        });
    }
}
