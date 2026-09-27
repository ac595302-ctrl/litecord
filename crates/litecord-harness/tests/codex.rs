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
            account: Some("ChatGPT Pro (me@x.y)".into())
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
    let got: Vec<(&str, &str, LoginKind, bool)> = opts
        .iter()
        .map(|o| (o.id.as_str(), o.label.as_str(), o.kind, o.featured))
        .collect();
    assert_eq!(
        got,
        [
            ("chatgpt", "Sign in with ChatGPT", LoginKind::Browser, true),
            ("api_key", "OpenAI API key", LoginKind::ApiKey, true),
            (
                "chatgpt_device_code",
                "Sign in with ChatGPT using a device code",
                LoginKind::Browser,
                false
            ),
        ]
    );
    for o in &opts {
        assert_eq!(o.provider.as_deref(), Some("openai"));
        assert_eq!(o.provider_label.as_deref(), Some("OpenAI"));
        assert_eq!(o.method_label, o.label);
        assert!(!o.connected);
        assert!(o.prompts.is_empty());
    }
    assert!(matches!(
        driver.begin_login_with("api_key").await,
        Err(HarnessError::Unsupported(_))
    ));
    assert!(matches!(
        driver.login_api_key("chatgpt", "sk-x").await,
        Err(HarnessError::Unsupported(_))
    ));
}

/// A browser sign-in that is abandoned and restarted: Codex reports the
/// first attempt as cancelled after the second one started. That late
/// failure must not turn the new attempt into an error.
#[tokio::test]
async fn superseded_browser_login_is_ignored() {
    let (driver, mut srv) = setup().await;
    let mut rx = driver.subscribe();
    for id in ["l1", "l2"] {
        let (state, ()) = tokio::join!(driver.begin_login_with("chatgpt"), async {
            let req = srv.expect("account/login/start").await;
            assert_eq!(req["params"], json!({ "type": "chatgpt" }));
            if id == "l2" {
                // As codex-cli 0.157.1 does: the old attempt is reported
                // cancelled before the new request is answered.
                srv.send(json!({ "method": "account/login/completed", "params": {
                    "loginId": "l1", "success": false, "error": "Login server error: Login cancelled" } }))
                    .await;
            }
            srv.reply(
                &req,
                json!({ "type": "chatgpt", "loginId": id, "authUrl": format!("https://auth.example/{id}") }),
            )
            .await;
        });
        assert!(matches!(
            state.unwrap(),
            LoginState::SigningIn { url: Some(_), .. }
        ));
    }
    // A late duplicate for the old attempt is ignored too.
    srv.send(json!({ "method": "account/login/completed", "params": {
        "loginId": "l1", "success": false, "error": "Login server error: Login cancelled" } }))
        .await;
    srv.send(json!({ "method": "account/login/completed", "params": {
        "loginId": "l2", "success": false, "error": "Login server error: port busy" } }))
        .await;
    assert_eq!(
        next(&mut rx).await,
        HarnessEvent::Login(LoginState::Error {
            message: "Login server error: port busy".into()
        })
    );
    // A retry after the failure still works and completes.
    let (state, ()) = tokio::join!(driver.begin_login(), async {
        let req = srv.expect("account/login/start").await;
        srv.reply(
            &req,
            json!({ "type": "chatgpt", "loginId": "l3", "authUrl": "https://auth.example/l3" }),
        )
        .await;
    });
    state.unwrap();
    srv.send(json!({ "method": "account/login/completed", "params": { "loginId": "l3", "success": true, "error": null } }))
        .await;
    let req = srv.expect("account/read").await;
    srv.reply(
        &req,
        json!({ "account": { "type": "chatgpt", "email": null, "planType": "plus" }, "requiresOpenaiAuth": true }),
    )
    .await;
    assert_eq!(
        next(&mut rx).await,
        HarnessEvent::Login(LoginState::Ready {
            account: Some("ChatGPT Plus".into())
        })
    );
}

#[tokio::test]
async fn cancel_login_frees_the_pending_attempt() {
    let (driver, mut srv) = setup().await;
    let mut rx = driver.subscribe();
    // Nothing pending: no request is sent.
    driver.cancel_login().await.unwrap();
    let (state, ()) = tokio::join!(driver.begin_login(), async {
        let req = srv.expect("account/login/start").await;
        srv.reply(
            &req,
            json!({ "type": "chatgpt", "loginId": "l9", "authUrl": "https://auth.example/l9" }),
        )
        .await;
    });
    state.unwrap();
    let (r, ()) = tokio::join!(driver.cancel_login(), async {
        let req = srv.expect("account/login/cancel").await;
        assert_eq!(req["params"], json!({ "loginId": "l9" }));
        srv.reply(&req, json!({ "status": "canceled" })).await;
    });
    r.unwrap();
    // Codex then reports the cancelled attempt; it is not an error.
    srv.send(json!({ "method": "account/login/completed", "params": {
        "loginId": "l9", "success": false, "error": "Login server error: Login cancelled" } }))
        .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(matches!(
        rx.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));
}

#[tokio::test]
async fn device_code_login() {
    let (driver, mut srv) = setup().await;
    let (state, ()) = tokio::join!(driver.begin_login_with("chatgpt_device_code"), async {
        let req = srv.expect("account/login/start").await;
        assert_eq!(req["params"], json!({ "type": "chatgptDeviceCode" }));
        srv.reply(
            &req,
            json!({ "type": "chatgptDeviceCode", "loginId": "d1",
                    "verificationUrl": "https://auth.example/device", "userCode": "ABCD-1234" }),
        )
        .await;
    });
    let LoginState::SigningIn {
        url,
        instructions,
        needs_code,
    } = state.unwrap()
    else {
        panic!("expected SigningIn");
    };
    assert_eq!(url.as_deref(), Some("https://auth.example/device"));
    assert!(instructions.unwrap().contains("ABCD-1234"));
    assert!(!needs_code);
}

#[tokio::test]
async fn provider_without_openai_auth_is_ready() {
    let (driver, mut srv) = setup().await;
    let (state, ()) = tokio::join!(driver.login_state(), async {
        let req = srv.expect("account/read").await;
        srv.reply(
            &req,
            json!({ "account": null, "requiresOpenaiAuth": false }),
        )
        .await;
    });
    assert!(state.unwrap().is_ready());
}

#[tokio::test]
async fn api_key_login_success_and_failure_redacts_key() {
    let (driver, mut srv) = setup().await;
    let mut rx = driver.subscribe();
    const KEY: &str = "sk-test-SECRET-123";

    // A browser sign-in left open is cancelled first (it holds a port).
    let (state, ()) = tokio::join!(driver.begin_login(), async {
        let req = srv.expect("account/login/start").await;
        srv.reply(
            &req,
            json!({ "type": "chatgpt", "loginId": "b1", "authUrl": "https://auth.example/b1" }),
        )
        .await;
    });
    state.unwrap();

    let padded = format!(" {KEY}\n");
    let (r, ()) = tokio::join!(driver.login_api_key("api_key", &padded), async {
        let req = srv.expect("account/login/cancel").await;
        assert_eq!(req["params"]["loginId"], "b1");
        srv.reply(&req, json!({ "status": "canceled" })).await;
        let req = srv.expect("account/login/start").await;
        assert_eq!(req["params"]["type"], "apiKey");
        assert_eq!(req["params"]["apiKey"], KEY, "trimmed");
        srv.reply(&req, json!({ "type": "apiKey" })).await;
        let req = srv.expect("account/read").await;
        srv.reply(
            &req,
            json!({ "account": { "type": "apiKey" }, "requiresOpenaiAuth": true }),
        )
        .await;
    });
    r.unwrap();
    assert_eq!(
        next(&mut rx).await,
        HarnessEvent::Login(LoginState::Ready {
            account: Some("OpenAI API key".into())
        })
    );
    // Now the options report the account as connected.
    assert!(driver
        .login_options()
        .await
        .unwrap()
        .iter()
        .all(|o| o.connected));

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

    assert!(matches!(
        driver.login_api_key("api_key", "   ").await,
        Err(HarnessError::Protocol(_))
    ));
}

#[tokio::test]
async fn logout_reports_the_new_state() {
    let (driver, mut srv) = setup().await;
    let mut rx = driver.subscribe();
    let (r, ()) = tokio::join!(driver.logout(), async {
        let req = srv.expect("account/logout").await;
        srv.reply(&req, json!({})).await;
        let req = srv.expect("account/read").await;
        srv.reply(&req, json!({ "account": null, "requiresOpenaiAuth": true }))
            .await;
    });
    r.unwrap();
    assert_eq!(
        next(&mut rx).await,
        HarnessEvent::Login(LoginState::SignedOut)
    );
}

#[tokio::test]
async fn models_list_pages_and_method_not_found() {
    let (driver, mut srv) = setup().await;

    let (r, ()) = tokio::join!(driver.models(), async {
        let req = srv.expect("model/list").await;
        assert_eq!(req["params"], json!({}));
        srv.reply(
            &req,
            json!({ "data": [{ "id": "model-a" }, { "model": "model-b" }, { "id": "old", "hidden": true }],
                    "nextCursor": "p2" }),
        )
        .await;
        let req = srv.expect("model/list").await;
        assert_eq!(req["params"], json!({ "cursor": "p2" }));
        srv.reply(
            &req,
            json!({ "data": [{ "id": "model-c" }, { "id": "model-a" }], "nextCursor": null }),
        )
        .await;
    });
    assert_eq!(r.unwrap(), ["model-a", "model-b", "model-c"]);

    let (r, ()) = tokio::join!(driver.models(), async {
        let req = srv.expect("model/list").await;
        srv.send(json!({"id": req["id"], "error": {"code": -32601, "message": "nope"}}))
            .await;
    });
    assert_eq!(r.unwrap(), Vec::<String>::new());
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
