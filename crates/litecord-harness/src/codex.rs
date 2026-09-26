//! Codex `app-server` driver (feature `codex`). See docs/AGENT_HARNESS.md.
//!
//! Speaks newline-delimited JSON-RPC 2.0 (without the `jsonrpc` field, like
//! Codex itself) over the child's stdin/stdout. Method and field names come
//! from the public app-server README and are kept as constants below so they
//! are easy to check against `codex app-server generate-json-schema`.
//! Parsing is tolerant: unknown notifications and odd payloads are ignored.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Map, Value};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::driver::{find_executable, HarnessDriver, HarnessLauncher, LaunchContext};
use crate::types::*;

// ---- Protocol names --------------------------------------------------------

const BINARY: &str = "codex";
const SUBCOMMAND: &str = "app-server";

// Client -> server requests / notifications.
const M_INITIALIZE: &str = "initialize";
const M_INITIALIZED: &str = "initialized";
const M_ACCOUNT_READ: &str = "account/read";
const M_LOGIN_START: &str = "account/login/start";
const M_LOGOUT: &str = "account/logout";
const M_THREAD_START: &str = "thread/start";
const M_THREAD_RESUME: &str = "thread/resume";
const M_TURN_START: &str = "turn/start";
const M_TURN_INTERRUPT: &str = "turn/interrupt";
const M_COMPACT: &str = "thread/compact/start";
const M_MODEL_LIST: &str = "model/list";

// Server -> client notifications.
const N_LOGIN_COMPLETED: &str = "account/login/completed";
const N_ACCOUNT_UPDATED: &str = "account/updated";
const N_TURN_STARTED: &str = "turn/started";
const N_TURN_COMPLETED: &str = "turn/completed";
const N_AGENT_DELTA: &str = "item/agentMessage/delta";
const N_ITEM_COMPLETED: &str = "item/completed";
const N_TOKEN_USAGE: &str = "thread/tokenUsage/updated";
const N_COMPACTED: &str = "thread/compacted";

// Server -> client requests.
const R_COMMAND_APPROVAL: &str = "item/commandExecution/requestApproval";
const R_FILE_APPROVAL: &str = "item/fileChange/requestApproval";

/// Every JSON-RPC method and notification name this driver sends or handles
/// (client requests/notifications, server notifications, server requests).
/// Used by the doctor command and [`schema_check`].
pub const PROTOCOL_METHODS: &[&str] = &[
    M_INITIALIZE,
    M_INITIALIZED,
    M_ACCOUNT_READ,
    M_LOGIN_START,
    M_LOGOUT,
    M_THREAD_START,
    M_THREAD_RESUME,
    M_TURN_START,
    M_TURN_INTERRUPT,
    M_COMPACT,
    M_MODEL_LIST,
    N_LOGIN_COMPLETED,
    N_ACCOUNT_UPDATED,
    N_TURN_STARTED,
    N_TURN_COMPLETED,
    N_AGENT_DELTA,
    N_ITEM_COMPLETED,
    N_TOKEN_USAGE,
    N_COMPACTED,
    R_COMMAND_APPROVAL,
    R_FILE_APPROVAL,
];

// Fields.
const F_CLIENT_INFO: &str = "clientInfo";
const F_ACCOUNT: &str = "account";
const F_TYPE: &str = "type";
const F_PLAN_TYPE: &str = "planType";
const F_AUTH_URL: &str = "authUrl";
const F_SUCCESS: &str = "success";
const F_ERROR: &str = "error";
const F_MESSAGE: &str = "message";
const F_THREAD: &str = "thread";
const F_THREAD_ID: &str = "threadId";
const F_TURN: &str = "turn";
const F_TURN_ID: &str = "turnId";
const F_ID: &str = "id";
const F_CWD: &str = "cwd";
const F_APPROVAL_POLICY: &str = "approvalPolicy";
const F_SANDBOX: &str = "sandbox";
const F_DEV_INSTRUCTIONS: &str = "developerInstructions";
const F_MODEL: &str = "model";
const F_INPUT: &str = "input";
const F_TEXT: &str = "text";
const F_DELTA: &str = "delta";
const F_ITEM: &str = "item";
const F_STATUS: &str = "status";
const F_COMMAND: &str = "command";
const F_CHANGES: &str = "changes";
const F_SERVER: &str = "server";
const F_TOOL: &str = "tool";
const F_ITEMS: &str = "items";
const F_REASON: &str = "reason";
const F_DECISION: &str = "decision";
const F_TOKEN_USAGE: &str = "tokenUsage";
const F_USAGE_LAST: &str = "last";
const F_USAGE_TOTAL: &str = "total";
const F_INPUT_TOKENS: &str = "inputTokens";
const F_OUTPUT_TOKENS: &str = "outputTokens";
const F_API_KEY: &str = "apiKey";
const F_DATA: &str = "data";
const F_MODELS: &str = "models";

// Values.
const ACCOUNT_CHATGPT: &str = "chatgpt";
const ACCOUNT_API_KEY: &str = "apiKey";
const LOGIN_TYPE_CHATGPT: &str = "chatgpt";
const LOGIN_TYPE_API_KEY: &str = "apiKey";
const TURN_FAILED: &str = "failed";
const INPUT_TEXT: &str = "text";
const DECISION_ACCEPT: &str = "accept";
const DECISION_ACCEPT_SESSION: &str = "acceptForSession";
const DECISION_DECLINE: &str = "decline";

// Item types.
const I_AGENT_MESSAGE: &str = "agentMessage";
const I_REASONING: &str = "reasoning";
const I_COMMAND: &str = "commandExecution";
const I_FILE_CHANGE: &str = "fileChange";
const I_MCP_TOOL: &str = "mcpToolCall";
const I_COLLAB: &str = "collabAgentToolCall";
const I_WEB_SEARCH: &str = "webSearch";
const I_COMPACTION: &str = "contextCompaction";
const I_PLAN: &str = "plan";
const I_TODO: &str = "todoList";

// Modes -> (approvalPolicy, sandbox).
const APPROVAL_UNTRUSTED: &str = "untrusted";
const APPROVAL_ON_REQUEST: &str = "on-request";
const SANDBOX_READ_ONLY: &str = "read-only";
const SANDBOX_WORKSPACE_WRITE: &str = "workspace-write";

// Login options offered to the app.
const OPTION_CHATGPT: &str = "chatgpt";
const OPTION_CHATGPT_LABEL: &str = "ChatGPT account";
const OPTION_API_KEY: &str = "api_key";
const OPTION_API_KEY_LABEL: &str = "OpenAI API key";

// CLI (doctor).
const ARG_VERSION: &str = "--version";
const SUBCOMMAND_SCHEMA: &str = "generate-json-schema";
const ARG_OUT: &str = "--out";
const CLI_TIMEOUT: Duration = Duration::from_secs(10);
const SCHEMA_TIMEOUT: Duration = Duration::from_secs(60);

// JSON-RPC.
const ERR_METHOD_NOT_FOUND: i64 = -32601;
const UNSUPPORTED_MESSAGE: &str = "not supported by litecord";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const EVENT_CAPACITY: usize = 512;

fn mode_settings(mode: OmniMode) -> (&'static str, &'static str) {
    match mode {
        OmniMode::Assistant => (APPROVAL_UNTRUSTED, SANDBOX_READ_ONLY),
        OmniMode::Workspace | OmniMode::ComputerUse => {
            (APPROVAL_ON_REQUEST, SANDBOX_WORKSPACE_WRITE)
        }
    }
}

// ---- JSON-RPC peer ---------------------------------------------------------

#[derive(Debug)]
struct RpcError {
    code: i64,
    message: String,
}

type Reply = Result<Value, RpcError>;

/// Outgoing half of a JSON-RPC connection plus the pending-request map.
#[derive(Debug)]
struct RpcPeer {
    out: mpsc::UnboundedSender<String>,
    pending: Mutex<HashMap<u64, oneshot::Sender<Result<Reply, HarnessError>>>>,
    next_id: AtomicU64,
    closed: AtomicBool,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    match m.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    }
}

impl RpcPeer {
    fn new(out: mpsc::UnboundedSender<String>) -> Self {
        Self {
            out,
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            closed: AtomicBool::new(false),
        }
    }

    fn write(&self, msg: &Value) -> HarnessResult<()> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(HarnessError::NotRunning);
        }
        self.out
            .send(msg.to_string())
            .map_err(|_| HarnessError::NotRunning)
    }

    async fn request(&self, method: &str, params: Value) -> HarnessResult<Reply> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        lock(&self.pending).insert(id, tx);
        let msg = json!({ "id": id, "method": method, "params": params });
        if let Err(e) = self.write(&msg) {
            lock(&self.pending).remove(&id);
            return Err(e);
        }
        match tokio::time::timeout(REQUEST_TIMEOUT, rx).await {
            Ok(Ok(r)) => r,
            Ok(Err(_)) => Err(HarnessError::NotRunning),
            Err(_) => {
                lock(&self.pending).remove(&id);
                Err(HarnessError::Timeout)
            }
        }
    }

    /// A request whose JSON-RPC error becomes `HarnessError::Harness`.
    async fn call(&self, method: &str, params: Value) -> HarnessResult<Value> {
        self.request(method, params)
            .await?
            .map_err(|e| HarnessError::Harness(e.message))
    }

    fn notify(&self, method: &str, params: Option<Value>) -> HarnessResult<()> {
        let msg = match params {
            Some(p) => json!({ "method": method, "params": p }),
            None => json!({ "method": method }),
        };
        self.write(&msg)
    }

    fn respond(&self, id: Value, result: Value) -> HarnessResult<()> {
        self.write(&json!({ "id": id, "result": result }))
    }

    fn respond_error(&self, id: Value, code: i64, message: &str) -> HarnessResult<()> {
        self.write(&json!({ "id": id, "error": { "code": code, "message": message } }))
    }

    fn on_response(&self, msg: &Map<String, Value>) {
        let Some(id) = msg.get(F_ID).and_then(Value::as_u64) else {
            return;
        };
        let Some(tx) = lock(&self.pending).remove(&id) else {
            return;
        };
        let reply = match msg.get(F_ERROR) {
            Some(err) if !err.is_null() => Err(RpcError {
                code: err.get("code").and_then(Value::as_i64).unwrap_or(0),
                message: err
                    .get(F_MESSAGE)
                    .and_then(Value::as_str)
                    .unwrap_or("error")
                    .to_owned(),
            }),
            _ => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
        };
        let _ = tx.send(Ok(reply));
    }

    fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        let pending: Vec<_> = lock(&self.pending).drain().collect();
        for (_, tx) in pending {
            let _ = tx.send(Err(HarnessError::NotRunning));
        }
    }
}

// ---- Driver ----------------------------------------------------------------

#[derive(Default)]
struct State {
    /// thread id -> active turn id.
    active_turn: HashMap<String, String>,
    /// thread id -> last reported usage.
    usage: HashMap<String, TokenUsage>,
    /// driver-scoped request id -> (JSON-RPC id, thread id).
    requests: HashMap<String, (Value, String)>,
    next_request: u64,
}

struct Inner {
    peer: RpcPeer,
    tx: broadcast::Sender<HarnessEvent>,
    state: Mutex<State>,
    exited: AtomicBool,
    child: Mutex<Option<Child>>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        for t in lock(&self.tasks).drain(..) {
            t.abort();
        }
    }
}

/// A running Codex app-server.
#[derive(Clone)]
pub struct CodexDriver {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for CodexDriver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CodexDriver")
    }
}

impl CodexDriver {
    /// Performs the handshake over any stream pair (the child's stdio, or a
    /// duplex pipe in tests).
    pub async fn connect<R, W>(reader: R, writer: W) -> HarnessResult<Self>
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        Self::connect_with(reader, writer, None).await
    }

    async fn connect_with<R, W>(reader: R, writer: W, child: Option<Child>) -> HarnessResult<Self>
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let (out_tx, out_rx) = mpsc::unbounded_channel::<String>();
        let (tx, _) = broadcast::channel(EVENT_CAPACITY);
        let inner = Arc::new(Inner {
            peer: RpcPeer::new(out_tx),
            tx,
            state: Mutex::new(State::default()),
            exited: AtomicBool::new(false),
            child: Mutex::new(child),
            tasks: Mutex::new(Vec::new()),
        });
        let writer_task = tokio::spawn(write_loop(writer, out_rx));
        let reader_task = tokio::spawn(read_loop(reader, Arc::downgrade(&inner)));
        lock(&inner.tasks).extend([writer_task, reader_task]);
        let driver = Self { inner };
        if let Err(e) = driver.handshake().await {
            driver.shutdown().await;
            return Err(e);
        }
        Ok(driver)
    }

    async fn handshake(&self) -> HarnessResult<()> {
        let params = json!({
            F_CLIENT_INFO: {
                "name": "litecord",
                "title": "Litecord",
                "version": env!("CARGO_PKG_VERSION"),
            }
        });
        self.inner.peer.call(M_INITIALIZE, params).await?;
        self.inner.peer.notify(M_INITIALIZED, None)
    }

    fn add_task(&self, t: JoinHandle<()>) {
        let mut tasks = lock(&self.inner.tasks);
        tasks.retain(|t| !t.is_finished());
        tasks.push(t);
    }

    fn thread_params(cfg: &SessionConfig) -> Map<String, Value> {
        let (approval, sandbox) = mode_settings(cfg.mode);
        let mut p = Map::new();
        p.insert(F_CWD.into(), json!(cfg.cwd.to_string_lossy()));
        p.insert(F_APPROVAL_POLICY.into(), json!(approval));
        p.insert(F_SANDBOX.into(), json!(sandbox));
        p.insert(F_DEV_INSTRUCTIONS.into(), json!(cfg.instructions));
        if let Some(model) = &cfg.model {
            p.insert(F_MODEL.into(), json!(model));
        }
        p
    }
}

impl Inner {
    fn emit(&self, e: HarnessEvent) {
        let _ = self.tx.send(e);
    }

    fn state(&self) -> MutexGuard<'_, State> {
        lock(&self.state)
    }

    fn on_closed(&self, message: Option<String>) {
        self.peer.close();
        let ids: Vec<String> = self.state().requests.drain().map(|(k, _)| k).collect();
        for id in ids {
            self.emit(HarnessEvent::RequestResolved { id });
        }
        if !self.exited.swap(true, Ordering::SeqCst) {
            self.emit(HarnessEvent::Exited { message });
        }
    }

    async fn read_login(&self) -> HarnessResult<LoginState> {
        let result = self.peer.call(M_ACCOUNT_READ, json!({})).await?;
        Ok(login_from_account(result.get(F_ACCOUNT)))
    }

    fn on_message(self: &Arc<Self>, msg: Map<String, Value>) {
        let method = msg.get("method").and_then(Value::as_str).map(str::to_owned);
        match (method, msg.get(F_ID).cloned()) {
            (Some(method), Some(id)) if !id.is_null() => {
                let params = msg.get("params").cloned().unwrap_or(Value::Null);
                self.on_server_request(&method, id, &params);
            }
            (Some(method), _) => {
                let params = msg.get("params").cloned().unwrap_or(Value::Null);
                self.on_notification(&method, &params);
            }
            (None, _) => self.peer.on_response(&msg),
        }
    }

    fn on_server_request(&self, method: &str, id: Value, params: &Value) {
        let request = match method {
            R_COMMAND_APPROVAL => RequestKind::Command {
                command: command_text(params.get(F_COMMAND)).unwrap_or_default(),
                cwd: str_field(params, F_CWD),
            },
            R_FILE_APPROVAL => RequestKind::FileChange {
                summary: str_field(params, F_REASON).unwrap_or_else(|| "file changes".into()),
            },
            _ => {
                tracing::debug!(method, "codex: declining unsupported server request");
                let _ = self
                    .peer
                    .respond_error(id, ERR_METHOD_NOT_FOUND, UNSUPPORTED_MESSAGE);
                return;
            }
        };
        let session = thread_id(params).unwrap_or_default();
        let key = {
            let mut s = self.state();
            s.next_request += 1;
            let key = format!("codex-{}", s.next_request);
            s.requests.insert(key.clone(), (id, session.clone()));
            key
        };
        self.emit(HarnessEvent::Request(HarnessRequest {
            id: key,
            session,
            request,
            reason: str_field(params, F_REASON),
        }));
    }

    fn on_notification(self: &Arc<Self>, method: &str, params: &Value) {
        match method {
            N_LOGIN_COMPLETED => {
                if params.get(F_SUCCESS).and_then(Value::as_bool) == Some(false) {
                    let message = params
                        .get(F_ERROR)
                        .and_then(|e| {
                            e.as_str()
                                .map(str::to_owned)
                                .or_else(|| str_field(e, F_MESSAGE))
                        })
                        .unwrap_or_else(|| "sign-in failed".into());
                    self.emit(HarnessEvent::Login(LoginState::Error { message }));
                } else {
                    self.spawn_login_refresh(true);
                }
            }
            N_ACCOUNT_UPDATED => self.spawn_login_refresh(false),
            _ => self.on_thread_notification(method, params),
        }
    }

    /// Re-reads the account from a separate task (the reader must keep
    /// running to receive the answer).
    fn spawn_login_refresh(self: &Arc<Self>, after_login: bool) {
        let me = Arc::clone(self);
        let task = tokio::spawn(async move {
            let state = match me.read_login().await {
                Ok(s) => s,
                Err(_) if after_login => LoginState::Ready { account: None },
                Err(_) => return,
            };
            me.emit(HarnessEvent::Login(state));
        });
        let mut tasks = lock(&self.tasks);
        tasks.retain(|t| !t.is_finished());
        tasks.push(task);
    }

    fn on_thread_notification(&self, method: &str, params: &Value) {
        let Some(session) = thread_id(params) else {
            return;
        };
        match method {
            N_TURN_STARTED => {
                if let Some(turn) = params.get(F_TURN).and_then(|t| str_field(t, F_ID)) {
                    self.state().active_turn.insert(session.clone(), turn);
                }
                self.emit(HarnessEvent::TurnStarted { session });
            }
            N_AGENT_DELTA => {
                if let Some(text) = str_field(params, F_DELTA) {
                    self.emit(HarnessEvent::Delta { session, text });
                }
            }
            N_ITEM_COMPLETED => {
                if let Some(item) = params.get(F_ITEM).and_then(transcript_item) {
                    self.emit(HarnessEvent::ItemCompleted { session, item });
                }
            }
            N_TOKEN_USAGE => {
                if let Some(usage) = parse_usage(params) {
                    self.state().usage.insert(session, usage);
                }
            }
            N_TURN_COMPLETED => {
                let turn = params.get(F_TURN).cloned().unwrap_or(Value::Null);
                let usage = {
                    let mut s = self.state();
                    s.active_turn.remove(&session);
                    s.usage.get(&session).copied()
                };
                if str_field(&turn, F_STATUS).as_deref() == Some(TURN_FAILED) {
                    let message = turn
                        .get(F_ERROR)
                        .and_then(|e| str_field(e, F_MESSAGE))
                        .unwrap_or_else(|| "turn failed".into());
                    self.emit(HarnessEvent::TurnFailed { session, message });
                } else {
                    self.emit(HarnessEvent::TurnCompleted { session, usage });
                }
            }
            N_COMPACTED => self.emit(HarnessEvent::Compacted { session }),
            _ => {}
        }
    }
}

async fn write_loop<W: AsyncWrite + Unpin>(mut w: W, mut rx: mpsc::UnboundedReceiver<String>) {
    while let Some(mut line) = rx.recv().await {
        line.push('\n');
        if w.write_all(line.as_bytes()).await.is_err() || w.flush().await.is_err() {
            tracing::debug!("codex: stdin closed");
            break;
        }
    }
}

async fn read_loop<R: AsyncRead + Unpin>(r: R, inner: Weak<Inner>) {
    let mut lines = BufReader::new(r).lines();
    let message = loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let Some(inner) = inner.upgrade() else {
                    return;
                };
                match serde_json::from_str::<Value>(line) {
                    Ok(Value::Object(msg)) => inner.on_message(msg),
                    _ => tracing::debug!(len = line.len(), "codex: ignoring non-JSON-RPC line"),
                }
            }
            Ok(None) => break "codex exited".to_owned(),
            Err(e) => break format!("codex output unreadable: {e}"),
        }
    };
    if let Some(inner) = inner.upgrade() {
        inner.on_closed(Some(message));
    }
}

// ---- Payload helpers -------------------------------------------------------

fn str_field(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn thread_id(params: &Value) -> Option<String> {
    str_field(params, F_THREAD_ID)
        .or_else(|| params.get(F_TURN).and_then(|t| str_field(t, F_THREAD_ID)))
        .or_else(|| params.get(F_THREAD).and_then(|t| str_field(t, F_ID)))
}

fn thread_id_from_result(result: &Value) -> Option<String> {
    result
        .get(F_THREAD)
        .and_then(|t| str_field(t, F_ID))
        .or_else(|| str_field(result, F_THREAD_ID))
        .or_else(|| str_field(result, F_ID))
}

fn command_text(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::String(s) => Some(s.clone()),
        Value::Array(parts) => Some(
            parts
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" "),
        ),
        _ => None,
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

fn login_from_account(account: Option<&Value>) -> LoginState {
    let Some(account) = account.filter(|a| !a.is_null()) else {
        return LoginState::SignedOut;
    };
    let label = match account.get(F_TYPE).and_then(Value::as_str) {
        Some(ACCOUNT_CHATGPT) => match str_field(account, F_PLAN_TYPE) {
            Some(plan) if !plan.is_empty() => format!("ChatGPT {}", capitalize(&plan)),
            _ => "ChatGPT".to_owned(),
        },
        Some(ACCOUNT_API_KEY) => "API key".to_owned(),
        _ => return LoginState::Ready { account: None },
    };
    LoginState::Ready {
        account: Some(label),
    }
}

fn parse_usage(params: &Value) -> Option<TokenUsage> {
    let usage = params.get(F_TOKEN_USAGE).unwrap_or(params);
    let pick = |v: &Value| -> Option<TokenUsage> {
        let input = v.get(F_INPUT_TOKENS).and_then(Value::as_u64);
        let output = v.get(F_OUTPUT_TOKENS).and_then(Value::as_u64);
        (input.is_some() || output.is_some()).then(|| TokenUsage {
            input: input.unwrap_or(0),
            output: output.unwrap_or(0),
        })
    };
    usage
        .get(F_USAGE_LAST)
        .and_then(pick)
        .or_else(|| usage.get(F_USAGE_TOTAL).and_then(pick))
        .or_else(|| pick(usage))
}

/// Maps a completed item to a content-light transcript item. Reasoning,
/// tool arguments and tool results are never kept.
fn transcript_item(item: &Value) -> Option<TranscriptItem> {
    let (kind, text) = match item.get(F_TYPE).and_then(Value::as_str)? {
        I_AGENT_MESSAGE => (
            ItemKind::Message,
            str_field(item, F_TEXT).unwrap_or_default(),
        ),
        I_REASONING => return None,
        I_COMMAND => {
            let mut text = command_text(item.get(F_COMMAND)).unwrap_or_else(|| "command".into());
            if let Some(status) = str_field(item, F_STATUS) {
                text = format!("{text} ({status})");
            }
            (ItemKind::Command, text)
        }
        I_FILE_CHANGE => {
            let text = match item.get(F_CHANGES).and_then(Value::as_array) {
                Some(c) if c.len() == 1 => "1 file changed".to_owned(),
                Some(c) => format!("{} files changed", c.len()),
                None => "files changed".to_owned(),
            };
            (ItemKind::FileChange, text)
        }
        I_MCP_TOOL => {
            let server = str_field(item, F_SERVER).unwrap_or_else(|| "mcp".into());
            let tool = str_field(item, F_TOOL).unwrap_or_else(|| "tool".into());
            (ItemKind::ToolCall, format!("{server}.{tool}"))
        }
        I_COLLAB => {
            let text = match str_field(item, F_TOOL) {
                Some(tool) => format!("subagent: {tool}"),
                None => "subagent".to_owned(),
            };
            (ItemKind::Subagent, text)
        }
        I_WEB_SEARCH => (ItemKind::ToolCall, "web search".to_owned()),
        I_COMPACTION => (ItemKind::Compaction, "Context compacted".to_owned()),
        I_PLAN | I_TODO => {
            let text = match item.get(F_ITEMS).and_then(Value::as_array) {
                Some(steps) => format!("Plan: {} steps", steps.len()),
                None => "Plan updated".to_owned(),
            };
            (ItemKind::Plan, text)
        }
        _ => return None,
    };
    Some(TranscriptItem { kind, text })
}

/// Model names from a `model/list` result. Accepts `{"data":[..]}`,
/// `{"models":[..]}` or a bare array, whose entries are strings or objects
/// with `id` / `model`.
fn parse_models(result: &Value) -> Vec<String> {
    let list = result
        .get(F_DATA)
        .and_then(Value::as_array)
        .or_else(|| result.get(F_MODELS).and_then(Value::as_array))
        .or_else(|| result.as_array());
    let Some(list) = list else {
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::new();
    for entry in list {
        let name = match entry {
            Value::String(s) => Some(s.clone()),
            Value::Object(_) => str_field(entry, F_ID).or_else(|| str_field(entry, F_MODEL)),
            _ => None,
        };
        if let Some(name) = name.filter(|n| !n.is_empty()) {
            if !out.contains(&name) {
                out.push(name);
            }
        }
    }
    out
}

/// Removes every occurrence of `secret` from `text`.
fn redact(text: &str, secret: &str) -> String {
    if secret.is_empty() {
        text.to_owned()
    } else {
        text.replace(secret, "[redacted]")
    }
}

// ---- Trait impls -----------------------------------------------------------

#[async_trait]
impl HarnessDriver for CodexDriver {
    fn kind(&self) -> HarnessKind {
        HarnessKind::Codex
    }

    fn subscribe(&self) -> broadcast::Receiver<HarnessEvent> {
        self.inner.tx.subscribe()
    }

    async fn login_state(&self) -> HarnessResult<LoginState> {
        self.inner.read_login().await
    }

    async fn begin_login(&self) -> HarnessResult<LoginState> {
        let result = self
            .inner
            .peer
            .call(M_LOGIN_START, json!({ F_TYPE: LOGIN_TYPE_CHATGPT }))
            .await?;
        let url = str_field(&result, F_AUTH_URL)
            .ok_or_else(|| HarnessError::Protocol("login/start returned no authUrl".into()))?;
        Ok(LoginState::SigningIn {
            url: Some(url),
            instructions: None,
            needs_code: false,
        })
    }

    async fn login_options(&self) -> HarnessResult<Vec<LoginOption>> {
        Ok(vec![
            LoginOption {
                id: OPTION_CHATGPT.into(),
                label: OPTION_CHATGPT_LABEL.into(),
                kind: LoginKind::Browser,
            },
            LoginOption {
                id: OPTION_API_KEY.into(),
                label: OPTION_API_KEY_LABEL.into(),
                kind: LoginKind::ApiKey,
            },
        ])
    }

    async fn begin_login_with(&self, option: &str) -> HarnessResult<LoginState> {
        match option {
            OPTION_CHATGPT => self.begin_login().await,
            _ => Err(HarnessError::Unsupported("this sign-in method")),
        }
    }

    async fn login_api_key(&self, _option: &str, key: &str) -> HarnessResult<()> {
        // The key only ever lives in this request body; it is never logged,
        // stored, or echoed back in errors.
        let params = json!({ F_TYPE: LOGIN_TYPE_API_KEY, F_API_KEY: key });
        match self.inner.peer.request(M_LOGIN_START, params).await? {
            Ok(_) => {}
            Err(e) => return Err(HarnessError::Harness(redact(&e.message, key))),
        }
        let state = self
            .inner
            .read_login()
            .await
            .unwrap_or(LoginState::Ready { account: None });
        self.inner.emit(HarnessEvent::Login(state));
        Ok(())
    }

    async fn models(&self) -> HarnessResult<Vec<String>> {
        match self.inner.peer.request(M_MODEL_LIST, json!({})).await {
            Ok(Ok(result)) => Ok(parse_models(&result)),
            Ok(Err(e)) => {
                tracing::debug!(code = e.code, "codex: model/list failed: {}", e.message);
                Ok(Vec::new())
            }
            Err(e) => {
                tracing::debug!("codex: model/list failed: {e}");
                Ok(Vec::new())
            }
        }
    }

    async fn logout(&self) -> HarnessResult<()> {
        self.inner.peer.call(M_LOGOUT, json!({})).await?;
        self.inner.emit(HarnessEvent::Login(LoginState::SignedOut));
        Ok(())
    }

    async fn start_session(&self, cfg: &SessionConfig) -> HarnessResult<ExternalId> {
        let params = Value::Object(Self::thread_params(cfg));
        let result = self.inner.peer.call(M_THREAD_START, params).await?;
        thread_id_from_result(&result)
            .ok_or_else(|| HarnessError::Protocol("thread/start returned no thread id".into()))
    }

    async fn resume_session(&self, id: &str, cfg: &SessionConfig) -> HarnessResult<ExternalId> {
        let mut params = Self::thread_params(cfg);
        params.insert(F_THREAD_ID.into(), json!(id));
        match self
            .inner
            .peer
            .request(M_THREAD_RESUME, Value::Object(params))
            .await?
        {
            Ok(result) => Ok(thread_id_from_result(&result).unwrap_or_else(|| id.to_owned())),
            Err(e) if e.code == ERR_METHOD_NOT_FOUND => {
                Err(HarnessError::Unsupported("thread resume"))
            }
            Err(e) => Err(HarnessError::Harness(e.message)),
        }
    }

    async fn send(&self, session: &str, text: &str) -> HarnessResult<()> {
        let params = json!({
            F_THREAD_ID: session,
            F_INPUT: [{ F_TYPE: INPUT_TEXT, F_TEXT: text }],
        });
        let result = self.inner.peer.call(M_TURN_START, params).await?;
        if let Some(turn) = result.get(F_TURN).and_then(|t| str_field(t, F_ID)) {
            self.inner
                .state()
                .active_turn
                .insert(session.to_owned(), turn);
        }
        Ok(())
    }

    async fn interrupt(&self, session: &str) -> HarnessResult<()> {
        let turn = self.inner.state().active_turn.get(session).cloned();
        let Some(turn) = turn else {
            return Ok(());
        };
        self.inner
            .peer
            .call(
                M_TURN_INTERRUPT,
                json!({ F_THREAD_ID: session, F_TURN_ID: turn }),
            )
            .await?;
        Ok(())
    }

    async fn compact(&self, session: &str) -> HarnessResult<()> {
        match self
            .inner
            .peer
            .request(M_COMPACT, json!({ F_THREAD_ID: session }))
            .await?
        {
            Ok(_) => Ok(()),
            Err(e) if e.code == ERR_METHOD_NOT_FOUND => {
                Err(HarnessError::Unsupported("compaction"))
            }
            Err(e) => Err(HarnessError::Harness(e.message)),
        }
    }

    async fn answer(&self, request_id: &str, decision: Decision) -> HarnessResult<()> {
        let (rpc_id, _) = self
            .inner
            .state()
            .requests
            .remove(request_id)
            .ok_or_else(|| HarnessError::Protocol(format!("unknown request {request_id}")))?;
        let decision = match decision {
            Decision::Accept => DECISION_ACCEPT,
            Decision::AcceptForSession => DECISION_ACCEPT_SESSION,
            Decision::Decline => DECISION_DECLINE,
        };
        self.inner
            .peer
            .respond(rpc_id, json!({ F_DECISION: decision }))
    }

    async fn shutdown(&self) {
        if let Some(mut child) = lock(&self.inner.child).take() {
            let _ = child.start_kill();
        }
        for t in lock(&self.inner.tasks).drain(..) {
            t.abort();
        }
        self.inner.on_closed(None);
    }
}

/// Starts `codex app-server` with Litecord's MCP server registered.
#[derive(Debug, Clone, Default)]
pub struct CodexLauncher {
    /// Explicit binary path; `None` searches `PATH`.
    pub path: Option<PathBuf>,
}

/// TOML basic string (double-quoted, escaped).
fn toml_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if c.is_control() => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn toml_array(items: &[String]) -> String {
    let parts: Vec<String> = items.iter().map(|s| toml_string(s)).collect();
    format!("[{}]", parts.join(", "))
}

/// Command-line arguments for the sidecar.
pub fn launch_args(mcp: &McpLaunch) -> Vec<String> {
    // `-c` is a root-level option that Codex applies to every subcommand,
    // so it goes before `app-server`.
    vec![
        "-c".to_owned(),
        format!(
            "mcp_servers.litecord.command={}",
            toml_string(&mcp.command.to_string_lossy())
        ),
        "-c".to_owned(),
        format!("mcp_servers.litecord.args={}", toml_array(&mcp.args)),
        SUBCOMMAND.to_owned(),
    ]
}

#[async_trait]
impl HarnessLauncher for CodexLauncher {
    fn kind(&self) -> HarnessKind {
        HarnessKind::Codex
    }

    fn installed(&self) -> bool {
        find_executable(BINARY, self.path.as_deref()).is_some()
    }

    async fn launch(&self, ctx: &LaunchContext) -> HarnessResult<Arc<dyn HarnessDriver>> {
        let exe = find_executable(BINARY, self.path.as_deref())
            .ok_or(HarnessError::NotInstalled("Codex"))?;
        let mut child = Command::new(exe)
            .args(launch_args(&ctx.mcp))
            .env_clear()
            .envs(crate::env::child_env())
            .current_dir(&ctx.workspace)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| HarnessError::Harness(format!("could not start codex: {e}")))?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            return Err(HarnessError::Protocol("codex stdio unavailable".into()));
        };
        let stderr = child.stderr.take();
        let driver = CodexDriver::connect_with(stdout, stdin, Some(child)).await?;
        if let Some(stderr) = stderr {
            driver.add_task(tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    tracing::debug!(target: "litecord_harness::codex::stderr", "{line}");
                }
            }));
        }
        Ok(Arc::new(driver))
    }
}

// ---- Doctor ----------------------------------------------------------------

/// `codex --version`, trimmed. `None` when not installed or it fails.
pub async fn version(path: Option<&Path>) -> Option<String> {
    let exe = find_executable(BINARY, path)?;
    let run = Command::new(exe)
        .arg(ARG_VERSION)
        .env_clear()
        .envs(crate::env::child_env())
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    let out = tokio::time::timeout(CLI_TIMEOUT, run).await.ok()?.ok()?;
    if !out.status.success() {
        return None;
    }
    let v = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    (!v.is_empty()).then_some(v)
}

/// Entries of [`PROTOCOL_METHODS`] that do not appear (as quoted strings)
/// anywhere in `schema_text`.
fn missing_methods(schema_text: &str) -> Vec<&'static str> {
    PROTOCOL_METHODS
        .iter()
        .copied()
        .filter(|m| !schema_text.contains(&format!("\"{m}\"")))
        .collect()
}

fn unique_temp_dir() -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    std::env::temp_dir().join(format!(
        "litecord-codex-schema-{}-{nanos}-{n}",
        std::process::id()
    ))
}

/// Concatenates every readable text file under `dir` (recursively).
fn read_all_text(dir: &Path) -> String {
    let mut text = String::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
            } else if let Ok(s) = std::fs::read_to_string(&p) {
                text.push_str(&s);
                text.push('\n');
            }
        }
    }
    text
}

/// Generates Codex's JSON schema and returns the [`PROTOCOL_METHODS`] it
/// does not mention.
pub async fn schema_check(path: Option<&Path>) -> Result<Vec<&'static str>, HarnessError> {
    let exe = find_executable(BINARY, path).ok_or(HarnessError::NotInstalled("Codex"))?;
    let dir = unique_temp_dir();
    std::fs::create_dir_all(&dir)
        .map_err(|e| HarnessError::Harness(format!("could not create temp dir: {e}")))?;
    let run = Command::new(exe)
        .args([SUBCOMMAND, SUBCOMMAND_SCHEMA, ARG_OUT])
        .arg(&dir)
        .env_clear()
        .envs(crate::env::child_env())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .status();
    let ok = matches!(
        tokio::time::timeout(SCHEMA_TIMEOUT, run).await,
        Ok(Ok(status)) if status.success()
    );
    let result = if ok {
        let d = dir.clone();
        match tokio::task::spawn_blocking(move || read_all_text(&d)).await {
            Ok(text) => Ok(missing_methods(&text)),
            Err(_) => Err(HarnessError::Unsupported("schema generation")),
        }
    } else {
        Err(HarnessError::Unsupported("schema generation"))
    };
    let _ = std::fs::remove_dir_all(&dir);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toml_escaping() {
        assert_eq!(toml_string(r#"C:\a "b""#), r#""C:\\a \"b\"""#);
        assert_eq!(
            toml_array(&["mcp".into(), "--x".into()]),
            r#"["mcp", "--x"]"#
        );
    }

    #[test]
    fn model_list_shapes() {
        assert_eq!(
            parse_models(&json!({"data":[{"id":"gpt-5"},{"model":"o3"},{"x":1}]})),
            ["gpt-5", "o3"]
        );
        assert_eq!(
            parse_models(&json!({"models":["a", {"id":"b"}, 3]})),
            ["a", "b"]
        );
        assert_eq!(parse_models(&json!(["a", "a", "c"])), ["a", "c"]);
        assert!(parse_models(&json!({"weird": true})).is_empty());
        assert!(parse_models(&Value::Null).is_empty());
    }

    #[test]
    fn schema_missing_methods() {
        let all: String = PROTOCOL_METHODS
            .iter()
            .map(|m| format!("{{\"const\": \"{m}\"}}\n"))
            .collect();
        assert!(missing_methods(&all).is_empty());
        let partial = all.replace("\"model/list\"", "\"model/listing\"");
        assert_eq!(missing_methods(&partial), ["model/list"]);
        // Unquoted mentions do not count.
        assert_eq!(missing_methods("initialize").len(), PROTOCOL_METHODS.len());
    }

    #[test]
    fn redaction() {
        assert_eq!(
            redact("bad key sk-1 here", "sk-1"),
            "bad key [redacted] here"
        );
        assert_eq!(redact("x", ""), "x");
    }

    #[test]
    fn account_labels() {
        assert_eq!(
            login_from_account(Some(&Value::Null)),
            LoginState::SignedOut
        );
        assert_eq!(
            login_from_account(Some(
                &json!({"type":"chatgpt","email":"a@b.c","planType":"plus"})
            )),
            LoginState::Ready {
                account: Some("ChatGPT Plus".into())
            }
        );
        assert_eq!(
            login_from_account(Some(&json!({"type":"apiKey"}))),
            LoginState::Ready {
                account: Some("API key".into())
            }
        );
    }
}
