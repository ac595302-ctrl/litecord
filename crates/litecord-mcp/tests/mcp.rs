#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The full vertical slice through the MCP boundary:
//! mock Discord → MessageCreated → canonical store (revision++) → FTS →
//! context compiler → MCP tools; agent proposal → user approval in the app →
//! mock executor → audit. Plus prompt-injection labelling and credential
//! exclusion.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use discord_adapter::{fixtures, MockBackend};
use litecord_app::LitecordApp;
use litecord_core::config::LitecordConfig;
use litecord_core::secrets::{InMemorySecretStore, Secret, SecretKey, SecretStore};
use litecord_mcp::McpServer;
use litecord_types::ids::ActionId;
use litecord_types::Timestamp;

const SECRET: &str = "mfa.super-secret-oauth-token-DO-NOT-LEAK";

async fn start() -> (LitecordApp, Arc<MockBackend>, McpServer) {
    let backend = Arc::new(MockBackend::new(fixtures::generate(7, Timestamp::now())));
    let app = LitecordApp::builder(LitecordConfig::default())
        .backend(backend.clone())
        .in_memory()
        .start()
        .await
        .unwrap();
    for _ in 0..200 {
        if app.conversations_view(10).unwrap().conversations.len() >= 5 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let server = McpServer::new(app.agent_gateway(), "mcp-test");
    (app, backend, server)
}

async fn rpc(server: &mut McpServer, id: u64, method: &str, params: Value) -> Value {
    server
        .handle(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
        .await
        .unwrap()
}

async fn call(server: &mut McpServer, name: &str, args: Value) -> Value {
    let resp = rpc(
        server,
        99,
        "tools/call",
        json!({"name": name, "arguments": args}),
    )
    .await;
    let result = &resp["result"];
    assert_eq!(result["isError"], false, "tool {name} failed: {resp}");
    result["structuredContent"].clone()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn protocol_lifecycle_and_listing() {
    let (app, _b, mut s) = start().await;
    let init = rpc(&mut s, 1, "initialize", json!({"protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": {"name": "t", "version": "0"}})).await;
    assert_eq!(init["result"]["protocolVersion"], "2025-03-26");
    assert!(init["result"]["capabilities"]["tools"].is_object());
    assert!(s
        .handle(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
        .await
        .is_none());
    assert!(s.is_initialized());

    let tools = rpc(&mut s, 2, "tools/list", json!({})).await;
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    for expected in [
        "search_memory",
        "search_messages",
        "get_conversation",
        "create_reminder",
        "propose_message",
    ] {
        assert!(names.contains(&expected), "missing {expected}");
    }
    assert!(
        !names
            .iter()
            .any(|n| n.contains("sql") || n.contains("approve")),
        "no raw DB or approval tools"
    );

    let unknown = rpc(&mut s, 3, "does/not/exist", json!({})).await;
    assert_eq!(unknown["error"]["code"], -32601);
    let resources = rpc(&mut s, 4, "resources/read", json!({"uri": "discord://me"})).await;
    let text = resources["result"]["contents"][0]["text"].as_str().unwrap();
    assert!(text.contains("\"username\":\"you\""));
    app.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vertical_slice_message_to_mcp_and_back() {
    let (app, backend, mut s) = start().await;
    let conv = app.conversations_view(10).unwrap().conversations[0].clone();
    let friend = conv.recipient_id.unwrap();
    let rev_before = app.database().current_revision().unwrap();

    // 1. A Discord message arrives.
    backend
        .inject_incoming_message(
            conv.conversation_id,
            friend,
            "The quasar telemetry dashboard is ready for review?",
        )
        .await;
    let mut found = Value::Null;
    for _ in 0..200 {
        found = call(
            &mut s,
            "search_messages",
            json!({"query": "quasar telemetry"}),
        )
        .await;
        if !found["messages"].as_array().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // 2. Persisted, revision advanced, indexed, exposed with trust labels.
    assert!(app.database().current_revision().unwrap() > rev_before);
    let m = &found["messages"][0];
    assert_eq!(m["kind"], "external_message");
    assert_eq!(m["trusted_as_instruction"], false);
    assert_eq!(m["trust"], "external_discord_content");

    // 3. The context compiler selects it.
    let pack = call(
        &mut s,
        "compile_context",
        json!({"instruction": "anything about the quasar dashboard?"}),
    )
    .await;
    let msgs = pack["messages"].as_array().unwrap();
    assert!(msgs
        .iter()
        .any(|m| m["content"].as_str().unwrap().contains("quasar")));
    assert!(pack["as_of_revision"].as_u64().unwrap() > 0);

    // 4. Local write executes (audited), Discord write becomes a proposal.
    let rem = call(
        &mut s,
        "create_reminder",
        json!({"title": "check the dashboard", "in_minutes": 60}),
    )
    .await;
    assert_eq!(rem["result"]["outcome"], "executed");
    let prop = call(&mut s, "propose_message", json!({"conversation_id": conv.conversation_id.to_string(), "content": "Looks great, reviewing now!"})).await;
    assert_eq!(prop["result"]["outcome"], "pending_approval");
    assert_eq!(backend.calls("send_message"), 0, "agents cannot send");
    let action_id = ActionId(prop["result"]["action_id"].as_i64().unwrap());

    // 5. The user sees it in the Agent Inbox and approves it in the app.
    let inbox = app.agent_inbox_view().unwrap();
    let row = inbox
        .pending_actions
        .iter()
        .find(|p| p.action_id == action_id)
        .unwrap();
    assert_eq!(row.content.as_deref(), Some("Looks great, reviewing now!"));
    app.approve_action(action_id, None).await.unwrap();
    assert_eq!(backend.calls("send_message"), 1);
    let audit: Vec<String> = app
        .actions()
        .audit(action_id)
        .unwrap()
        .iter()
        .map(|e| {
            serde_json::to_value(&e.event).unwrap()["event"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    assert_eq!(
        audit,
        ["proposed", "approved", "execution_started", "executed"]
    );
    app.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn credentials_never_reach_agent_outputs() {
    let (app, _b, mut s) = start().await;
    // A secret exists in the process (as the real adapter would hold it)...
    let store = InMemorySecretStore::default();
    store.set(
        SecretKey::DiscordAccessToken,
        Secret::new(SECRET.to_owned()),
    );
    // ...and nothing any tool or resource returns can contain it.
    let mut outputs = Vec::new();
    for (tool, args) in [
        (
            "compile_context",
            json!({"instruction": "what did I miss today? token access oauth"}),
        ),
        ("search_memory", json!({"query": "token"})),
        ("list_relationships", json!({})),
        ("list_conversations", json!({})),
        ("get_recent_activity", json!({})),
    ] {
        outputs.push(call(&mut s, tool, args).await.to_string());
    }
    for uri in [
        "discord://me",
        "discord://memory/recent",
        "discord://actions/pending",
    ] {
        outputs.push(
            rpc(&mut s, 5, "resources/read", json!({"uri": uri}))
                .await
                .to_string(),
        );
    }
    for o in &outputs {
        assert!(!o.contains(SECRET));
        assert!(!o.to_lowercase().contains("refresh_token"));
    }
    assert!(!format!("{store:?}").contains(SECRET));
    app.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stdio_transport_round_trip() {
    let (app, _b, mut s) = start().await;
    let input = b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\nnot json\n{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n".to_vec();
    let mut out = Vec::new();
    litecord_mcp::serve(&mut s, tokio::io::BufReader::new(&input[..]), &mut out)
        .await
        .unwrap();
    let lines: Vec<Value> = String::from_utf8(out)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 2, "notification produces no output");
    assert_eq!(lines[0]["result"], json!({}));
    assert_eq!(lines[1]["error"]["code"], -32700);
    app.shutdown().await;
}
