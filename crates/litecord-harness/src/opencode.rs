//! OpenCode `serve` driver (feature `opencode`). See docs/AGENT_HARNESS.md.
//!
//! The sidecar is `opencode serve` bound to `127.0.0.1` on a random port
//! with a per-launch password (HTTP basic auth, user `opencode`). Requests
//! are plain REST calls; everything the harness does arrives on one
//! long-lived SSE stream (`GET /event`). Payload shapes follow the public
//! server docs and are parsed tolerantly: unknown events and fields are
//! ignored, and nothing here panics on unexpected input.
//!
//! Tracing records ids, kinds and lengths only, never prompt or reply text.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{broadcast, oneshot};
use tokio::task::JoinHandle;

use crate::driver::{find_executable, HarnessDriver, HarnessLauncher, LaunchContext};
use crate::types::*;

// ---------------------------------------------------------------------------
// Protocol constants (checked against the server's OpenAPI at `GET /doc`).
// ---------------------------------------------------------------------------

const BINARY: &str = "opencode";
const AUTH_USER: &str = "opencode";
const ENV_PASSWORD: &str = "OPENCODE_SERVER_PASSWORD";
const ENV_CONFIG: &str = "OPENCODE_CONFIG_CONTENT";

const PATH_HEALTH: &str = "/global/health";
const PATH_EVENT: &str = "/event";
const PATH_SESSION: &str = "/session";
const PATH_CONFIG: &str = "/config";
const PATH_PROVIDER: &str = "/provider";
const PATH_PROVIDER_AUTH: &str = "/provider/auth";
const PATH_CONFIG_PROVIDERS: &str = "/config/providers";
/// Credential store: `PUT /auth/:id`.
const PATH_AUTH: &str = "/auth";
/// OpenAPI document.
const PATH_DOC: &str = "/doc";
/// Suffixes under `/session/:id`.
const SUFFIX_PROMPT: &str = "prompt_async";
const SUFFIX_ABORT: &str = "abort";
const SUFFIX_SUMMARIZE: &str = "summarize";
const SUFFIX_PERMISSIONS: &str = "permissions";
/// Suffixes under `/provider/:id/oauth`.
const SUFFIX_OAUTH_AUTHORIZE: &str = "oauth/authorize";
const SUFFIX_OAUTH_CALLBACK: &str = "oauth/callback";

const EV_PART_UPDATED: &str = "message.part.updated";
const EV_MESSAGE_UPDATED: &str = "message.updated";
const EV_SESSION_STATUS: &str = "session.status";
const EV_SESSION_IDLE: &str = "session.idle";
const EV_SESSION_ERROR: &str = "session.error";
const EV_SESSION_COMPACTED: &str = "session.compacted";
const EV_PERMISSION_UPDATED: &str = "permission.updated";
const EV_PERMISSION_ASKED: &str = "permission.asked";
const EV_PERMISSION_REPLIED: &str = "permission.replied";

const AGENT_ASSISTANT: &str = "omni";
const AGENT_WORKSPACE: &str = "omni-workspace";
const SESSION_TITLE: &str = "Omni";
const LITECORD_TOOL_PREFIX: &str = "litecord_";
const SUBAGENT_TOOL: &str = "task";

/// Sign-in provider preference; any provider with an OAuth method follows.
const LOGIN_PREFERENCE: &[&str] = &["openai", "anthropic", "github-copilot"];
const METHOD_OAUTH: &str = "oauth";
const METHOD_API: &str = "api";
/// `oauth/authorize` response `method` for a pasted-code flow.
const AUTHORIZE_CODE: &str = "code";
/// Separator in login option ids (`<provider>:<method index>`).
const OPTION_SEP: char = ':';
const OPTION_LABEL_SEP: &str = " \u{b7} ";
const DEFAULT_OAUTH_LABEL: &str = "Sign in with browser";
const DEFAULT_API_LABEL: &str = "API key";
const NO_OAUTH: &str = "sign in with `opencode auth login` in a terminal";

/// Every HTTP path the driver uses, in OpenAPI style. Checked against the
/// server's `GET /doc` by [`missing_paths`].
pub const PROTOCOL_PATHS: &[&str] = &[
    "/global/health",
    "/event",
    "/session",
    "/session/{id}",
    "/session/{id}/prompt_async",
    "/session/{id}/abort",
    "/session/{id}/summarize",
    "/session/{id}/permissions/{permissionID}",
    "/config",
    "/config/providers",
    "/provider",
    "/provider/auth",
    "/provider/{id}/oauth/authorize",
    "/provider/{id}/oauth/callback",
    "/auth/{id}",
];

const VERSION_TIMEOUT: Duration = Duration::from_secs(10);

const HEALTH_POLL: Duration = Duration::from_millis(200);
const HEALTH_DEADLINE: Duration = Duration::from_secs(20);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const SUMMARIZE_TIMEOUT: Duration = Duration::from_secs(600);
const BACKOFF_MIN: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(10);
const SHUTDOWN_WAIT: Duration = Duration::from_secs(5);
/// A single SSE line longer than this is dropped rather than buffered.
const SSE_MAX_LINE: usize = 16 * 1024 * 1024;

// ---------------------------------------------------------------------------
// SSE parsing
// ---------------------------------------------------------------------------

/// Minimal incremental `text/event-stream` parser. Feed raw bytes; get back
/// the `data` payload of each completed event (multi-line data joined by
/// `\n`). Only the `data` field is used; comments and other fields are
/// ignored.
#[derive(Debug, Default)]
pub struct SseParser {
    buf: Vec<u8>,
    data: Vec<String>,
    has_data: bool,
}

impl SseParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds a chunk; returns the data of every event it completed.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<String> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        let mut start = 0;
        while let Some(pos) = self.buf[start..].iter().position(|&b| b == b'\n') {
            let end = start + pos;
            let mut line = &self.buf[start..end];
            if let Some(stripped) = line.strip_suffix(b"\r") {
                line = stripped;
            }
            let line = String::from_utf8_lossy(line).into_owned();
            self.line(&line, &mut out);
            start = end + 1;
        }
        self.buf.drain(..start);
        if self.buf.len() > SSE_MAX_LINE {
            self.buf.clear();
        }
        out
    }

    fn line(&mut self, line: &str, out: &mut Vec<String>) {
        if line.is_empty() {
            if self.has_data {
                out.push(self.data.join("\n"));
            }
            self.data.clear();
            self.has_data = false;
            return;
        }
        if line.starts_with(':') {
            return;
        }
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line, ""),
        };
        if field == "data" {
            self.data.push(value.to_owned());
            self.has_data = true;
        }
    }
}

// ---------------------------------------------------------------------------
// Driver state
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
struct TextPart {
    id: String,
    message: String,
    text: String,
}

/// Per-turn bookkeeping for one session.
#[derive(Debug, Default)]
struct Turn {
    /// `TurnStarted` was emitted.
    active: bool,
    /// A prompt was sent and its turn has not finished.
    awaiting: bool,
    last_prompt: Option<String>,
    roles: HashMap<String, String>,
    parts: Vec<TextPart>,
    usage: Option<TokenUsage>,
    tools_done: HashSet<String>,
}

#[derive(Debug)]
struct Session {
    agent: &'static str,
    system: String,
    model: Option<(String, String)>,
    turn: Turn,
}

#[derive(Debug, Default)]
struct State {
    sessions: HashMap<String, Session>,
    /// Pending permission id → session id.
    permissions: HashMap<String, String>,
}

struct Inner {
    base: String,
    password: String,
    http: reqwest::Client,
    tx: broadcast::Sender<HarnessEvent>,
    state: Mutex<State>,
    stopped: AtomicBool,
    exited: AtomicBool,
    kill: Mutex<Option<oneshot::Sender<()>>>,
    monitor: Mutex<Option<JoinHandle<()>>>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
    /// `(provider, method index)` of a sign-in waiting for a pasted code.
    pending_code: Mutex<Option<(String, usize)>>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        for t in lock(&self.tasks).drain(..) {
            t.abort();
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    match m.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    }
}

/// Driver for a running `opencode serve`.
#[derive(Clone)]
pub struct OpenCodeDriver {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for OpenCodeDriver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenCodeDriver")
            .field("base", &self.inner.base)
            .finish_non_exhaustive()
    }
}

fn http_client() -> HarnessResult<reqwest::Client> {
    reqwest::Client::builder()
        .no_proxy()
        .connect_timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| HarnessError::Harness(format!("http client: {e}")))
}

fn str_at<'a>(v: &'a Value, ptr: &str) -> Option<&'a str> {
    v.pointer(ptr).and_then(Value::as_str)
}

fn split_model(model: &str) -> Option<(String, String)> {
    let (p, m) = model.split_once('/')?;
    (!p.is_empty() && !m.is_empty()).then(|| (p.to_owned(), m.to_owned()))
}

fn agent_for(mode: OmniMode) -> &'static str {
    match mode {
        OmniMode::Assistant => AGENT_ASSISTANT,
        OmniMode::Workspace | OmniMode::ComputerUse => AGENT_WORKSPACE,
    }
}

fn path_segment(s: &str) -> HarnessResult<&str> {
    if s.is_empty() || s.contains(['/', '?', '#', '%']) || s.chars().any(char::is_whitespace) {
        return Err(HarnessError::Protocol("invalid id".into()));
    }
    Ok(s)
}

/// Providers in sign-in preference order, then alphabetically.
fn sorted_providers<'a>(ids: impl IntoIterator<Item = &'a String>) -> Vec<&'a str> {
    let mut ids: Vec<&str> = ids.into_iter().map(String::as_str).collect();
    ids.sort_by_key(|id| {
        let rank = LOGIN_PREFERENCE
            .iter()
            .position(|p| p == id)
            .unwrap_or(LOGIN_PREFERENCE.len());
        (rank, *id)
    });
    ids
}

fn method_type(method: &Value) -> Option<&str> {
    method.get("type").and_then(Value::as_str)
}

/// Provider id → display name from `GET /provider` (`all[] {id, name}`).
fn provider_names(providers: &Value) -> HashMap<String, String> {
    providers
        .get("all")
        .and_then(Value::as_array)
        .map(|all| {
            all.iter()
                .filter_map(|p| {
                    let id = p.get("id").and_then(Value::as_str)?;
                    let name = p
                        .get("name")
                        .and_then(Value::as_str)
                        .filter(|n| !n.trim().is_empty())?;
                    Some((id.to_owned(), name.to_owned()))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Login options from `GET /provider/auth` and (optionally) `GET /provider`.
fn login_options_from(auth: &Value, providers: &Value) -> Vec<LoginOption> {
    let Some(map) = auth.as_object() else {
        return Vec::new();
    };
    let names = provider_names(providers);
    let mut out = Vec::new();
    for provider in sorted_providers(map.keys()) {
        let Some(methods) = map.get(provider).and_then(Value::as_array) else {
            continue;
        };
        let display = names.get(provider).map_or(provider, String::as_str);
        for (index, method) in methods.iter().enumerate() {
            let (kind, fallback) = match method_type(method) {
                Some(METHOD_OAUTH) => (LoginKind::Browser, DEFAULT_OAUTH_LABEL),
                Some(METHOD_API) => (LoginKind::ApiKey, DEFAULT_API_LABEL),
                _ => continue,
            };
            let label = method
                .get("label")
                .and_then(Value::as_str)
                .filter(|l| !l.trim().is_empty())
                .unwrap_or(fallback);
            out.push(LoginOption {
                id: format!("{provider}{OPTION_SEP}{index}"),
                label: format!("{display}{OPTION_LABEL_SEP}{label}"),
                kind,
            });
        }
    }
    out
}

/// Parses a `<provider>:<index>` option id.
fn parse_option(option: &str) -> HarnessResult<(String, usize)> {
    let bad = || HarnessError::Protocol("invalid sign-in option".into());
    let (provider, index) = option.rsplit_once(OPTION_SEP).ok_or_else(bad)?;
    let provider = path_segment(provider).map_err(|_| bad())?;
    let index = index.parse::<usize>().map_err(|_| bad())?;
    Ok((provider.to_owned(), index))
}

/// Provider of an option id: `<provider>:<index>` or a bare provider id.
fn option_provider(option: &str) -> HarnessResult<String> {
    let provider = match option.rsplit_once(OPTION_SEP) {
        Some((p, i)) if i.parse::<usize>().is_ok() => p,
        Some(_) => return Err(HarnessError::Protocol("invalid sign-in option".into())),
        None => option,
    };
    path_segment(provider)
        .map(str::to_owned)
        .map_err(|_| HarnessError::Protocol("invalid sign-in option".into()))
}

/// `"provider/model"` names from `GET /config/providers`.
fn models_from(v: &Value) -> Vec<String> {
    let mut out = Vec::new();
    let Some(providers) = v.get("providers").and_then(Value::as_array) else {
        return out;
    };
    for p in providers {
        let Some(pid) = p
            .get("id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        let ids: Vec<&str> = match p.get("models") {
            Some(Value::Object(m)) => m
                .iter()
                .map(|(k, v)| v.get("id").and_then(Value::as_str).unwrap_or(k))
                .collect(),
            Some(Value::Array(a)) => a
                .iter()
                .filter_map(|m| match m {
                    Value::String(s) => Some(s.as_str()),
                    other => other.get("id").and_then(Value::as_str),
                })
                .collect(),
            _ => Vec::new(),
        };
        out.extend(
            ids.into_iter()
                .filter(|m| !m.is_empty())
                .map(|m| format!("{pid}/{m}")),
        );
    }
    out.sort();
    out.dedup();
    out
}

/// Normalizes an OpenAPI path: every parameter segment (`{id}`,
/// `{sessionID}`, `:id`) becomes `{}`.
fn normalize_path(path: &str) -> String {
    path.trim_end_matches('/')
        .split('/')
        .map(|seg| {
            if seg.starts_with(':') || (seg.starts_with('{') && seg.ends_with('}')) {
                "{}"
            } else {
                seg
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Entries of [`PROTOCOL_PATHS`] absent from an OpenAPI document's `paths`.
pub fn missing_paths(openapi: &Value) -> Vec<&'static str> {
    let have: HashSet<String> = openapi
        .get("paths")
        .and_then(Value::as_object)
        .map(|p| p.keys().map(|k| normalize_path(k)).collect())
        .unwrap_or_default();
    PROTOCOL_PATHS
        .iter()
        .copied()
        .filter(|p| !have.contains(&normalize_path(p)))
        .collect()
}

/// `opencode --version`, or `None` if it is missing or does not answer.
pub async fn version(path: Option<&Path>) -> Option<String> {
    let exe = find_executable(BINARY, path)?;
    let run = Command::new(exe)
        .arg("--version")
        .env_clear()
        .envs(crate::env::child_env())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    let out = tokio::time::timeout(VERSION_TIMEOUT, run)
        .await
        .ok()?
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(str::to_owned)
}

impl OpenCodeDriver {
    /// Connects to a server at `base_url` (e.g. `http://127.0.0.1:4096`)
    /// and starts reading its event stream. `child`, when given, is the
    /// server process: its exit is reported and `shutdown` kills it.
    pub fn connect(
        base_url: impl Into<String>,
        password: impl Into<String>,
        child: Option<Child>,
    ) -> HarnessResult<Self> {
        let base = base_url.into().trim_end_matches('/').to_owned();
        let (tx, _) = broadcast::channel(512);
        let inner = Arc::new(Inner {
            base,
            password: password.into(),
            http: http_client()?,
            tx,
            state: Mutex::new(State::default()),
            stopped: AtomicBool::new(false),
            exited: AtomicBool::new(false),
            kill: Mutex::new(None),
            monitor: Mutex::new(None),
            tasks: Mutex::new(Vec::new()),
            pending_code: Mutex::new(None),
        });
        let weak = Arc::downgrade(&inner);
        let sse = tokio::spawn(sse_loop(
            weak.clone(),
            inner.http.clone(),
            format!("{}{PATH_EVENT}", inner.base),
            inner.password.clone(),
        ));
        lock(&inner.tasks).push(sse);
        if let Some(child) = child {
            let (kill_tx, kill_rx) = oneshot::channel();
            *lock(&inner.kill) = Some(kill_tx);
            *lock(&inner.monitor) = Some(tokio::spawn(monitor(child, kill_rx, weak)));
        }
        Ok(Self { inner })
    }

    fn check_running(&self) -> HarnessResult<()> {
        if self.inner.stopped.load(Ordering::SeqCst) {
            Err(HarnessError::NotRunning)
        } else {
            Ok(())
        }
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.inner.request(method, path)
    }

    async fn call(
        &self,
        builder: reqwest::RequestBuilder,
        what: &str,
    ) -> HarnessResult<reqwest::Response> {
        let resp = builder.send().await.map_err(|e| {
            if e.is_timeout() {
                HarnessError::Timeout
            } else if e.is_connect() {
                HarnessError::NotRunning
            } else {
                HarnessError::Protocol(format!("{what}: request failed"))
            }
        })?;
        let status = resp.status();
        if status.is_success() {
            Ok(resp)
        } else {
            tracing::debug!(what, status = status.as_u16(), "opencode request rejected");
            Err(HarnessError::Harness(format!(
                "{what} failed: HTTP {}",
                status.as_u16()
            )))
        }
    }

    async fn json(&self, builder: reqwest::RequestBuilder, what: &str) -> HarnessResult<Value> {
        let resp = self.call(builder, what).await?;
        let bytes = resp
            .bytes()
            .await
            .map_err(|_| HarnessError::Protocol(format!("{what}: body")))?;
        if bytes.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_slice(&bytes).map_err(|_| HarnessError::Protocol(format!("{what}: json")))
    }

    fn emit(&self, e: HarnessEvent) {
        let _ = self.inner.tx.send(e);
    }

    fn register(&self, id: &str, cfg: &SessionConfig) {
        let session = Session {
            agent: agent_for(cfg.mode),
            system: cfg.instructions.clone(),
            model: cfg.model.as_deref().and_then(split_model),
            turn: Turn::default(),
        };
        lock(&self.inner.state)
            .sessions
            .insert(id.to_owned(), session);
    }

    async fn read_login(&self) -> HarnessResult<LoginState> {
        let v = self
            .json(
                self.request(reqwest::Method::GET, PATH_PROVIDER),
                "provider",
            )
            .await?;
        let first = v
            .get("connected")
            .and_then(Value::as_array)
            .and_then(|a| a.iter().find_map(Value::as_str));
        Ok(match first {
            Some(id) => LoginState::Ready {
                account: Some(id.to_owned()),
            },
            None => LoginState::SignedOut,
        })
    }

    /// Fetches the server's OpenAPI document (`GET /doc`) and returns the
    /// protocol paths it lacks.
    pub async fn doc_check(&self) -> HarnessResult<Vec<&'static str>> {
        self.check_running()?;
        let doc = self
            .json(self.request(reqwest::Method::GET, PATH_DOC), "doc")
            .await?;
        Ok(missing_paths(&doc))
    }

    async fn auth_methods(&self) -> HarnessResult<Value> {
        self.json(
            self.request(reqwest::Method::GET, PATH_PROVIDER_AUTH),
            "provider auth",
        )
        .await
    }

    /// State after `provider` finished signing in.
    async fn ready_after(&self, provider: &str) -> LoginState {
        match self
            .json(
                self.request(reqwest::Method::GET, PATH_PROVIDER),
                "provider",
            )
            .await
        {
            Ok(v) => {
                let connected: Vec<&str> = v
                    .get("connected")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(Value::as_str).collect())
                    .unwrap_or_default();
                let account = if connected.contains(&provider) {
                    provider
                } else {
                    connected.first().copied().unwrap_or(provider)
                };
                LoginState::Ready {
                    account: Some(account.to_owned()),
                }
            }
            Err(_) => LoginState::Ready {
                account: Some(provider.to_owned()),
            },
        }
    }

    /// `POST /provider/:id/oauth/authorize` and, for the `auto` method, a
    /// background long-poll on the callback that reports completion.
    async fn start_oauth(&self, provider: String, index: usize) -> HarnessResult<LoginState> {
        let provider_seg = path_segment(&provider)?.to_owned();
        *lock(&self.inner.pending_code) = None;
        let auth = self
            .json(
                self.request(
                    reqwest::Method::POST,
                    &format!("{PATH_PROVIDER}/{provider_seg}/{SUFFIX_OAUTH_AUTHORIZE}"),
                )
                .json(&json!({ "method": index })),
                "oauth authorize",
            )
            .await?;
        let url = auth.get("url").and_then(Value::as_str).map(str::to_owned);
        let instructions = auth
            .get("instructions")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        if auth.get("method").and_then(Value::as_str) == Some(AUTHORIZE_CODE) {
            tracing::debug!(provider = %provider, "opencode code sign-in started");
            *lock(&self.inner.pending_code) = Some((provider, index));
            return Ok(LoginState::SigningIn {
                url,
                instructions,
                needs_code: true,
            });
        }
        tracing::debug!(provider = %provider, "opencode sign-in started");

        let me = self.clone();
        let task = tokio::spawn(async move {
            let req = me
                .inner
                .http
                .post(format!(
                    "{}{PATH_PROVIDER}/{provider_seg}/{SUFFIX_OAUTH_CALLBACK}",
                    me.inner.base
                ))
                .basic_auth(AUTH_USER, Some(&me.inner.password))
                .json(&json!({ "method": index }));
            let state = match me.json(req, "oauth callback").await {
                Ok(Value::Bool(false)) => LoginState::Error {
                    message: "OpenCode sign-in did not complete".into(),
                },
                Ok(_) => me.ready_after(&provider).await,
                Err(e) => LoginState::Error {
                    message: format!("OpenCode sign-in failed: {e}"),
                },
            };
            me.emit(HarnessEvent::Login(state));
        });
        lock(&self.inner.tasks).push(task);
        Ok(LoginState::SigningIn {
            url,
            instructions,
            needs_code: false,
        })
    }
}

impl Inner {
    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.http
            .request(method, format!("{}{path}", self.base))
            .basic_auth(AUTH_USER, Some(&self.password))
            .timeout(REQUEST_TIMEOUT)
    }

    fn emit_exited(&self, message: Option<String>) {
        self.stopped.store(true, Ordering::SeqCst);
        if !self.exited.swap(true, Ordering::SeqCst) {
            let _ = self.tx.send(HarnessEvent::Exited { message });
        }
    }

    fn handle_event(&self, v: &Value) {
        let v = match v.get("type") {
            Some(_) => v,
            None => match v.get("payload") {
                Some(p) => p,
                None => return,
            },
        };
        let Some(kind) = v.get("type").and_then(Value::as_str) else {
            return;
        };
        let null = Value::Null;
        let props = v.get("properties").unwrap_or(&null);
        let mut out = Vec::new();
        {
            let mut state = lock(&self.state);
            handle(&mut state, kind, props, &mut out);
        }
        for e in out {
            let _ = self.tx.send(e);
        }
    }
}

fn session_of(props: &Value) -> Option<&str> {
    props
        .get("sessionID")
        .and_then(Value::as_str)
        .or_else(|| str_at(props, "/part/sessionID"))
        .or_else(|| str_at(props, "/info/sessionID"))
}

fn handle(state: &mut State, kind: &str, props: &Value, out: &mut Vec<HarnessEvent>) {
    match kind {
        EV_PERMISSION_REPLIED => {
            let id = props
                .get("permissionID")
                .or_else(|| props.get("requestID"))
                .or_else(|| props.get("id"))
                .and_then(Value::as_str);
            if let Some(id) = id {
                if state.permissions.remove(id).is_some() {
                    out.push(HarnessEvent::RequestResolved { id: id.to_owned() });
                }
            }
            return;
        }
        EV_PERMISSION_UPDATED | EV_PERMISSION_ASKED => {
            permission(state, props, out);
            return;
        }
        _ => {}
    }
    let Some(sid) = session_of(props) else {
        return;
    };
    let Some(session) = state.sessions.get_mut(sid) else {
        return;
    };
    let sid = sid.to_owned();
    let turn = &mut session.turn;
    match kind {
        EV_MESSAGE_UPDATED => {
            let Some(info) = props.get("info") else {
                return;
            };
            let (Some(id), Some(role)) = (
                info.get("id").and_then(Value::as_str),
                info.get("role").and_then(Value::as_str),
            ) else {
                return;
            };
            turn.roles.insert(id.to_owned(), role.to_owned());
            if role == "assistant" {
                if let Some(tokens) = info.get("tokens") {
                    let n = |k: &str| tokens.get(k).and_then(Value::as_u64);
                    if n("input").is_some() || n("output").is_some() {
                        turn.usage = Some(TokenUsage {
                            input: n("input").unwrap_or(0),
                            output: n("output").unwrap_or(0),
                        });
                    }
                }
            }
        }
        EV_PART_UPDATED => {
            let Some(part) = props.get("part") else {
                return;
            };
            match part.get("type").and_then(Value::as_str) {
                Some("text") => {
                    let delta = props.get("delta").and_then(Value::as_str);
                    text_part(turn, &sid, part, delta, out);
                }
                Some("tool") => tool_part(turn, &sid, part, out),
                _ => {}
            }
        }
        EV_SESSION_STATUS => match str_at(props, "/status/type") {
            Some("busy") => start_turn(turn, &sid, out),
            Some("idle") => finish_turn(turn, &sid, out),
            _ => {}
        },
        EV_SESSION_IDLE => finish_turn(turn, &sid, out),
        EV_SESSION_ERROR => {
            let message = str_at(props, "/error/data/message")
                .or_else(|| str_at(props, "/error/name"))
                .unwrap_or("OpenCode error")
                .to_owned();
            *turn = Turn::default();
            out.push(HarnessEvent::TurnFailed {
                session: sid,
                message,
            });
        }
        EV_SESSION_COMPACTED => out.push(HarnessEvent::Compacted { session: sid }),
        _ => {}
    }
}

fn start_turn(turn: &mut Turn, sid: &str, out: &mut Vec<HarnessEvent>) {
    if !turn.active {
        turn.active = true;
        out.push(HarnessEvent::TurnStarted {
            session: sid.to_owned(),
        });
    }
}

fn finish_turn(turn: &mut Turn, sid: &str, out: &mut Vec<HarnessEvent>) {
    if !turn.active && !turn.awaiting {
        return;
    }
    start_turn(turn, sid, out);
    if let Some(last) = turn.parts.last().map(|p| p.message.clone()) {
        let text = turn
            .parts
            .iter()
            .filter(|p| p.message == last && !p.text.trim().is_empty())
            .map(|p| p.text.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");
        if !text.trim().is_empty() {
            out.push(HarnessEvent::ItemCompleted {
                session: sid.to_owned(),
                item: TranscriptItem {
                    kind: ItemKind::Message,
                    text,
                },
            });
        }
    }
    let usage = turn.usage;
    *turn = Turn::default();
    out.push(HarnessEvent::TurnCompleted {
        session: sid.to_owned(),
        usage,
    });
}

fn text_part(
    turn: &mut Turn,
    sid: &str,
    part: &Value,
    delta: Option<&str>,
    out: &mut Vec<HarnessEvent>,
) {
    let flag = |k: &str| part.get(k).and_then(Value::as_bool).unwrap_or(false);
    if flag("synthetic") || flag("ignored") {
        return;
    }
    let Some(pid) = part.get("id").and_then(Value::as_str) else {
        return;
    };
    let mid = part.get("messageID").and_then(Value::as_str).unwrap_or("");
    let text = part.get("text").and_then(Value::as_str);
    match turn.roles.get(mid).map(String::as_str) {
        Some("assistant") => {}
        Some(_) => return,
        None => {
            if !turn.active && !turn.awaiting {
                return;
            }
            // An echo of our own prompt before its `message.updated`.
            if text.is_some() && text == turn.last_prompt.as_deref() {
                turn.roles.insert(mid.to_owned(), "user".to_owned());
                return;
            }
        }
    }
    start_turn(turn, sid, out);
    let idx = match turn.parts.iter().position(|p| p.id == pid) {
        Some(i) => i,
        None => {
            turn.parts.push(TextPart {
                id: pid.to_owned(),
                message: mid.to_owned(),
                text: String::new(),
            });
            turn.parts.len() - 1
        }
    };
    let entry = &mut turn.parts[idx];
    let emitted = match (delta, text) {
        (Some(d), Some(t)) => {
            entry.text = t.to_owned();
            d.to_owned()
        }
        (Some(d), None) => {
            entry.text.push_str(d);
            d.to_owned()
        }
        (None, Some(t)) => {
            let suffix = t
                .strip_prefix(entry.text.as_str())
                .map(str::to_owned)
                .unwrap_or_default();
            entry.text = t.to_owned();
            suffix
        }
        (None, None) => return,
    };
    if !emitted.is_empty() {
        out.push(HarnessEvent::Delta {
            session: sid.to_owned(),
            text: emitted,
        });
    }
}

fn tool_part(turn: &mut Turn, sid: &str, part: &Value, out: &mut Vec<HarnessEvent>) {
    if str_at(part, "/state/status") != Some("completed") {
        return;
    }
    let Some(pid) = part.get("id").and_then(Value::as_str) else {
        return;
    };
    if let Some(mid) = part.get("messageID").and_then(Value::as_str) {
        if turn.roles.get(mid).is_some_and(|r| r != "assistant") {
            return;
        }
    }
    if !turn.tools_done.insert(pid.to_owned()) {
        return;
    }
    let name = part.get("tool").and_then(Value::as_str).unwrap_or("tool");
    let kind = if name == SUBAGENT_TOOL {
        ItemKind::Subagent
    } else {
        // Litecord tools (`litecord_*`) and every other tool are tool calls.
        if name.starts_with(LITECORD_TOOL_PREFIX) {
            tracing::debug!(tool = name, "litecord tool completed");
        }
        ItemKind::ToolCall
    };
    start_turn(turn, sid, out);
    out.push(HarnessEvent::ItemCompleted {
        session: sid.to_owned(),
        item: TranscriptItem {
            kind,
            text: name.to_owned(),
        },
    });
}

fn permission(state: &mut State, props: &Value, out: &mut Vec<HarnessEvent>) {
    let (Some(id), Some(sid)) = (
        props.get("id").and_then(Value::as_str),
        props.get("sessionID").and_then(Value::as_str),
    ) else {
        return;
    };
    if !state.sessions.contains_key(sid) {
        return;
    }
    let kind = props
        .get("type")
        .or_else(|| props.get("permission"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let title = props
        .get("title")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let pattern = props
        .get("pattern")
        .or_else(|| props.get("patterns"))
        .and_then(|p| match p {
            Value::String(s) => Some(s.clone()),
            Value::Array(a) => Some(
                a.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" "),
            ),
            _ => None,
        })
        .filter(|s| !s.is_empty());
    let label = title.or(pattern);
    let request = match kind {
        "bash" => RequestKind::Command {
            command: label.unwrap_or_else(|| "command".into()),
            cwd: None,
        },
        "edit" | "write" => RequestKind::FileChange {
            summary: label.unwrap_or_else(|| "file change".into()),
        },
        other => RequestKind::Permission {
            title: label.unwrap_or_else(|| {
                if other.is_empty() {
                    "permission".into()
                } else {
                    other.to_owned()
                }
            }),
        },
    };
    state.permissions.insert(id.to_owned(), sid.to_owned());
    out.push(HarnessEvent::Request(HarnessRequest {
        id: id.to_owned(),
        session: sid.to_owned(),
        request,
        reason: None,
    }));
}

async fn sse_loop(weak: Weak<Inner>, http: reqwest::Client, url: String, password: String) {
    let mut backoff = BACKOFF_MIN;
    loop {
        match weak.upgrade() {
            Some(inner) if !inner.stopped.load(Ordering::SeqCst) => {}
            _ => return,
        }
        let req = http
            .get(&url)
            .basic_auth(AUTH_USER, Some(&password))
            .header(reqwest::header::ACCEPT, "text/event-stream");
        match req.send().await {
            Ok(resp) if resp.status().is_success() => {
                tracing::debug!("opencode event stream connected");
                backoff = BACKOFF_MIN;
                let mut parser = SseParser::new();
                let mut stream = resp.bytes_stream();
                while let Some(chunk) = stream.next().await {
                    let Ok(chunk) = chunk else {
                        break;
                    };
                    let events = parser.push(&chunk);
                    if events.is_empty() {
                        continue;
                    }
                    let Some(inner) = weak.upgrade() else {
                        return;
                    };
                    for data in events {
                        match serde_json::from_str::<Value>(&data) {
                            Ok(v) => inner.handle_event(&v),
                            Err(_) => tracing::debug!(len = data.len(), "unparsed opencode event"),
                        }
                    }
                }
                tracing::debug!("opencode event stream ended");
            }
            Ok(resp) => {
                tracing::debug!(
                    status = resp.status().as_u16(),
                    "opencode event stream rejected"
                );
            }
            Err(_) => tracing::debug!("opencode event stream connect failed"),
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
}

async fn monitor(mut child: Child, kill: oneshot::Receiver<()>, weak: Weak<Inner>) {
    tokio::select! {
        status = child.wait() => {
            let message = match status {
                Ok(s) => format!("OpenCode exited ({s})"),
                Err(_) => "OpenCode exited".to_owned(),
            };
            tracing::debug!("opencode server exited");
            if let Some(inner) = weak.upgrade() {
                inner.emit_exited(Some(message));
            }
        }
        _ = kill => {
            let _ = child.kill().await;
        }
    }
}

#[async_trait]
impl HarnessDriver for OpenCodeDriver {
    fn kind(&self) -> HarnessKind {
        HarnessKind::OpenCode
    }

    fn subscribe(&self) -> broadcast::Receiver<HarnessEvent> {
        self.inner.tx.subscribe()
    }

    async fn login_state(&self) -> HarnessResult<LoginState> {
        self.check_running()?;
        self.read_login().await
    }

    async fn begin_login(&self) -> HarnessResult<LoginState> {
        self.check_running()?;
        let auth = self.auth_methods().await?;
        let first = login_options_from(&auth, &Value::Null)
            .into_iter()
            .find(|o| o.kind == LoginKind::Browser)
            .ok_or(HarnessError::Unsupported(NO_OAUTH))?;
        let (provider, index) = parse_option(&first.id)?;
        self.start_oauth(provider, index).await
    }

    async fn login_options(&self) -> HarnessResult<Vec<LoginOption>> {
        self.check_running()?;
        let auth = self.auth_methods().await?;
        let providers = self
            .json(
                self.request(reqwest::Method::GET, PATH_PROVIDER),
                "provider",
            )
            .await
            .unwrap_or(Value::Null);
        Ok(login_options_from(&auth, &providers))
    }

    async fn begin_login_with(&self, option: &str) -> HarnessResult<LoginState> {
        self.check_running()?;
        let (provider, index) = parse_option(option)?;
        let auth = self.auth_methods().await?;
        let is_oauth = auth
            .get(&provider)
            .and_then(Value::as_array)
            .and_then(|m| m.get(index))
            .and_then(method_type)
            == Some(METHOD_OAUTH);
        if !is_oauth {
            return Err(HarnessError::Unsupported("unknown browser sign-in option"));
        }
        self.start_oauth(provider, index).await
    }

    async fn submit_login_code(&self, code: &str) -> HarnessResult<()> {
        self.check_running()?;
        let (provider, index) = lock(&self.inner.pending_code)
            .clone()
            .ok_or_else(|| HarnessError::Protocol("no sign-in is waiting for a code".into()))?;
        let code = code.trim();
        if code.is_empty() {
            return Err(HarnessError::Protocol("empty sign-in code".into()));
        }
        let seg = path_segment(&provider)?;
        let res = self
            .json(
                self.request(
                    reqwest::Method::POST,
                    &format!("{PATH_PROVIDER}/{seg}/{SUFFIX_OAUTH_CALLBACK}"),
                )
                .json(&json!({ "method": index, "code": code })),
                "oauth callback",
            )
            .await;
        *lock(&self.inner.pending_code) = None;
        let (state, result) = match res {
            Ok(Value::Bool(false)) => (
                LoginState::Error {
                    message: "OpenCode sign-in did not complete".into(),
                },
                Err(HarnessError::Harness(
                    "sign-in code was not accepted".into(),
                )),
            ),
            Ok(_) => (self.ready_after(&provider).await, Ok(())),
            Err(e) => (
                LoginState::Error {
                    message: format!("OpenCode sign-in failed: {e}"),
                },
                Err(e),
            ),
        };
        tracing::debug!(provider = %provider, ok = result.is_ok(), "opencode code sign-in finished");
        self.emit(HarnessEvent::Login(state));
        result
    }

    async fn login_api_key(&self, option: &str, key: &str) -> HarnessResult<()> {
        self.check_running()?;
        let provider = option_provider(option)?;
        if key.trim().is_empty() {
            return Err(HarnessError::Protocol("empty API key".into()));
        }
        self.call(
            self.request(reqwest::Method::PUT, &format!("{PATH_AUTH}/{provider}"))
                .json(&json!({ "type": METHOD_API, "key": key.trim() })),
            "set API key",
        )
        .await?;
        tracing::debug!(provider = %provider, "opencode API key stored");
        let state = self.ready_after(&provider).await;
        self.emit(HarnessEvent::Login(state));
        Ok(())
    }

    async fn models(&self) -> HarnessResult<Vec<String>> {
        if self.check_running().is_err() {
            return Ok(Vec::new());
        }
        Ok(self
            .json(
                self.request(reqwest::Method::GET, PATH_CONFIG_PROVIDERS),
                "config providers",
            )
            .await
            .map(|v| models_from(&v))
            .unwrap_or_default())
    }

    async fn logout(&self) -> HarnessResult<()> {
        Err(HarnessError::Unsupported(
            "sign out with `opencode auth logout`",
        ))
    }

    async fn start_session(&self, cfg: &SessionConfig) -> HarnessResult<ExternalId> {
        self.check_running()?;
        let v = self
            .json(
                self.request(reqwest::Method::POST, PATH_SESSION)
                    .json(&json!({ "title": SESSION_TITLE })),
                "create session",
            )
            .await?;
        let id = v
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| HarnessError::Protocol("create session: missing id".into()))?
            .to_owned();
        path_segment(&id)?;
        self.register(&id, cfg);
        tracing::debug!(session = %id, "opencode session started");
        Ok(id)
    }

    async fn resume_session(&self, id: &str, cfg: &SessionConfig) -> HarnessResult<ExternalId> {
        self.check_running()?;
        let seg = path_segment(id)?;
        let resp = self
            .request(reqwest::Method::GET, &format!("{PATH_SESSION}/{seg}"))
            .send()
            .await
            .map_err(|_| HarnessError::NotRunning)?;
        let status = resp.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(HarnessError::Unsupported("session no longer exists"));
        }
        if !status.is_success() {
            return Err(HarnessError::Harness(format!(
                "get session failed: HTTP {}",
                status.as_u16()
            )));
        }
        self.register(id, cfg);
        Ok(id.to_owned())
    }

    async fn send(&self, session: &str, text: &str) -> HarnessResult<()> {
        self.check_running()?;
        let seg = path_segment(session)?;
        let body = {
            let mut st = lock(&self.inner.state);
            let s = st
                .sessions
                .get_mut(session)
                .ok_or_else(|| HarnessError::Protocol("unknown session".into()))?;
            s.turn.awaiting = true;
            s.turn.last_prompt = Some(text.to_owned());
            let mut body = json!({
                "agent": s.agent,
                "system": s.system,
                "parts": [{ "type": "text", "text": text }],
            });
            if let Some((p, m)) = &s.model {
                body["model"] = json!({ "providerID": p, "modelID": m });
            }
            body
        };
        let res = self
            .call(
                self.request(
                    reqwest::Method::POST,
                    &format!("{PATH_SESSION}/{seg}/{SUFFIX_PROMPT}"),
                )
                .json(&body),
                "prompt",
            )
            .await;
        if let Err(e) = res {
            if let Some(s) = lock(&self.inner.state).sessions.get_mut(session) {
                if !s.turn.active {
                    s.turn.awaiting = false;
                }
            }
            return Err(e);
        }
        Ok(())
    }

    async fn interrupt(&self, session: &str) -> HarnessResult<()> {
        self.check_running()?;
        let seg = path_segment(session)?;
        self.call(
            self.request(
                reqwest::Method::POST,
                &format!("{PATH_SESSION}/{seg}/{SUFFIX_ABORT}"),
            ),
            "abort",
        )
        .await?;
        Ok(())
    }

    async fn compact(&self, session: &str) -> HarnessResult<()> {
        self.check_running()?;
        let seg = path_segment(session)?;
        let configured = lock(&self.inner.state)
            .sessions
            .get(session)
            .and_then(|s| s.model.clone());
        let (provider, model) = match configured {
            Some(m) => m,
            None => {
                let cfg = self
                    .json(self.request(reqwest::Method::GET, PATH_CONFIG), "config")
                    .await
                    .ok();
                cfg.as_ref()
                    .and_then(|c| c.get("model"))
                    .and_then(Value::as_str)
                    .and_then(split_model)
                    .ok_or(HarnessError::Unsupported(
                        "compaction needs a model; choose one in Settings",
                    ))?
            }
        };
        self.call(
            self.request(
                reqwest::Method::POST,
                &format!("{PATH_SESSION}/{seg}/{SUFFIX_SUMMARIZE}"),
            )
            .timeout(SUMMARIZE_TIMEOUT)
            .json(&json!({ "providerID": provider, "modelID": model })),
            "summarize",
        )
        .await?;
        Ok(())
    }

    async fn answer(&self, request_id: &str, decision: Decision) -> HarnessResult<()> {
        self.check_running()?;
        let seg = path_segment(request_id)?;
        let session = lock(&self.inner.state)
            .permissions
            .get(request_id)
            .cloned()
            .ok_or_else(|| HarnessError::Protocol(format!("unknown request {request_id}")))?;
        let response = match decision {
            Decision::Accept => "once",
            Decision::AcceptForSession => "always",
            Decision::Decline => "reject",
        };
        self.call(
            self.request(
                reqwest::Method::POST,
                &format!("{PATH_SESSION}/{session}/{SUFFIX_PERMISSIONS}/{seg}"),
            )
            .json(&json!({ "response": response })),
            "permission reply",
        )
        .await?;
        lock(&self.inner.state).permissions.remove(request_id);
        Ok(())
    }

    async fn shutdown(&self) {
        self.inner.stopped.store(true, Ordering::SeqCst);
        for t in lock(&self.inner.tasks).drain(..) {
            t.abort();
        }
        if let Some(kill) = lock(&self.inner.kill).take() {
            let _ = kill.send(());
        }
        let monitor = lock(&self.inner.monitor).take();
        if let Some(m) = monitor {
            let _ = tokio::time::timeout(SHUTDOWN_WAIT, m).await;
        }
        lock(&self.inner.state).permissions.clear();
        self.inner.emit_exited(None);
    }
}

// ---------------------------------------------------------------------------
// Launcher
// ---------------------------------------------------------------------------

/// Starts `opencode serve` on demand.
#[derive(Debug, Clone, Default)]
pub struct OpenCodeLauncher {
    /// Explicit path to the `opencode` binary; `PATH` is searched if unset.
    pub path: Option<PathBuf>,
}

impl OpenCodeLauncher {
    pub fn new(path: Option<PathBuf>) -> Self {
        Self { path }
    }

    /// Start `opencode serve` and return the concrete driver (used by
    /// `launch` and by diagnostics that need OpenCode-specific checks).
    pub async fn spawn(&self, ctx: &LaunchContext) -> HarnessResult<OpenCodeDriver> {
        let exe = find_executable(BINARY, self.path.as_deref())
            .ok_or(HarnessError::NotInstalled("OpenCode"))?;
        let port = free_port()?;
        let password = random_password()?;
        let config = config_content(ctx).to_string();
        let mut child = Command::new(exe)
            .args(["serve", "--hostname", "127.0.0.1", "--port"])
            .arg(port.to_string())
            .env_clear()
            .envs(crate::env::child_env())
            .env(ENV_PASSWORD, &password)
            .env(ENV_CONFIG, config)
            .current_dir(&ctx.workspace)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| HarnessError::Harness(format!("could not start OpenCode: {e}")))?;
        if let Some(out) = child.stdout.take() {
            drain(out, "stdout");
        }
        if let Some(err) = child.stderr.take() {
            drain(err, "stderr");
        }
        let base = format!("http://127.0.0.1:{port}");
        if !wait_healthy(&mut child, &base, &password).await {
            let _ = child.kill().await;
            return Err(HarnessError::Harness(
                "OpenCode server did not start".into(),
            ));
        }
        tracing::debug!(port, "opencode server ready");
        OpenCodeDriver::connect(base, password, Some(child))
    }
}

/// The `OPENCODE_CONFIG_CONTENT` for a launch.
pub fn config_content(ctx: &LaunchContext) -> Value {
    let mut command = vec![ctx.mcp.command.to_string_lossy().into_owned()];
    command.extend(ctx.mcp.args.iter().cloned());
    json!({
        "$schema": "https://opencode.ai/config.json",
        "mcp": {
            "litecord": { "type": "local", "command": command, "enabled": true }
        },
        "share": "disabled",
        "autoupdate": false,
        "agent": {
            AGENT_ASSISTANT: {
                "mode": "primary",
                "description": "Litecord assistant",
                "tools": {
                    "*": false,
                    "litecord_*": true,
                    "todowrite": true,
                    "todoread": true,
                    "task": true
                },
                "permission": {
                    "edit": "deny",
                    "bash": "deny",
                    "webfetch": "deny",
                    "external_directory": "deny"
                }
            },
            AGENT_WORKSPACE: {
                "mode": "primary",
                "description": "Litecord assistant with workspace tools",
                "tools": { "litecord_*": true },
                "permission": {
                    "edit": "ask",
                    "bash": "ask",
                    "webfetch": "ask",
                    "external_directory": "deny"
                }
            }
        }
    })
}

fn free_port() -> HarnessResult<u16> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")
        .map_err(|e| HarnessError::Harness(format!("no free port: {e}")))?;
    let port = listener
        .local_addr()
        .map_err(|e| HarnessError::Harness(format!("no free port: {e}")))?
        .port();
    drop(listener);
    Ok(port)
}

fn random_password() -> HarnessResult<String> {
    let mut raw = [0u8; 32];
    getrandom::fill(&mut raw).map_err(|_| HarnessError::Harness("no randomness".into()))?;
    Ok(raw.iter().map(|b| format!("{b:02x}")).collect())
}

fn drain<R: AsyncRead + Unpin + Send + 'static>(reader: R, stream: &'static str) {
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            tracing::debug!(stream, len = line.len(), "opencode output");
        }
    });
}

async fn wait_healthy(child: &mut Child, base: &str, password: &str) -> bool {
    let Ok(http) = http_client() else {
        return false;
    };
    let deadline = tokio::time::Instant::now() + HEALTH_DEADLINE;
    while tokio::time::Instant::now() < deadline {
        if matches!(child.try_wait(), Ok(Some(_)) | Err(_)) {
            return false;
        }
        let ok = http
            .get(format!("{base}{PATH_HEALTH}"))
            .basic_auth(AUTH_USER, Some(password))
            .timeout(Duration::from_secs(2))
            .send()
            .await
            .is_ok_and(|r| r.status().is_success());
        if ok {
            return true;
        }
        tokio::time::sleep(HEALTH_POLL).await;
    }
    false
}

#[async_trait]
impl HarnessLauncher for OpenCodeLauncher {
    fn kind(&self) -> HarnessKind {
        HarnessKind::OpenCode
    }

    fn installed(&self) -> bool {
        find_executable(BINARY, self.path.as_deref()).is_some()
    }

    async fn launch(&self, ctx: &LaunchContext) -> HarnessResult<Arc<dyn HarnessDriver>> {
        Ok(Arc::new(self.spawn(ctx).await?))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn sse_basic_and_no_space() {
        let mut p = SseParser::new();
        let out = p.push(b"data: {\"a\":1}\n\ndata:{\"b\":2}\n\n");
        assert_eq!(out, ["{\"a\":1}", "{\"b\":2}"]);
    }

    #[test]
    fn sse_multiline_comments_crlf_and_split_chunks() {
        let mut p = SseParser::new();
        assert!(p
            .push(b": keepalive\r\n\r\nevent: x\r\ndata: one\r\nda")
            .is_empty());
        assert!(p.push(b"ta: two\r\n").is_empty());
        assert_eq!(p.push(b"\r\n"), ["one\ntwo"]);
        // Only one leading space is stripped; `data` without a colon is empty.
        assert_eq!(p.push(b"data:  x\ndata\n\n"), [" x\n"]);
        // A blank line with no data dispatches nothing.
        assert!(p.push(b"\n\nid: 3\n\n").is_empty());
    }

    #[test]
    fn sse_utf8_split_across_chunks() {
        let mut p = SseParser::new();
        let s = "data: héllo\n\n".as_bytes();
        let (a, b) = s.split_at(8);
        assert!(p.push(a).is_empty());
        assert_eq!(p.push(b), ["héllo"]);
    }

    #[test]
    fn model_split() {
        assert_eq!(
            split_model("anthropic/claude-x"),
            Some(("anthropic".into(), "claude-x".into()))
        );
        assert_eq!(
            split_model("openrouter/a/b"),
            Some(("openrouter".into(), "a/b".into()))
        );
        assert_eq!(split_model("nomodel"), None);
    }

    #[test]
    fn config_uses_mcp_launch() {
        let ctx = LaunchContext {
            workspace: "/w".into(),
            protected_dir: "/d".into(),
            mcp: McpLaunch {
                command: "/bin/litecord".into(),
                args: vec!["mcp".into(), "--harness".into(), "opencode".into()],
            },
        };
        let c = config_content(&ctx);
        assert_eq!(
            c["mcp"]["litecord"]["command"],
            json!(["/bin/litecord", "mcp", "--harness", "opencode"])
        );
        assert_eq!(c["agent"]["omni"]["tools"]["*"], json!(false));
        assert_eq!(
            c["agent"]["omni-workspace"]["permission"]["external_directory"],
            json!("deny")
        );
    }

    #[test]
    fn password_is_hex_64() {
        let p = random_password().unwrap();
        assert_eq!(p.len(), 64);
        assert!(p.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
