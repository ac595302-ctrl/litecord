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
    assert_eq!(app.account_view().unwrap().user_id, None);
    assert_eq!(app.account_view().unwrap().display_name, "Not signed in");
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn uncertain_send_is_not_retried_and_exact_gateway_nonce_reconciles_it() {
    use litecord_types::actions::OutboundState;
    use litecord_types::provenance::DiscordIdentity;
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
    let mut socket = transport.next_socket();
    app.authenticate_session(Secret::new("dummy".into()))
        .await
        .unwrap();
    ready(&mut socket).await;
    state(&app, SessionState::Ready).await;
    assert_eq!(app.diagnostics_view().unwrap().session, SessionState::Ready);
    transport.respond_error(
        "POST /channels/2/messages",
        litecord_core::ports::BackendError::DeliveryUncertain("response lost".into()),
    );
    assert!(app
        .send_message_as(
            ConversationId(2),
            "one transmission",
            DiscordIdentity::UserSession
        )
        .await
        .is_err());
    let operations = app
        .database()
        .read(|r| repos::outbound::recent(r, 10))
        .unwrap();
    assert_eq!(operations.len(), 1);
    assert_eq!(operations[0].state, OutboundState::Uncertain);
    let sends: Vec<_> = transport
        .requests()
        .into_iter()
        .filter(|r| r.method == discord_adapter::bot::rest::Method::Post)
        .collect();
    assert_eq!(sends.len(), 1);
    let nonce = sends[0].body.as_ref().unwrap()["nonce"].clone();
    let mut receipt = message("850", "one transmission");
    receipt["author"] = json!({"id":"1","username":"owner"});
    receipt["nonce"] = json!("wrong nonce");
    frame(&socket, 2, "MESSAGE_CREATE", receipt.clone()).await;
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(
        app.database()
            .read(|r| repos::outbound::recent(r, 10))
            .unwrap()[0]
            .state,
        OutboundState::Uncertain
    );
    receipt["nonce"] = nonce;
    frame(&socket, 3, "MESSAGE_CREATE", receipt).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if app
                .database()
                .read(|r| repos::outbound::recent(r, 10))
                .unwrap()[0]
                .state
                == OutboundState::Confirmed
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        app.database()
            .read(|r| repos::actions::get(r, operations[0].action_id))
            .unwrap()
            .unwrap()
            .status,
        litecord_types::actions::ActionStatus::Executed
    );
    assert_eq!(
        transport
            .requests()
            .iter()
            .filter(|r| r.method == discord_adapter::bot::rest::Method::Post)
            .count(),
        1
    );
    app.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn account_presence_and_relationship_actions_round_trip_through_engine() {
    use litecord_types::actions::{PresenceDraft, RelationshipAction};
    use litecord_types::social::{PresenceStatus, RelationshipKind};
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
    let mut socket = transport.next_socket();
    app.authenticate_session(Secret::new("dummy".into()))
        .await
        .unwrap();
    ready(&mut socket).await;
    state(&app, SessionState::Ready).await;
    let proposal = app
        .agent_gateway()
        .call_tool(
            "propose_message",
            json!({"conversation_id":"2","content":"An approved account message"}),
            &litecord_agent::Caller::new("test-omni"),
        )
        .await
        .unwrap();
    assert_eq!(proposal["result"]["outcome"], "pending_approval");
    let action_id = litecord_types::ActionId(proposal["result"]["action_id"].as_i64().unwrap());
    let row = app
        .database()
        .read(|r| repos::actions::get(r, action_id))
        .unwrap()
        .unwrap();
    assert_eq!(
        row.identity,
        litecord_types::provenance::DiscordIdentity::UserSession
    );
    assert!(transport
        .requests()
        .iter()
        .all(|r| r.method == discord_adapter::bot::rest::Method::Get));
    let mut sent = message("899", "An approved account message");
    sent["author"] = json!({"id":"1","username":"owner"});
    transport.respond("POST /channels/2/messages", sent);
    app.approve_action(action_id, None).await.unwrap();
    assert_eq!(
        transport
            .requests()
            .iter()
            .filter(|r| r.method == discord_adapter::bot::rest::Method::Post)
            .count(),
        1
    );
    assert_eq!(
        app.database()
            .read(|r| repos::messages::get(r, MessageId(899)))
            .unwrap()
            .unwrap()
            .origin,
        Origin::DiscordUserSession
    );
    app.change_presence(PresenceDraft {
        status: PresenceStatus::DoNotDisturb,
        activity: None,
    })
    .await
    .unwrap();
    let presence: Value = serde_json::from_str(
        &tokio::time::timeout(Duration::from_secs(5), socket.from_client.recv())
            .await
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(presence["op"], 3);
    assert_eq!(presence["d"]["status"], "dnd");
    assert!(
        app.change_presence(PresenceDraft {
            status: PresenceStatus::Online,
            activity: None
        })
        .await
        .is_err(),
        "presence changes respect the cooldown"
    );
    let relation = |kind| json!([{"type":kind,"user":{"id":"3","username":"friend"}}]);
    transport.respond("GET /users/@me/relationships", relation(3));
    for (action, method, next, expected) in [
        (
            RelationshipAction::AcceptFriendRequest,
            "PUT",
            relation(1),
            RelationshipKind::Friend,
        ),
        (
            RelationshipAction::RemoveFriend,
            "DELETE",
            json!([]),
            RelationshipKind::None,
        ),
        (
            RelationshipAction::SendFriendRequest,
            "PUT",
            relation(4),
            RelationshipKind::PendingOutgoing,
        ),
        (
            RelationshipAction::Block,
            "PUT",
            relation(2),
            RelationshipKind::Blocked,
        ),
        (
            RelationshipAction::Unblock,
            "DELETE",
            json!([]),
            RelationshipKind::None,
        ),
    ] {
        transport.respond_then(
            &format!("{method} /users/@me/relationships/3"),
            Value::Null,
            "GET /users/@me/relationships",
            next,
        );
        app.change_relationship(litecord_types::UserId(3), action)
            .await
            .unwrap();
        assert_eq!(
            app.database()
                .read(|r| repos::relationships::get(r, litecord_types::UserId(3)))
                .unwrap()
                .map(|r| r.discord)
                .unwrap_or(RelationshipKind::None),
            expected
        );
    }
    transport.respond("GET /users/@me/relationships", relation(3));
    transport.respond_then(
        "DELETE /users/@me/relationships/3",
        Value::Null,
        "GET /users/@me/relationships",
        json!([]),
    );
    app.change_relationship(
        litecord_types::UserId(3),
        RelationshipAction::RejectFriendRequest,
    )
    .await
    .unwrap();
    app.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn account_write_switch_blocks_pending_approvals_and_survives_restart() {
    use litecord_types::actions::{MessageTarget, PresenceDraft, RelationshipAction};
    use litecord_types::capability::{Capability, SessionAccessMode};
    let dir = tempfile::tempdir().unwrap();
    let cfg = LitecordConfig {
        data_dir: dir.path().to_owned(),
        ..Default::default()
    };
    let (_, transport, secrets) = setup();
    let backend = Arc::new(UserSessionBackend::with_access(
        Arc::new(transport.clone()),
        secrets.clone(),
        Arc::new(SystemClock),
        SessionAccessMode::ReadWrite,
    ));
    let mut socket = transport.next_socket();
    let app = LitecordApp::builder(cfg.clone())
        .backend(backend.clone())
        .start()
        .await
        .unwrap();
    app.authenticate_session(Secret::new("dummy".into()))
        .await
        .unwrap();
    ready(&mut socket).await;
    state(&app, SessionState::Ready).await;
    let out = app
        .agent_gateway()
        .call_tool(
            "propose_message",
            json!({"conversation_id":"2","content":"Needs approval"}),
            &litecord_agent::Caller::new("test-agent"),
        )
        .await
        .unwrap();
    let id = litecord_types::ActionId(out["result"]["action_id"].as_i64().unwrap());
    let before = transport.requests().len();
    app.set_account_writes(false).await.unwrap();
    assert!(!backend.capabilities().is_usable(Capability::DmSend));
    assert!(
        !app.conversation_view(ConversationId(2), 20, None)
            .unwrap()
            .capabilities
            .can_send
    );
    assert!(app.approve_action(id, None).await.is_err());
    assert!(backend
        .send_message(
            &MessageTarget::Conversation {
                conversation_id: ConversationId(2)
            },
            "denied"
        )
        .await
        .is_err());
    assert!(backend
        .set_presence(&PresenceDraft {
            status: litecord_types::social::PresenceStatus::Online,
            activity: None
        })
        .await
        .is_err());
    assert!(backend
        .relationship_action(litecord_types::UserId(3), RelationshipAction::Block)
        .await
        .is_err());
    assert!(transport.requests()[before..]
        .iter()
        .all(|r| r.method == discord_adapter::bot::rest::Method::Get));
    app.shutdown().await;
    drop(app);
    let (_, second_transport, _) = setup();
    let second = Arc::new(UserSessionBackend::with_access(
        Arc::new(second_transport.clone()),
        secrets,
        Arc::new(SystemClock),
        SessionAccessMode::ReadWrite,
    ));
    let mut socket = second_transport.next_socket();
    let app = LitecordApp::builder(cfg)
        .backend(second.clone())
        .start()
        .await
        .unwrap();
    ready(&mut socket).await;
    state(&app, SessionState::Ready).await;
    assert_eq!(
        app.settings_view().unwrap().account_access,
        Some(SessionAccessMode::ReadOnly)
    );
    assert!(!second.capabilities().is_usable(Capability::DmSend));
    app.set_account_writes(true).await.unwrap();
    assert!(second.capabilities().is_usable(Capability::DmSend));
    app.shutdown().await;
}
