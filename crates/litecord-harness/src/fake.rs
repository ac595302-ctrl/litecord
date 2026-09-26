//! Scripted driver for tests and the demo build. It performs no reasoning:
//! each turn replays what the responder returns.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::sync::{broadcast, oneshot};

use crate::driver::{HarnessDriver, HarnessLauncher, LaunchContext};
use crate::types::*;

/// One scripted turn.
#[derive(Debug, Clone, Default)]
pub struct FakeTurn {
    pub reply: String,
    /// Ask for approval before replying.
    pub request: Option<RequestKind>,
    /// Extra completed items (tool calls, subagents, plans) before the reply.
    pub items: Vec<TranscriptItem>,
    pub fail: Option<String>,
}

type Responder = dyn Fn(&str) -> FakeTurn + Send + Sync;

#[derive(Default)]
struct State {
    login: Option<LoginState>,
    sessions: Vec<String>,
    sent: Vec<(String, String)>,
    pending: HashMap<String, oneshot::Sender<Decision>>,
    compacted: Vec<String>,
    interrupted: Vec<String>,
    stopped: bool,
    next_request: u64,
}

#[derive(Clone)]
pub struct FakeDriver {
    tx: broadcast::Sender<HarnessEvent>,
    state: Arc<Mutex<State>>,
    responder: Arc<Responder>,
}

impl std::fmt::Debug for FakeDriver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FakeDriver")
    }
}

impl FakeDriver {
    pub fn new(responder: impl Fn(&str) -> FakeTurn + Send + Sync + 'static) -> Self {
        let (tx, _) = broadcast::channel(256);
        Self {
            tx,
            state: Arc::new(Mutex::new(State {
                login: Some(LoginState::Ready {
                    account: Some("Demo".into()),
                }),
                ..State::default()
            })),
            responder: Arc::new(responder),
        }
    }

    /// Replies with a fixed, clearly labelled demo message.
    pub fn demo() -> Self {
        Self::new(|_| FakeTurn {
            reply: "This is the demo harness, not a model. Connect Codex or OpenCode in \
                    Settings (Omni section) to get real answers."
                .into(),
            ..FakeTurn::default()
        })
    }

    pub fn signed_out(self) -> Self {
        self.lock().login = Some(LoginState::SignedOut);
        self
    }

    /// Simulates the browser completing sign-in.
    pub fn complete_login(&self) {
        let state = LoginState::Ready {
            account: Some("Demo".into()),
        };
        self.lock().login = Some(state.clone());
        let _ = self.tx.send(HarnessEvent::Login(state));
    }

    pub fn sent(&self) -> Vec<(String, String)> {
        self.lock().sent.clone()
    }

    pub fn compacted(&self) -> Vec<String> {
        self.lock().compacted.clone()
    }

    pub fn interrupted(&self) -> Vec<String> {
        self.lock().interrupted.clone()
    }

    pub fn is_stopped(&self) -> bool {
        self.lock().stopped
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        match self.state.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        }
    }

    fn emit(&self, e: HarnessEvent) {
        let _ = self.tx.send(e);
    }

    fn check_running(&self) -> HarnessResult<()> {
        if self.lock().stopped {
            Err(HarnessError::NotRunning)
        } else {
            Ok(())
        }
    }
}

#[async_trait]
impl HarnessDriver for FakeDriver {
    fn kind(&self) -> HarnessKind {
        HarnessKind::Fake
    }

    fn subscribe(&self) -> broadcast::Receiver<HarnessEvent> {
        self.tx.subscribe()
    }

    async fn login_state(&self) -> HarnessResult<LoginState> {
        self.check_running()?;
        Ok(self.lock().login.clone().unwrap_or(LoginState::SignedOut))
    }

    async fn begin_login(&self) -> HarnessResult<LoginState> {
        self.check_running()?;
        let state = LoginState::SigningIn {
            url: Some("https://example.invalid/demo-login".into()),
            instructions: None,
        };
        self.lock().login = Some(state.clone());
        Ok(state)
    }

    async fn logout(&self) -> HarnessResult<()> {
        self.lock().login = Some(LoginState::SignedOut);
        self.emit(HarnessEvent::Login(LoginState::SignedOut));
        Ok(())
    }

    async fn start_session(&self, _cfg: &SessionConfig) -> HarnessResult<ExternalId> {
        self.check_running()?;
        if !self.lock().login.as_ref().is_some_and(LoginState::is_ready) {
            return Err(HarnessError::SignedOut);
        }
        let mut s = self.lock();
        let id = format!("fake-{}", s.sessions.len() + 1);
        s.sessions.push(id.clone());
        Ok(id)
    }

    async fn resume_session(&self, id: &str, _cfg: &SessionConfig) -> HarnessResult<ExternalId> {
        self.check_running()?;
        let mut s = self.lock();
        if !s.sessions.iter().any(|x| x == id) {
            s.sessions.push(id.to_owned());
        }
        Ok(id.to_owned())
    }

    async fn send(&self, session: &str, text: &str) -> HarnessResult<()> {
        self.check_running()?;
        self.lock().sent.push((session.to_owned(), text.to_owned()));
        let turn = (self.responder)(text);
        let me = self.clone();
        let session = session.to_owned();
        tokio::spawn(async move {
            me.emit(HarnessEvent::TurnStarted {
                session: session.clone(),
            });
            for item in turn.items {
                me.emit(HarnessEvent::ItemCompleted {
                    session: session.clone(),
                    item,
                });
            }
            if let Some(request) = turn.request {
                let (tx, rx) = oneshot::channel();
                let id = {
                    let mut s = me.lock();
                    s.next_request += 1;
                    let id = format!("req-{}", s.next_request);
                    s.pending.insert(id.clone(), tx);
                    id
                };
                me.emit(HarnessEvent::Request(HarnessRequest {
                    id: id.clone(),
                    session: session.clone(),
                    request: request.clone(),
                    reason: None,
                }));
                let decision = rx.await.unwrap_or(Decision::Decline);
                let label = match &request {
                    RequestKind::Command { command, .. } => command.clone(),
                    RequestKind::FileChange { summary } => summary.clone(),
                    RequestKind::Permission { title } => title.clone(),
                };
                let ran = decision != Decision::Decline;
                me.emit(HarnessEvent::ItemCompleted {
                    session: session.clone(),
                    item: TranscriptItem {
                        kind: ItemKind::Command,
                        text: format!("{label} ({})", if ran { "ran" } else { "declined" }),
                    },
                });
            }
            if let Some(message) = turn.fail {
                me.emit(HarnessEvent::TurnFailed { session, message });
                return;
            }
            for word in turn.reply.split_inclusive(' ') {
                me.emit(HarnessEvent::Delta {
                    session: session.clone(),
                    text: word.to_owned(),
                });
            }
            me.emit(HarnessEvent::ItemCompleted {
                session: session.clone(),
                item: TranscriptItem {
                    kind: ItemKind::Message,
                    text: turn.reply.clone(),
                },
            });
            me.emit(HarnessEvent::TurnCompleted {
                session,
                usage: Some(TokenUsage {
                    input: 100,
                    output: turn.reply.len() as u64 / 4,
                }),
            });
        });
        Ok(())
    }

    async fn interrupt(&self, session: &str) -> HarnessResult<()> {
        self.lock().interrupted.push(session.to_owned());
        Ok(())
    }

    async fn compact(&self, session: &str) -> HarnessResult<()> {
        self.lock().compacted.push(session.to_owned());
        self.emit(HarnessEvent::Compacted {
            session: session.to_owned(),
        });
        Ok(())
    }

    async fn answer(&self, request_id: &str, decision: Decision) -> HarnessResult<()> {
        let tx = self
            .lock()
            .pending
            .remove(request_id)
            .ok_or_else(|| HarnessError::Protocol(format!("unknown request {request_id}")))?;
        let _ = tx.send(decision);
        Ok(())
    }

    async fn shutdown(&self) {
        let mut s = self.lock();
        if !s.stopped {
            s.stopped = true;
            s.pending.clear();
            drop(s);
            self.emit(HarnessEvent::Exited { message: None });
        }
    }
}

/// Launcher that hands out one shared [`FakeDriver`] (tests keep a clone to
/// inspect it). Each launch after a shutdown gets a fresh driver sharing
/// the same responder, like a restarted sidecar.
#[derive(Debug, Clone)]
pub struct FakeLauncher {
    current: Arc<Mutex<FakeDriver>>,
    launches: Arc<Mutex<u32>>,
}

impl FakeLauncher {
    pub fn new(driver: FakeDriver) -> Self {
        Self {
            current: Arc::new(Mutex::new(driver)),
            launches: Arc::new(Mutex::new(0)),
        }
    }

    pub fn driver(&self) -> FakeDriver {
        match self.current.lock() {
            Ok(g) => g.clone(),
            Err(p) => p.into_inner().clone(),
        }
    }

    pub fn launches(&self) -> u32 {
        match self.launches.lock() {
            Ok(g) => *g,
            Err(p) => *p.into_inner(),
        }
    }
}

#[async_trait]
impl HarnessLauncher for FakeLauncher {
    fn kind(&self) -> HarnessKind {
        HarnessKind::Fake
    }

    fn installed(&self) -> bool {
        true
    }

    async fn launch(&self, _ctx: &LaunchContext) -> HarnessResult<Arc<dyn HarnessDriver>> {
        let mut current = match self.current.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        if current.is_stopped() {
            let (login, sessions) = {
                let s = current.lock();
                (s.login.clone(), s.sessions.clone())
            };
            let (tx, _) = broadcast::channel(256);
            let responder = current.responder.clone();
            *current = FakeDriver {
                tx,
                state: Arc::new(Mutex::new(State {
                    login,
                    sessions,
                    ..State::default()
                })),
                responder,
            };
        }
        match self.launches.lock() {
            Ok(mut g) => *g += 1,
            Err(p) => *p.into_inner() += 1,
        }
        Ok(Arc::new(current.clone()))
    }
}
