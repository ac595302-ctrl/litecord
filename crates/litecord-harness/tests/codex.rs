#![cfg(feature = "codex")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::time::Duration;

use litecord_harness::codex::CodexDriver;
use litecord_harness::*;
use serde_json::{json, Value};
use tokio::io::{
    AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, Lines, ReadHalf, WriteHalf,
};
use tokio::sync::broadcast;

struct Server {
    lines: Lines<BufReader<ReadHalf<DuplexStream>>>,
    w: WriteHalf<DuplexStream>,
}

impl Server {
    async fn recv(&mut self) -> Value {
        let line = tokio::time::timeout(Duration::from_secs(5), self.lines.next_line())
            .await
            .expect("timed out waiting for client")
            .unwrap()
            .expect("client closed");
        serde_json::from_str(&line).unwrap()
    }

    async fn expect(&mut self, method: &str) -> Value {
        let msg = self.recv().await;
        assert_eq!(msg["method"], method, "unexpected message {msg}");
        msg
    }

    async fn send(&mut self, v: Value) {
        let mut line = v.to_string();
        line.push('\n');
        self.w.write_all(line.as_bytes()).await.unwrap();
        self.w.flush().await.unwrap();
    }

    async fn reply(&mut self, req: &Value, result: Value) {
        self.send(json!({ "id": req["id"], "result": result }))
            .await;
    }
}

async fn setup() -> (CodexDriver, Server) {
    let (client, server) = tokio::io::duplex(1 << 16);
    let (cr, cw) = tokio::io::split(client);
    let (sr, sw) = tokio::io::split(server);
    let mut srv = Server {
        lines: BufReader::new(sr).lines(),
        w: sw,
    };
    let (driver, ()) = tokio::join!(CodexDriver::connect(cr, cw), async {
        let init = srv.expect("initialize").await;
        assert_eq!(init["params"]["clientInfo"]["name"], "litecord");
        srv.reply(&init, json!({ "userAgent": "codex" })).await;
        let n = srv.expect("initialized").await;
        assert!(n.get("id").is_none());
    });
    (driver.unwrap(), srv)
}

async fn next(rx: &mut broadcast::Receiver<HarnessEvent>) -> HarnessEvent {
    tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timed out waiting for event")
        .unwrap()
}

fn cfg(mode: OmniMode) -> SessionConfig {
    SessionConfig {
        instructions: "be Omni".into(),
        mode,
        model: None,
        cwd: PathBuf::from("/tmp/omni-workspace"),
        protected_dir: PathBuf::from("/tmp/data"),
        mcp: McpLaunch {
            command: PathBuf::from("/usr/bin/litecord"),
            args: vec!["mcp".into()],
        },
    }
}

#[tokio::test]
async fn login_flow() {
    let (driver, mut srv) = setup().await;
    let mut rx = driver.subscribe();

    let (state, ()) = tokio::join!(driver.login_state(), async {
        let req = srv.expect("account/read").await;
        srv.reply(&req, json!({ "account": null })).await;
    });
    assert_eq!(state.unwrap(), LoginState::SignedOut);

    let (state, ()) = tokio::join!(driver.begin_login(), async {
        let req = srv.expect("account/login/start").await;
        assert_eq!(req["params"]["type"], "chatgpt");
        srv.reply(
            &req,
            json!({ "loginId": "l1", "authUrl": "https://auth.example/x" }),
        )
        .await;
    });
    assert_eq!(
        state.unwrap(),
        LoginState::SigningIn {
            url: Some("https://auth.example/x".into()),
            instructions: None,
            needs_code: false,
        }
    );

    srv.send(json!({ "method": "account/login/completed", "params": { "loginId": "l1", "success": true } }))
        .await;
    let req = srv.expect("account/read").await;
    srv.reply(
        &req,
        json!({ "account": { "type": "chatgpt", "email": "me@x.y", "planType": "pro" } }),
    )
    .await;
    assert_eq!(
        next(&mut rx).await,
        HarnessEvent::Login(LoginState::Ready {
            account: Some("ChatGPT Pro".into())
        })
    );
}

#[tokio::test]
async fn session_turn_approval_interrupt_and_exit() {
    let (driver, mut srv) = setup().await;
    let mut rx = driver.subscribe();

    // Start a session in Assistant mode.
    let config = cfg(OmniMode::Assistant);
    let (id, ()) = tokio::join!(driver.start_session(&config), async {
        let req = srv.expect("thread/start").await;
        let p = &req["params"];
        assert_eq!(p["developerInstructions"], "be Omni");
        assert_eq!(p["sandbox"], "read-only");
        assert_eq!(p["approvalPolicy"], "untrusted");
        assert_eq!(p["cwd"], "/tmp/omni-workspace");
        assert!(p.get("model").is_none());
        srv.reply(&req, json!({ "thread": { "id": "th1" } })).await;
    });
    let id = id.unwrap();
    assert_eq!(id, "th1");

    // Send a message and stream a reply.
    let (r, ()) = tokio::join!(driver.send(&id, "hello"), async {
        let req = srv.expect("turn/start").await;
        assert_eq!(req["params"]["threadId"], "th1");
        assert_eq!(req["params"]["input"][0]["text"], "hello");
        srv.reply(
            &req,
            json!({ "turn": { "id": "tu1", "status": "inProgress" } }),
        )
        .await;
    });
    r.unwrap();
    for n in [
        json!({"method":"turn/started","params":{"threadId":"th1","turn":{"id":"tu1"}}}),
        json!({"method":"item/agentMessage/delta","params":{"threadId":"th1","turnId":"tu1","itemId":"i1","delta":"Hi "}}),
        json!({"method":"item/agentMessage/delta","params":{"threadId":"th1","turnId":"tu1","itemId":"i1","delta":"there"}}),
        json!({"method":"item/completed","params":{"threadId":"th1","item":{"type":"reasoning","text":"secret"}}}),
        json!({"method":"item/completed","params":{"threadId":"th1","item":{"type":"mcpToolCall","server":"litecord","tool":"search","arguments":{"q":"x"}}}}),
        json!({"method":"item/completed","params":{"threadId":"th1","item":{"type":"agentMessage","id":"i1","text":"Hi there"}}}),
        json!({"method":"some/unknown","params":{"threadId":"th1"}}),
        json!({"method":"thread/tokenUsage/updated","params":{"threadId":"th1","tokenUsage":{"last":{"inputTokens":12,"outputTokens":3},"total":{"inputTokens":99,"outputTokens":9}}}}),
    ] {
        srv.send(n).await;
    }

    // Interrupt while the turn is active: sends the current turn id.
    let (r, ()) = tokio::join!(driver.interrupt("th1"), async {
        let req = srv.expect("turn/interrupt").await;
        assert_eq!(req["params"]["threadId"], "th1");
        assert_eq!(req["params"]["turnId"], "tu1");
        srv.reply(&req, json!({})).await;
    });
    r.unwrap();

    srv.send(json!({"method":"turn/completed","params":{"threadId":"th1","turn":{"id":"tu1","status":"completed"}}}))
        .await;

    let s = || "th1".to_string();
    assert_eq!(
        next(&mut rx).await,
        HarnessEvent::TurnStarted { session: s() }
    );
    assert_eq!(
        next(&mut rx).await,
        HarnessEvent::Delta {
            session: s(),
            text: "Hi ".into()
        }
    );
    assert_eq!(
        next(&mut rx).await,
        HarnessEvent::Delta {
            session: s(),
            text: "there".into()
        }
    );
    assert_eq!(
        next(&mut rx).await,
        HarnessEvent::ItemCompleted {
            session: s(),
            item: TranscriptItem {
                kind: ItemKind::ToolCall,
                text: "litecord.search".into()
            }
        }
    );
    assert_eq!(
        next(&mut rx).await,
        HarnessEvent::ItemCompleted {
            session: s(),
            item: TranscriptItem {
                kind: ItemKind::Message,
                text: "Hi there".into()
            }
        }
    );
    assert_eq!(
        next(&mut rx).await,
        HarnessEvent::TurnCompleted {
            session: s(),
            usage: Some(TokenUsage {
                input: 12,
                output: 3
            })
        }
    );

    // Command approval -> Request event -> decline.
    srv.send(json!({"id": 77, "method": "item/commandExecution/requestApproval", "params": {
        "threadId":"th1","turnId":"tu2","itemId":"c1","command":"rm -rf /","cwd":"/tmp","reason":"cleanup"}}))
        .await;
    let HarnessEvent::Request(req) = next(&mut rx).await else {
        panic!("expected request");
    };
    assert_eq!(req.session, "th1");
    assert_eq!(req.reason.as_deref(), Some("cleanup"));
    assert_eq!(
        req.request,
        RequestKind::Command {
            command: "rm -rf /".into(),
            cwd: Some("/tmp".into())
        }
    );
    driver.answer(&req.id, Decision::Decline).await.unwrap();
    let reply = srv.recv().await;
    assert_eq!(reply["id"], 77);
    assert_eq!(reply["result"]["decision"], "decline");
    assert!(driver.answer(&req.id, Decision::Accept).await.is_err());

    // Unknown server request -> -32601.
    srv.send(
        json!({"id": "x9", "method": "item/tool/requestUserInput", "params": {"threadId":"th1"}}),
    )
    .await;
    let reply = srv.recv().await;
    assert_eq!(reply["id"], "x9");
    assert_eq!(reply["error"]["code"], -32601);

    // Compaction unsupported by method-not-found.
    let (r, ()) = tokio::join!(driver.compact("th1"), async {
        let req = srv.expect("thread/compact/start").await;
        srv.send(json!({"id": req["id"], "error": {"code": -32601, "message": "nope"}}))
            .await;
    });
    assert!(matches!(r, Err(HarnessError::Unsupported(_))));

    // EOF -> Exited once; later calls fail with NotRunning.
    drop(srv);
    assert!(matches!(next(&mut rx).await, HarnessEvent::Exited { .. }));
    assert_eq!(driver.login_state().await, Err(HarnessError::NotRunning));
    driver.shutdown().await;
    assert!(matches!(
        rx.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));
}

#[tokio::test]
async fn failed_turn_and_pending_request_on_eof() {
    let (driver, mut srv) = setup().await;
    let mut rx = driver.subscribe();
    srv.send(json!({"method":"turn/completed","params":{"threadId":"t","turn":{"id":"u","status":"failed","error":{"message":"quota"}}}}))
        .await;
    assert_eq!(
        next(&mut rx).await,
        HarnessEvent::TurnFailed {
            session: "t".into(),
            message: "quota".into()
        }
    );
    let (r, ()) = tokio::join!(driver.login_state(), async {
        srv.expect("account/read").await;
        drop(srv);
    });
    assert_eq!(r, Err(HarnessError::NotRunning));
}

#[tokio::test]
async fn login_options_and_unknown_option() {
    let (driver, _srv) = setup().await;
    let opts = driver.login_options().await.unwrap();
    assert_eq!(
        opts,
        [
            LoginOption {
                id: "chatgpt".into(),
                label: "ChatGPT account".into(),
                kind: LoginKind::Browser,
            },
            LoginOption {
                id: "api_key".into(),
                label: "OpenAI API key".into(),
                kind: LoginKind::ApiKey,
            },
        ]
    );
    assert!(matches!(
        driver.begin_login_with("api_key").await,
        Err(HarnessError::Unsupported(_))
    ));
}

#[tokio::test]
async fn api_key_login_success_and_failure_redacts_key() {
    let (driver, mut srv) = setup().await;
    let mut rx = driver.subscribe();
    const KEY: &str = "sk-test-SECRET-123";

    let (r, ()) = tokio::join!(driver.login_api_key("api_key", KEY), async {
        let req = srv.expect("account/login/start").await;
        assert_eq!(req["params"]["type"], "apiKey");
        assert_eq!(req["params"]["apiKey"], KEY);
        srv.reply(&req, json!({})).await;
        let req = srv.expect("account/read").await;
        srv.reply(&req, json!({ "account": { "type": "apiKey" } }))
            .await;
    });
    r.unwrap();
    assert_eq!(
        next(&mut rx).await,
        HarnessEvent::Login(LoginState::Ready {
            account: Some("API key".into())
        })
    );

    let (r, ()) = tokio::join!(driver.login_api_key("api_key", KEY), async {
        let req = srv.expect("account/login/start").await;
        assert_eq!(req["params"]["apiKey"], KEY);
        srv.send(json!({"id": req["id"], "error": {"code": -32000, "message": format!("invalid key {KEY}")}}))
            .await;
    });
    let err = r.unwrap_err();
    let text = format!("{err} {err:?}");
    assert!(!text.contains(KEY), "key leaked: {text}");
    assert!(text.contains("invalid key"));
}

#[tokio::test]
async fn models_list_and_method_not_found() {
    let (driver, mut srv) = setup().await;

    let (r, ()) = tokio::join!(driver.models(), async {
        let req = srv.expect("model/list").await;
        assert_eq!(req["params"], json!({}));
        srv.reply(
            &req,
            json!({ "data": [{ "id": "gpt-5-codex" }, { "model": "o4-mini" }] }),
        )
        .await;
    });
    assert_eq!(r.unwrap(), ["gpt-5-codex", "o4-mini"]);

    let (r, ()) = tokio::join!(driver.models(), async {
        let req = srv.expect("model/list").await;
        srv.send(json!({"id": req["id"], "error": {"code": -32601, "message": "nope"}}))
            .await;
    });
    assert!(r.unwrap_err().to_string().contains("could not list models"));
}

#[test]
fn protocol_methods_listed() {
    let m = litecord_harness::codex::PROTOCOL_METHODS;
    for name in [
        "initialize",
        "account/login/start",
        "model/list",
        "turn/completed",
        "item/fileChange/requestApproval",
    ] {
        assert!(m.contains(&name), "{name}");
    }
}
