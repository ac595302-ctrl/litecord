#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Full account-adapter → ingest → reducer → SQLite → view pipeline.
//! Frames and credentials are scripted; these tests do not claim live access.
use discord_adapter::bot::transport::{
    fake::{FakeTransport, SocketHandle},
    SocketEvent,
};
use discord_adapter::user_session::UserSessionBackend;
use litecord_app::LitecordApp;
use litecord_core::clock::SystemClock;
use litecord_core::config::LitecordConfig;
use litecord_core::ports::SocialBackend;
use litecord_core::secrets::{InMemorySecretStore, Secret, SecretKey, SecretStore};
use litecord_store::repos;
use litecord_types::provenance::Origin;
use litecord_types::social::SessionState;
use litecord_types::{ConversationId, MessageId};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;

fn setup() -> (
    Arc<UserSessionBackend>,
    FakeTransport,
    Arc<InMemorySecretStore>,
) {
    let transport = FakeTransport::new();
    transport.respond("GET /users/@me", json!({"id":"1","username":"owner"}));
    transport.respond("GET /users/@me/guilds", json!([]));
    transport.respond("GET /users/@me/relationships", json!([]));
    transport.respond(
        "GET /users/@me/channels",
        json!([{"id":"2","type":1,"recipients":[{"id":"3","username":"friend"}]}]),
    );
    transport.respond("GET /channels/2/messages", json!([]));
    let secrets = Arc::new(InMemorySecretStore::default());
    let backend = Arc::new(UserSessionBackend::new(
        Arc::new(transport.clone()),
        secrets.clone(),
        Arc::new(SystemClock),
    ));
    (backend, transport, secrets)
}
async fn state(app: &LitecordApp, wanted: SessionState) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if app.session_state().unwrap() == wanted {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("session state reached");
}
async fn frame(socket: &SocketHandle, sequence: u64, event: &str, data: Value) {
    socket
        .to_client
        .send(SocketEvent::Text(
            json!({"op":0,"s":sequence,"t":event,"d":data}).to_string(),
        ))
        .await
        .unwrap();
}
async fn ready(socket: &mut SocketHandle) {
    socket
        .to_client
        .send(SocketEvent::Text(
            json!({"op":10,"d":{"heartbeat_interval":45000}}).to_string(),
        ))
        .await
        .unwrap();
    let identify: Value = serde_json::from_str(
        &tokio::time::timeout(Duration::from_secs(5), socket.from_client.recv())
            .await
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(identify["op"], 2);
    assert!(identify["d"].get("intents").is_none());
    frame(socket,1,"READY",json!({"session_id":"test-session","resume_gateway_url":"wss://gateway.discord.gg","user":{"id":"1","username":"owner"},"guilds":[],"private_channels":[{"id":"2","type":1,"recipients":[{"id":"3","username":"friend"}]}],"relationships":[]})).await;
}
fn message(id: &str, content: &str) -> Value {
    json!({"id":id,"channel_id":"2","author":{"id":"3","username":"friend"},"content":content,"timestamp":"2026-09-26T12:00:00Z","attachments":[]})
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_messages_edits_deletes_persist_with_user_source_and_logout() {
    let (backend, transport, secrets) = setup();
    let app = LitecordApp::builder(LitecordConfig::default())
        .backend(backend.clone())
        .in_memory()
        .start()
        .await
        .unwrap();
    state(&app, SessionState::LoggedOut).await;
    let mut socket = transport.next_socket();
    app.authenticate_session(Secret::new("test-owned-credential".into()))
        .await
        .unwrap();
    ready(&mut socket).await;
    state(&app, SessionState::Ready).await;
    frame(
        &socket,
        2,
        "MESSAGE_CREATE",
        message("100", "first message"),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if app
                .database()
                .read(|r| repos::messages::get(r, MessageId(100)))
                .unwrap()
                .is_some()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let view = app.conversation_view(ConversationId(2), 200, None).unwrap();
    assert!(!view.capabilities.can_send);
    let record = app
        .database()
        .read(|r| repos::messages::get(r, MessageId(100)))
        .unwrap()
        .unwrap();
    assert_eq!(record.origin, Origin::DiscordUserSession);
    assert_eq!(record.message.content.as_ref(), "first message");
    let mut edited = message("100", "edited message");
    edited["edited_timestamp"] = json!("2026-09-26T12:01:00Z");
    transport.respond("GET /channels/2/messages/100", edited);
    frame(
        &socket,
        3,
        "MESSAGE_UPDATE",
        json!({"id":"100","channel_id":"2","embeds":[]}),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if app
                .database()
                .read(|r| repos::messages::get(r, MessageId(100)))
                .unwrap()
                .unwrap()
                .message
                .content
                .as_ref()
                == "edited message"
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    frame(
        &socket,
        4,
        "MESSAGE_DELETE",
        json!({"id":"100","channel_id":"2"}),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if app
                .database()
                .read(|r| repos::messages::get(r, MessageId(100)))
                .unwrap()
                .unwrap()
                .deleted
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    app.sign_out().await.unwrap();
    state(&app, SessionState::LoggedOut).await;
    assert!(secrets.get(SecretKey::DiscordUserSessionToken).is_none());
    assert!(backend.current_user().await.is_err());
    assert!(transport
        .requests()
        .iter()
        .all(|r| r.method == discord_adapter::bot::rest::Method::Get));
    app.shutdown().await;
}

#[tokio::test]
async fn source_refuses_bot_credentials_and_database_account_switch() {
    let (backend, transport, _) = setup();
    transport.respond(
        "GET /users/@me",
        json!({"id":"9","username":"bot","bot":true}),
    );
    assert!(backend
        .authenticate_session(Secret::new("test".into()))
        .await
        .is_err());
    transport.respond(
        "GET /users/@me",
        json!({"id":"9","username":"different-owner"}),
    );
    backend.bind_account(litecord_types::UserId(1)).unwrap();
    assert!(backend
        .authenticate_session(Secret::new("test".into()))
        .await
        .is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resumed_session_recovers_a_gap_before_a_later_live_message() {
    let (backend, transport, _) = setup();
    transport.respond(
        "GET /channels/2/messages",
        json!([message("100", "baseline")]),
    );
    let mut cfg = LitecordConfig::default();
    cfg.hydration.history_idle_after_ms = 0;
    cfg.hydration.history_page_interval_ms = 1;
    cfg.runtime.memory_soft_limit_mb = 1 << 20;
    let app = LitecordApp::builder(cfg)
        .backend(backend)
        .in_memory()
        .start()
        .await
        .unwrap();
    state(&app, SessionState::LoggedOut).await;
    let mut socket = transport.next_socket();
    app.authenticate_session(Secret::new("test-owned-credential".into()))
        .await
        .unwrap();
    ready(&mut socket).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let cursor = app
                .database()
                .read(|r| {
                    r.query_row(
                        "SELECT newest_message_id FROM account_catchup WHERE conversation_id=2",
                        [],
                        |r| r.get::<_, Option<i64>>(0),
                    )
                    .map_err(litecord_store::StoreError::from)
                })
                .ok();
            if cursor == Some(Some(100)) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    frame(
        &socket,
        2,
        "MESSAGE_CREATE",
        message("300", "later live message"),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if app
                .database()
                .read(|r| repos::messages::get(r, MessageId(300)))
                .unwrap()
                .is_some()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    transport.respond(
        "GET /channels/2/messages",
        json!([
            message("300", "later live message"),
            message("150", "missed message"),
            message("100", "baseline")
        ]),
    );
    let mut resumed = transport.next_socket();
    socket
        .to_client
        .send(SocketEvent::Closed(None))
        .await
        .unwrap();
    resumed
        .to_client
        .send(SocketEvent::Text(
            json!({"op":10,"d":{"heartbeat_interval":45000}}).to_string(),
        ))
        .await
        .unwrap();
    let resume: Value = serde_json::from_str(
        &tokio::time::timeout(Duration::from_secs(5), resumed.from_client.recv())
            .await
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(resume["op"], 6);
    assert_eq!(resume["d"]["seq"], 2);
    frame(&resumed, 3, "RESUMED", json!({})).await;
    state(&app, SessionState::Ready).await;
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let cursor = app
                .database()
                .read(|r| {
                    r.query_row(
                        "SELECT newest_message_id FROM account_catchup WHERE conversation_id=2",
                        [],
                        |r| r.get::<_, Option<i64>>(0),
                    )
                    .map_err(litecord_store::StoreError::from)
                })
                .unwrap();
            if cursor == Some(300) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(app
        .database()
        .read(|r| repos::messages::get(r, MessageId(150)))
        .unwrap()
        .is_some());
    assert!(
        transport
            .requests()
            .iter()
            .any(|r| r.path.contains("after=100")),
        "catch-up uses verified coverage rather than live message 300"
    );
    app.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn account_message_writes_use_action_engine_and_commit_back_to_canonical_store() {
    let (_, transport, secrets) = setup();
    let backend = Arc::new(UserSessionBackend::with_access(
        Arc::new(transport.clone()),
        secrets,
        Arc::new(SystemClock),
        litecord_types::capability::SessionAccessMode::ReadWrite,
    ));
    let app = LitecordApp::builder(LitecordConfig::default())
        .backend(backend)
        .in_memory()
        .start()
        .await
        .unwrap();
    state(&app, SessionState::LoggedOut).await;
    let mut socket = transport.next_socket();
    app.authenticate_session(Secret::new("test-owned-credential".into()))
        .await
        .unwrap();
    ready(&mut socket).await;
    state(&app, SessionState::Ready).await;
    let mut own = message("800", "outgoing");
    own["author"] = json!({"id":"1","username":"owner"});
    transport.respond("POST /channels/2/messages", own.clone());
    let view = app.conversation_view(ConversationId(2), 100, None).unwrap();
    assert!(view.capabilities.can_send);
    assert_eq!(
        view.capabilities.send_identity,
        Some(litecord_types::provenance::DiscordIdentity::UserSession)
    );
    app.send_message_as(
        ConversationId(2),
        "outgoing",
        litecord_types::provenance::DiscordIdentity::UserSession,
    )
    .await
    .unwrap();
    let stored = app
        .database()
        .read(|r| repos::messages::get(r, MessageId(800)))
        .unwrap()
        .unwrap();
    assert_eq!(stored.origin, Origin::DiscordUserSession);
    assert_eq!(stored.message.content.as_ref(), "outgoing");
    transport.respond("GET /channels/2/messages/800", own.clone());
    own["content"] = json!("edited outgoing");
    own["edited_timestamp"] = json!("2026-09-26T12:02:00Z");
    transport.respond("PATCH /channels/2/messages/800", own.clone());
    app.edit_message(MessageId(800), "edited outgoing")
        .await
        .unwrap();
    assert_eq!(
        app.database()
            .read(|r| repos::messages::get(r, MessageId(800)))
            .unwrap()
            .unwrap()
            .message
            .content
            .as_ref(),
        "edited outgoing"
    );
    let mut reply = own.clone();
    reply["id"] = json!("801");
    reply["message_reference"] = json!({"message_id":"800"});
    transport.respond("POST /channels/2/messages", reply);
    app.send_reply_as(
        ConversationId(2),
        MessageId(800),
        "reply",
        litecord_types::provenance::DiscordIdentity::UserSession,
    )
    .await
    .unwrap();
    transport.respond("DELETE /channels/2/messages/800", Value::Null);
    app.delete_message(MessageId(800)).await.unwrap();
    assert!(
        app.database()
            .read(|r| repos::messages::get(r, MessageId(800)))
            .unwrap()
            .unwrap()
            .deleted
    );
    let requests = transport.requests();
    let send = requests
        .iter()
        .find(|r| r.method == discord_adapter::bot::rest::Method::Post)
        .unwrap();
    assert_eq!(send.body.as_ref().unwrap()["enforce_nonce"], true);
    assert!(send.body.as_ref().unwrap()["nonce"].is_string());
    app.sign_out().await.unwrap();
    assert!(app
        .send_message_as(
            ConversationId(2),
            "after logout",
            litecord_types::provenance::DiscordIdentity::UserSession
        )
        .await
        .is_err());
    app.shutdown().await;
}
