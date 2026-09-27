#![cfg(feature = "opencode")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

//! OpenCode driver against a tiny hand-written HTTP/1.1 fake server.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use litecord_harness::opencode::{missing_paths, OpenCodeDriver, PROTOCOL_PATHS};
use litecord_harness::*;
use serde_json::{json, Value};
use tokio::sync::broadcast;

const PASSWORD: &str = "s3cret";

#[derive(Debug, Clone)]
struct Req {
    method: String,
    path: String,
    body: Value,
}

type Router = dyn Fn(&Req) -> (u16, String) + Send + Sync;

struct Fake {
    base: String,
    reqs: Arc<Mutex<Vec<Req>>>,
    events: mpsc::Sender<String>,
}

fn base64(input: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in input.chunks(3) {
        let b = [c[0], *c.get(1).unwrap_or(&0), *c.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= c.len() {
                out.push(T[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn serve(router: impl Fn(&Req) -> (u16, String) + Send + Sync + 'static) -> Fake {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let reqs = Arc::new(Mutex::new(Vec::new()));
    let (tx, rx) = mpsc::channel::<String>();
    let rx = Arc::new(Mutex::new(rx));
    let router: Arc<Router> = Arc::new(router);
    let expected_auth = format!(
        "Basic {}",
        base64(format!("opencode:{PASSWORD}").as_bytes())
    );
    {
        let reqs = reqs.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let reqs = reqs.clone();
                let rx = rx.clone();
                let router = router.clone();
                let auth = expected_auth.clone();
                std::thread::spawn(move || handle(stream, &reqs, &rx, &*router, &auth));
            }
        });
    }
    Fake {
        base,
        reqs,
        events: tx,
    }
}

fn handle(
    stream: TcpStream,
    reqs: &Mutex<Vec<Req>>,
    rx: &Mutex<mpsc::Receiver<String>>,
    router: &Router,
    auth: &str,
) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut line = String::new();
    if reader.read_line(&mut line).unwrap_or(0) == 0 {
        return;
    }
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("").to_owned();
    let path = parts.next().unwrap_or("").to_owned();
    let mut len = 0usize;
    let mut authorized = false;
    loop {
        let mut h = String::new();
        if reader.read_line(&mut h).unwrap_or(0) == 0 {
            return;
        }
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            let (k, v) = (k.trim().to_ascii_lowercase(), v.trim());
            if k == "content-length" {
                len = v.parse().unwrap_or(0);
            }
            if k == "authorization" && v == auth {
                authorized = true;
            }
        }
    }
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body).unwrap();
    let mut stream = stream;
    if !authorized {
        let _ = stream.write_all(
            b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        );
        return;
    }
    let body = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let req = Req { method, path, body };
    if req.path == "/event" {
        let _ = stream.write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n",
        );
        let _ = stream.write_all(b": hello\n\n");
        let _ = stream.flush();
        loop {
            let frame = rx.lock().unwrap().recv_timeout(Duration::from_millis(50));
            match frame {
                Ok(data) => {
                    if stream
                        .write_all(format!("data: {data}\n\n").as_bytes())
                        .is_err()
                    {
                        return;
                    }
                    let _ = stream.flush();
                }
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
        }
    }
    reqs.lock().unwrap().push(req.clone());
    let (status, body) = router(&req);
    let resp = format!(
        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(resp.as_bytes());
}

impl Fake {
    fn driver(&self) -> OpenCodeDriver {
        OpenCodeDriver::connect(&self.base, PASSWORD, None).unwrap()
    }

    fn push(&self, v: Value) {
        self.events.send(v.to_string()).unwrap();
    }

    fn find(&self, method: &str, path: &str) -> Option<Req> {
        self.reqs
            .lock()
            .unwrap()
            .iter()
            .find(|r| r.method == method && r.path == path)
            .cloned()
    }
}

fn default_routes(req: &Req) -> (u16, String) {
    match (req.method.as_str(), req.path.as_str()) {
        ("POST", "/session") => (200, r#"{"id":"ses_1","title":"Omni"}"#.into()),
        ("GET", "/session/ses_1") => (200, r#"{"id":"ses_1"}"#.into()),
        ("GET", "/session/ses_gone") => (404, "{}".into()),
        ("POST", p) if p.ends_with("/prompt_async") => (204, String::new()),
        ("POST", p) if p.contains("/permissions/") => (200, "true".into()),
        ("POST", "/session/ses_1/abort") => (200, "true".into()),
        _ => (404, "{}".into()),
    }
}

fn cfg(mode: OmniMode) -> SessionConfig {
    SessionConfig {
        instructions: "You are Omni.".into(),
        mode,
        model: None,
        cwd: "/tmp/omni".into(),
        protected_dir: "/tmp/data".into(),
        mcp: McpLaunch {
            command: "/bin/litecord".into(),
            args: vec!["mcp".into()],
        },
    }
}

async fn next(rx: &mut broadcast::Receiver<HarnessEvent>) -> HarnessEvent {
    tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("event in time")
        .expect("channel open")
}

fn ev(kind: &str, props: Value) -> Value {
    json!({ "type": kind, "properties": props })
}

#[tokio::test]
async fn login_state_signed_out_then_ready() {
    let connected = Arc::new(Mutex::new(false));
    let c = connected.clone();
    let fake = serve(move |req| match req.path.as_str() {
        "/provider" => {
            if *c.lock().unwrap() {
                (
                    200,
                    r#"{"all":[],"default":{},"connected":["anthropic"]}"#.into(),
                )
            } else {
                (200, r#"{"all":[],"default":{},"connected":[]}"#.into())
            }
        }
        _ => (404, "{}".into()),
    });
    let d = fake.driver();
    assert_eq!(d.kind(), HarnessKind::OpenCode);
    assert_eq!(d.login_state().await.unwrap(), LoginState::SignedOut);
    *connected.lock().unwrap() = true;
    assert_eq!(
        d.login_state().await.unwrap(),
        LoginState::Ready {
            account: Some("anthropic".into())
        }
    );
    assert_eq!(
        d.logout().await,
        Err(HarnessError::Unsupported(
            "sign out with `opencode auth logout`"
        ))
    );
    d.shutdown().await;
}

#[tokio::test]
async fn wrong_password_is_rejected() {
    let fake = serve(|_| (200, r#"{"connected":["x"]}"#.into()));
    let d = OpenCodeDriver::connect(&fake.base, "wrong", None).unwrap();
    assert!(matches!(
        d.login_state().await,
        Err(HarnessError::Harness(_))
    ));
    d.shutdown().await;
}

#[tokio::test]
async fn begin_login_prefers_oauth_provider_and_reports_completion() {
    let fake = serve(|req| {
        match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/provider/auth") => (
            200,
            r#"{"openai":[{"type":"api","label":"API key"}],
               "anthropic":[{"type":"api","label":"API key"},{"type":"oauth","label":"Claude Pro/Max"}]}"#
                .into(),
        ),
        ("POST", "/provider/anthropic/oauth/authorize") => (
            200,
            r#"{"url":"https://example.invalid/auth","method":"auto","instructions":"Open the link"}"#
                .into(),
        ),
        ("POST", "/provider/anthropic/oauth/callback") => (200, "true".into()),
        ("GET", "/provider") => (200, r#"{"connected":["anthropic"]}"#.into()),
        _ => (404, "{}".into()),
    }
    });
    let d = fake.driver();
    let mut rx = d.subscribe();
    let state = d.begin_login().await.unwrap();
    assert_eq!(
        state,
        LoginState::SigningIn {
            url: Some("https://example.invalid/auth".into()),
            instructions: Some("Open the link".into()),
            needs_code: false,
        }
    );
    let auth = fake
        .find("POST", "/provider/anthropic/oauth/authorize")
        .unwrap();
    assert_eq!(auth.body, json!({ "method": 1 }));
    assert_eq!(
        next(&mut rx).await,
        HarnessEvent::Login(LoginState::Ready {
            account: Some("anthropic".into())
        })
    );
    let cb = fake
        .find("POST", "/provider/anthropic/oauth/callback")
        .unwrap();
    assert_eq!(cb.body, json!({ "method": 1 }));
    d.shutdown().await;
}

#[tokio::test]
async fn begin_login_without_oauth_is_unsupported() {
    let fake = serve(|req| match req.path.as_str() {
        "/provider/auth" => (
            200,
            r#"{"openai":[{"type":"api","label":"API key"}]}"#.into(),
        ),
        _ => (404, "{}".into()),
    });
    let d = fake.driver();
    assert!(matches!(
        d.begin_login().await,
        Err(HarnessError::Unsupported(_))
    ));
    d.shutdown().await;
}

#[tokio::test]
async fn streaming_turn_with_tool_call() {
    let fake = serve(default_routes);
    let d = fake.driver();
    let mut rx = d.subscribe();
    let id = d.start_session(&cfg(OmniMode::Assistant)).await.unwrap();
    assert_eq!(id, "ses_1");
    assert_eq!(
        fake.find("POST", "/session").unwrap().body,
        json!({ "title": "Omni" })
    );

    d.send(&id, "hi there").await.unwrap();
    let prompt = fake.find("POST", "/session/ses_1/prompt_async").unwrap();
    assert_eq!(prompt.body["agent"], "omni");
    assert_eq!(prompt.body["system"], "You are Omni.");
    assert_eq!(
        prompt.body["parts"],
        json!([{ "type": "text", "text": "hi there" }])
    );
    assert!(prompt.body.get("model").is_none());

    // Our own prompt echoed back, before and after its message.updated.
    fake.push(ev(
        "message.part.updated",
        json!({"part": {"id": "prt_u0", "sessionID": "ses_1", "messageID": "msg_u", "type": "text", "text": "hi there"}}),
    ));
    fake.push(ev(
        "message.updated",
        json!({"info": {"id": "msg_u", "sessionID": "ses_1", "role": "user"}}),
    ));
    fake.push(ev(
        "message.part.updated",
        json!({"part": {"id": "prt_u", "sessionID": "ses_1", "messageID": "msg_u", "type": "text", "text": "hi there!"}}),
    ));
    // Another session's events are ignored.
    fake.push(ev(
        "session.status",
        json!({"sessionID": "ses_other", "status": {"type": "busy"}}),
    ));
    fake.push(ev(
        "session.status",
        json!({"sessionID": "ses_1", "status": {"type": "busy"}}),
    ));
    fake.push(ev(
        "message.updated",
        json!({"info": {"id": "msg_a", "sessionID": "ses_1", "role": "assistant"}}),
    ));
    let part = |text: &str| json!({"id": "prt_a", "sessionID": "ses_1", "messageID": "msg_a", "type": "text", "text": text});
    fake.push(ev("message.part.updated", json!({ "part": part("Hel") })));
    fake.push(ev(
        "message.part.updated",
        json!({ "part": part("Hello, world") }),
    ));
    fake.push(ev(
        "message.part.updated",
        json!({ "part": part("Hello, world!"), "delta": "!" }),
    ));
    fake.push(ev(
        "message.part.updated",
        json!({"part": {"id": "prt_t", "sessionID": "ses_1", "messageID": "msg_a", "type": "tool",
            "tool": "litecord_search", "callID": "c1",
            "state": {"status": "completed", "input": {"query": "SECRET-INPUT"}, "output": "SECRET-OUTPUT", "title": "SECRET-TITLE"}}}),
    ));
    // Repeated completion of the same part is reported once.
    fake.push(ev(
        "message.part.updated",
        json!({"part": {"id": "prt_t", "sessionID": "ses_1", "messageID": "msg_a", "type": "tool",
            "tool": "litecord_search", "state": {"status": "completed"}}}),
    ));
    fake.push(ev(
        "message.part.updated",
        json!({"part": {"id": "prt_k", "sessionID": "ses_1", "messageID": "msg_a", "type": "tool",
            "tool": "task", "state": {"status": "completed", "input": {"prompt": "SECRET"}}}}),
    ));
    fake.push(ev(
        "message.updated",
        json!({"info": {"id": "msg_a", "sessionID": "ses_1", "role": "assistant",
            "tokens": {"input": 10, "output": 5, "reasoning": 0, "cache": {"read": 0, "write": 0}}}}),
    ));
    fake.push(ev("some.unknown.event", json!({"sessionID": "ses_1"})));
    fake.push(ev(
        "session.status",
        json!({"sessionID": "ses_1", "status": {"type": "idle"}}),
    ));
    fake.push(ev("session.idle", json!({"sessionID": "ses_1"})));
    fake.push(ev("session.compacted", json!({"sessionID": "ses_1"})));

    let s = || "ses_1".to_owned();
    let expected = vec![
        HarnessEvent::TurnStarted { session: s() },
        HarnessEvent::Delta {
            session: s(),
            text: "Hel".into(),
        },
        HarnessEvent::Delta {
            session: s(),
            text: "lo, world".into(),
        },
        HarnessEvent::Delta {
            session: s(),
            text: "!".into(),
        },
        HarnessEvent::ItemCompleted {
            session: s(),
            item: TranscriptItem {
                kind: ItemKind::ToolCall,
                text: "litecord_search".into(),
            },
        },
        HarnessEvent::ItemCompleted {
            session: s(),
            item: TranscriptItem {
                kind: ItemKind::Subagent,
                text: "task".into(),
            },
        },
        HarnessEvent::ItemCompleted {
            session: s(),
            item: TranscriptItem {
                kind: ItemKind::Message,
                text: "Hello, world!".into(),
            },
        },
        HarnessEvent::TurnCompleted {
            session: s(),
            usage: Some(TokenUsage {
                input: 10,
                output: 5,
            }),
        },
        // The second idle signal does not complete another turn.
        HarnessEvent::Compacted { session: s() },
    ];
    for want in expected {
        let got = next(&mut rx).await;
        assert!(!format!("{got:?}").contains("SECRET"), "{got:?}");
        assert_eq!(got, want);
    }

    d.interrupt(&id).await.unwrap();
    assert!(fake.find("POST", "/session/ses_1/abort").is_some());
    d.shutdown().await;
}

#[tokio::test]
async fn session_error_fails_turn() {
    let fake = serve(default_routes);
    let d = fake.driver();
    let mut rx = d.subscribe();
    let id = d.start_session(&cfg(OmniMode::Assistant)).await.unwrap();
    d.send(&id, "go").await.unwrap();
    fake.push(ev(
        "session.status",
        json!({"sessionID": "ses_1", "status": {"type": "busy"}}),
    ));
    fake.push(ev(
        "session.error",
        json!({"sessionID": "ses_1", "error": {"name": "ProviderAuthError", "data": {"message": "bad key"}}}),
    ));
    fake.push(ev("session.idle", json!({"sessionID": "ses_1"})));
    fake.push(ev("session.compacted", json!({"sessionID": "ses_1"})));
    assert_eq!(
        next(&mut rx).await,
        HarnessEvent::TurnStarted {
            session: "ses_1".into()
        }
    );
    assert_eq!(
        next(&mut rx).await,
        HarnessEvent::TurnFailed {
            session: "ses_1".into(),
            message: "bad key".into()
        }
    );
    assert_eq!(
        next(&mut rx).await,
        HarnessEvent::Compacted {
            session: "ses_1".into()
        }
    );
    d.shutdown().await;
}

#[tokio::test]
async fn permission_request_and_decline() {
    let fake = serve(default_routes);
    let d = fake.driver();
    let mut rx = d.subscribe();
    let mut c = cfg(OmniMode::Workspace);
    c.model = Some("anthropic/claude-test".into());
    let id = d.start_session(&c).await.unwrap();
    d.send(&id, "list files").await.unwrap();
    let prompt = fake.find("POST", "/session/ses_1/prompt_async").unwrap();
    assert_eq!(prompt.body["agent"], "omni-workspace");
    assert_eq!(
        prompt.body["model"],
        json!({"providerID": "anthropic", "modelID": "claude-test"})
    );

    fake.push(ev(
        "permission.updated",
        json!({"id": "per_1", "sessionID": "ses_1", "type": "bash", "title": "ls -la", "pattern": "ls *",
               "messageID": "msg_a", "metadata": {}, "time": {"created": 1}}),
    ));
    let HarnessEvent::Request(req) = next(&mut rx).await else {
        panic!("expected request");
    };
    assert_eq!(req.id, "per_1");
    assert_eq!(req.session, "ses_1");
    assert_eq!(
        req.request,
        RequestKind::Command {
            command: "ls -la".into(),
            cwd: None
        }
    );
    d.answer("per_1", Decision::Decline).await.unwrap();
    let reply = fake
        .find("POST", "/session/ses_1/permissions/per_1")
        .unwrap();
    assert_eq!(reply.body, json!({ "response": "reject" }));
    // Our own reply is not reported back as resolved elsewhere.
    fake.push(ev(
        "permission.replied",
        json!({"sessionID": "ses_1", "permissionID": "per_1", "response": "reject"}),
    ));
    assert!(d.answer("per_1", Decision::Accept).await.is_err());

    // An edit request resolved by the harness itself.
    fake.push(ev(
        "permission.updated",
        json!({"id": "per_2", "sessionID": "ses_1", "type": "edit", "title": "Edit notes.md"}),
    ));
    fake.push(ev(
        "permission.replied",
        json!({"sessionID": "ses_1", "permissionID": "per_2", "response": "once"}),
    ));
    assert_eq!(
        next(&mut rx).await,
        HarnessEvent::Request(HarnessRequest {
            id: "per_2".into(),
            session: "ses_1".into(),
            request: RequestKind::FileChange {
                summary: "Edit notes.md".into()
            },
            reason: None,
        })
    );
    assert_eq!(
        next(&mut rx).await,
        HarnessEvent::RequestResolved { id: "per_2".into() }
    );
    d.shutdown().await;
}

#[tokio::test]
async fn resume_missing_session_is_unsupported() {
    let fake = serve(default_routes);
    let d = fake.driver();
    assert_eq!(
        d.resume_session("ses_1", &cfg(OmniMode::Assistant))
            .await
            .unwrap(),
        "ses_1"
    );
    assert!(matches!(
        d.resume_session("ses_gone", &cfg(OmniMode::Assistant))
            .await,
        Err(HarnessError::Unsupported(_))
    ));
    d.shutdown().await;
}

#[tokio::test]
async fn compact_without_model_is_unsupported() {
    let fake = serve(|req| match req.path.as_str() {
        "/session" => (200, r#"{"id":"ses_1"}"#.into()),
        "/config" => (200, r#"{"share":"disabled"}"#.into()),
        _ => (404, "{}".into()),
    });
    let d = fake.driver();
    let id = d.start_session(&cfg(OmniMode::Assistant)).await.unwrap();
    assert!(matches!(
        d.compact(&id).await,
        Err(HarnessError::Unsupported(_))
    ));
    d.shutdown().await;
}

#[tokio::test]
async fn compact_uses_config_model() {
    let fake = serve(|req| match req.path.as_str() {
        "/session" => (200, r#"{"id":"ses_1"}"#.into()),
        "/config" => (200, r#"{"model":"openai/gpt-test"}"#.into()),
        "/session/ses_1/summarize" => (200, "true".into()),
        _ => (404, "{}".into()),
    });
    let d = fake.driver();
    let id = d.start_session(&cfg(OmniMode::Assistant)).await.unwrap();
    d.compact(&id).await.unwrap();
    assert_eq!(
        fake.find("POST", "/session/ses_1/summarize").unwrap().body,
        json!({"providerID": "openai", "modelID": "gpt-test"})
    );
    d.shutdown().await;
}

#[tokio::test]
async fn shutdown_emits_exited_once() {
    let fake = serve(default_routes);
    let d = fake.driver();
    let mut rx = d.subscribe();
    d.shutdown().await;
    d.shutdown().await;
    assert_eq!(next(&mut rx).await, HarnessEvent::Exited { message: None });
    assert!(rx.try_recv().is_err());
    assert_eq!(d.login_state().await, Err(HarnessError::NotRunning));
}

const AUTH_METHODS: &str = r#"{
    "zeta":[{"type":"api","label":"API key"}],
    "anthropic":[{"type":"oauth","label":"Claude Pro/Max"},{"type":"api","label":"API key"}],
    "openai":[{"type":"oauth","label":"ChatGPT Plus/Pro"},{"type":"api"},{"type":"weird"}],
    "alpha":[{"type":"oauth","label":"Browser"}]
}"#;

#[tokio::test]
async fn login_options_are_sorted_and_labelled() {
    let fake = serve(|req| {
        match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/provider/auth") => (200, AUTH_METHODS.into()),
        ("GET", "/provider") => (
            200,
            r#"{"all":[{"id":"openai","name":"OpenAI"},{"id":"anthropic","name":"Anthropic"},{"id":"zeta"}],"connected":[]}"#
                .into(),
        ),
        _ => (404, "{}".into()),
    }
    });
    let d = fake.driver();
    let opts = d.login_options().await.unwrap();
    let got: Vec<(&str, &str, LoginKind)> = opts
        .iter()
        .map(|o| (o.id.as_str(), o.label.as_str(), o.kind))
        .collect();
    assert_eq!(
        got,
        [
            ("openai:0", "OpenAI · ChatGPT Plus/Pro", LoginKind::Browser),
            ("openai:1", "OpenAI · API key", LoginKind::ApiKey),
            (
                "anthropic:0",
                "Anthropic · Claude Pro/Max",
                LoginKind::Browser
            ),
            ("anthropic:1", "Anthropic · API key", LoginKind::ApiKey),
            ("alpha:0", "alpha · Browser", LoginKind::Browser),
            ("zeta:0", "zeta · API key", LoginKind::ApiKey),
        ]
    );
    d.shutdown().await;
}

#[tokio::test]
async fn login_options_without_provider_names_use_ids() {
    let fake = serve(|req| match req.path.as_str() {
        "/provider/auth" => (
            200,
            r#"{"anthropic":[{"type":"oauth","label":"Max"}]}"#.into(),
        ),
        _ => (500, "{}".into()),
    });
    let d = fake.driver();
    let opts = d.login_options().await.unwrap();
    assert_eq!(opts.len(), 1);
    assert_eq!(opts[0].label, "anthropic · Max");
    d.shutdown().await;
}

#[tokio::test]
async fn begin_login_with_rejects_bad_options() {
    let fake = serve(|req| match req.path.as_str() {
        "/provider/auth" => (200, AUTH_METHODS.into()),
        _ => (404, "{}".into()),
    });
    let d = fake.driver();
    for bad in ["", "openai", "openai:x", "a/b:0", ":0"] {
        assert!(
            matches!(
                d.begin_login_with(bad).await,
                Err(HarnessError::Protocol(_))
            ),
            "{bad}"
        );
    }
    for unknown in ["nope:0", "openai:1", "openai:9", "zeta:0"] {
        assert!(
            matches!(
                d.begin_login_with(unknown).await,
                Err(HarnessError::Unsupported(_))
            ),
            "{unknown}"
        );
    }
    assert!(fake
        .reqs
        .lock()
        .unwrap()
        .iter()
        .all(|r| !r.path.contains("/oauth/")));
    assert!(matches!(
        d.submit_login_code("123").await,
        Err(HarnessError::Protocol(_))
    ));
    d.shutdown().await;
}

#[tokio::test]
async fn code_sign_in_flow() {
    let fake = serve(|req| {
        match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/provider/auth") => (200, AUTH_METHODS.into()),
        ("POST", "/provider/anthropic/oauth/authorize") => (
            200,
            r#"{"url":"https://example.invalid/code","method":"code","instructions":"Paste the code"}"#
                .into(),
        ),
        ("POST", "/provider/anthropic/oauth/callback") => (200, "true".into()),
        ("GET", "/provider") => (200, r#"{"connected":["openai","anthropic"]}"#.into()),
        _ => (404, "{}".into()),
    }
    });
    let d = fake.driver();
    let mut rx = d.subscribe();
    assert_eq!(
        d.begin_login_with("anthropic:0").await.unwrap(),
        LoginState::SigningIn {
            url: Some("https://example.invalid/code".into()),
            instructions: Some("Paste the code".into()),
            needs_code: true,
        }
    );
    assert_eq!(
        fake.find("POST", "/provider/anthropic/oauth/authorize")
            .unwrap()
            .body,
        json!({ "method": 0 })
    );
    // No background callback for the code method.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(fake
        .find("POST", "/provider/anthropic/oauth/callback")
        .is_none());

    d.submit_login_code(" abc#123 ").await.unwrap();
    assert_eq!(
        fake.find("POST", "/provider/anthropic/oauth/callback")
            .unwrap()
            .body,
        json!({ "method": 0, "code": "abc#123" })
    );
    assert_eq!(
        next(&mut rx).await,
        HarnessEvent::Login(LoginState::Ready {
            account: Some("anthropic".into())
        })
    );
    // The pending sign-in is consumed.
    assert!(matches!(
        d.submit_login_code("again").await,
        Err(HarnessError::Protocol(_))
    ));
    d.shutdown().await;
}

#[tokio::test]
async fn code_sign_in_failure_reports_error() {
    let fake = serve(|req| match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/provider/auth") => (200, AUTH_METHODS.into()),
        ("POST", "/provider/openai/oauth/authorize") => {
            (200, r#"{"url":"https://x.invalid","method":"code"}"#.into())
        }
        ("POST", "/provider/openai/oauth/callback") => (400, "{}".into()),
        _ => (404, "{}".into()),
    });
    let d = fake.driver();
    let mut rx = d.subscribe();
    d.begin_login_with("openai:0").await.unwrap();
    assert!(d.submit_login_code("bad-code").await.is_err());
    let HarnessEvent::Login(LoginState::Error { message }) = next(&mut rx).await else {
        panic!("expected login error");
    };
    assert!(!message.contains("bad-code"), "{message}");
    d.shutdown().await;
}

#[tokio::test]
async fn api_key_sign_in() {
    let fake = serve(|req| match (req.method.as_str(), req.path.as_str()) {
        ("PUT", "/auth/openai") => (200, "true".into()),
        ("PUT", "/auth/zeta") => (400, r#"{"error":"bad key"}"#.into()),
        ("GET", "/provider") => (200, r#"{"connected":["openai"]}"#.into()),
        _ => (404, "{}".into()),
    });
    let d = fake.driver();
    let mut rx = d.subscribe();
    d.login_api_key("openai:1", "sk-SECRET-KEY").await.unwrap();
    assert_eq!(
        fake.find("PUT", "/auth/openai").unwrap().body,
        json!({ "type": "api", "key": "sk-SECRET-KEY" })
    );
    assert_eq!(
        next(&mut rx).await,
        HarnessEvent::Login(LoginState::Ready {
            account: Some("openai".into())
        })
    );
    assert!(!format!("{d:?}").contains("SECRET"));

    let err = d
        .login_api_key("zeta:0", "sk-OTHER-SECRET")
        .await
        .unwrap_err();
    assert!(!err.to_string().contains("SECRET"), "{err}");
    assert!(!format!("{err:?}").contains("SECRET"), "{err:?}");
    assert!(matches!(
        d.login_api_key("a/b:0", "k").await,
        Err(HarnessError::Protocol(_))
    ));
    d.shutdown().await;
}

#[tokio::test]
async fn models_are_listed() {
    let fake = serve(|req| {
        match req.path.as_str() {
        "/config/providers" => (
            200,
            r#"{"providers":[
                {"id":"openai","models":{"gpt-5":{"name":"GPT-5"},"gpt-4o":{}}},
                {"id":"anthropic","models":[{"id":"claude-sonnet-4"},{"id":"claude-sonnet-4"},{"name":"no id"}]},
                {"models":{"orphan":{}}},
                {"id":"empty"}
            ],"default":{"openai":"gpt-5"}}"#
                .into(),
        ),
        _ => (404, "{}".into()),
    }
    });
    let d = fake.driver();
    assert_eq!(
        d.models().await.unwrap(),
        ["anthropic/claude-sonnet-4", "openai/gpt-4o", "openai/gpt-5"]
    );
    d.shutdown().await;

    let broken = serve(|_| (500, "oops".into()));
    let d = broken.driver();
    assert!(d.models().await.is_err());
    d.shutdown().await;
}

#[test]
fn missing_paths_normalizes_params() {
    let mut paths = serde_json::Map::new();
    for p in PROTOCOL_PATHS {
        let p = p
            .replace("/session/{id}", "/session/{sessionID}")
            .replace("/provider/{id}", "/provider/:id");
        if p != "/auth/{id}" && p != "/config/providers" {
            paths.insert(p, json!({}));
        }
    }
    let doc = json!({ "openapi": "3.1.0", "paths": paths });
    assert_eq!(missing_paths(&doc), ["/config/providers", "/auth/{id}"]);
    assert_eq!(missing_paths(&json!({})).len(), PROTOCOL_PATHS.len());
}

#[tokio::test]
async fn doc_check_reads_openapi() {
    let fake = serve(|req| match req.path.as_str() {
        "/doc" => {
            let paths: serde_json::Map<String, Value> = PROTOCOL_PATHS
                .iter()
                .filter(|p| **p != "/event")
                .map(|p| (p.to_string(), json!({})))
                .collect();
            (200, json!({ "paths": paths }).to_string())
        }
        _ => (404, "{}".into()),
    });
    let d = fake.driver();
    assert_eq!(d.doc_check().await.unwrap(), ["/event"]);
    d.shutdown().await;
}
