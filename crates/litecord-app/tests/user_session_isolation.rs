#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Account-binding and stopped-source regressions for the user-session path.
//! Credentials and gateway frames are scripted; these tests make no live calls.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use discord_adapter::bot::transport::{
    fake::{FakeTransport, SocketHandle},
    SocketEvent,
};
use discord_adapter::user_session::UserSessionBackend;
use litecord_app::LitecordApp;
use litecord_core::bus::ingest_channel;
use litecord_core::clock::SystemClock;
use litecord_core::config::LitecordConfig;
use litecord_core::ports::SocialBackend;
use litecord_core::secrets::{InMemorySecretStore, Secret, SecretKey, SecretStore};
use litecord_store::repos;
use litecord_store::{Database, StoreResult};
use litecord_types::provenance::{DiscordIdentity, Origin};
use litecord_types::social::SessionState;
use litecord_types::UserId;
use serde_json::{json, Value};

fn setup() -> (
    Arc<UserSessionBackend>,
    FakeTransport,
    Arc<InMemorySecretStore>,
) {
    let transport = FakeTransport::new();
    set_account_responses(&transport, "1", "owner");
    let secrets = Arc::new(InMemorySecretStore::default());
    let backend = Arc::new(UserSessionBackend::new(
        Arc::new(transport.clone()),
        secrets.clone(),
        Arc::new(SystemClock),
    ));
    (backend, transport, secrets)
}

fn set_account_responses(transport: &FakeTransport, user_id: &str, username: &str) {
    transport.respond("GET /users/@me", json!({"id":user_id,"username":username}));
    transport.respond("GET /users/@me/guilds", json!([]));
    transport.respond("GET /users/@me/relationships", json!([]));
    transport.respond("GET /users/@me/channels", json!([]));
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

async fn error_state(app: &LitecordApp) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if matches!(app.session_state().unwrap(), SessionState::Error { .. }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("session error state reached");
}

async fn ready(socket: &mut SocketHandle, user_id: &str, username: &str) {
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
    socket
        .to_client
        .send(SocketEvent::Text(
            json!({
                "op":0,
                "s":1,
                "t":"READY",
                "d":{
                    "session_id":"test-session",
                    "resume_gateway_url":"wss://gateway.discord.gg",
                    "user":{"id":user_id,"username":username},
                    "guilds":[],
                    "private_channels":[],
                    "relationships":[]
                }
            })
            .to_string(),
        ))
        .await
        .unwrap();
}

fn test_dir() -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    std::env::temp_dir().join(format!(
        "litecord-user-session-isolation-{}-{stamp}",
        std::process::id()
    ))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn saved_credential_pins_first_account_across_signout_and_reauth() {
    let (backend, transport, secrets) = setup();
    secrets.set(
        SecretKey::DiscordUserSessionToken,
        Secret::new("saved-account-one-token".into()),
    );
    let mut socket = transport.next_socket();
    let app = LitecordApp::builder(LitecordConfig::default())
        .backend(backend.clone())
        .in_memory()
        .start()
        .await
        .unwrap();

    ready(&mut socket, "1", "owner").await;
    state(&app, SessionState::Ready).await;
    let first_account = app
        .database()
        .read(|r| repos::accounts::current(r, DiscordIdentity::UserSession))
        .unwrap()
        .expect("first account was persisted");
    assert_eq!(first_account.user_id, UserId(1));

    app.sign_out().await.unwrap();
    state(&app, SessionState::LoggedOut).await;
    set_account_responses(&transport, "2", "different-owner");

    assert!(app
        .authenticate_session(Secret::new("saved-account-two-token".into()))
        .await
        .is_err());
    let still_first = app
        .database()
        .read(|r| repos::accounts::current(r, DiscordIdentity::UserSession))
        .unwrap()
        .expect("the pinned account remains current");
    assert_eq!(still_first.user_id, UserId(1));
    assert!(secrets.get(SecretKey::DiscordUserSessionToken).is_none());
    app.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn saved_credential_for_wrong_account_stops_before_other_reads_or_rows() {
    let (backend, transport, secrets) = setup();
    set_account_responses(&transport, "2", "different-owner");
    transport.respond(
        "GET /users/@me/guilds",
        json!([{"id":"200","name":"other account guild"}]),
    );
    transport.respond(
        "GET /users/@me/relationships",
        json!([{"id":"3","type":1,"user":{"id":"3","username":"other friend"}}]),
    );
    transport.respond(
        "GET /users/@me/channels",
        json!([{"id":"22","type":1,"recipients":[{"id":"23","username":"other recipient"}]}]),
    );
    secrets.set(
        SecretKey::DiscordUserSessionToken,
        Secret::new("account-two-token".into()),
    );

    let path = test_dir();
    let cfg = LitecordConfig {
        data_dir: path.clone(),
        ..LitecordConfig::default()
    };
    let db = Database::open(cfg.database_path(), &cfg.database).unwrap();
    db.write(|tx| {
        repos::accounts::upsert_current(
            tx,
            UserId(1),
            DiscordIdentity::UserSession,
            Origin::DiscordUserSession,
        )
    })
    .unwrap();
    drop(db);

    let _unused_socket = transport.next_socket();
    let app = LitecordApp::builder(cfg)
        .backend(backend)
        .start()
        .await
        .unwrap();
    error_state(&app).await;

    let requests = transport.requests();
    assert_eq!(
        requests.len(),
        1,
        "only the account verification request is allowed"
    );
    assert_eq!(requests[0].path, "/users/@me");
    assert_eq!(requests[0].method, discord_adapter::bot::rest::Method::Get);
    assert_eq!(
        transport.connects(),
        0,
        "a mismatched credential never opens a gateway"
    );

    let (account, users, guilds, relationships, conversations, messages) = app
        .database()
        .read(|r| -> StoreResult<_> {
            Ok((
                repos::accounts::current(r, DiscordIdentity::UserSession)?,
                repos::users::count(r)?,
                repos::guilds::list(r, false)?.len(),
                repos::relationships::list(r, None)?.len(),
                repos::conversations::list_recent(r, 100, 0)?.len(),
                repos::messages::count(r, None)?,
            ))
        })
        .unwrap();
    assert_eq!(
        account.expect("preseeded account remains").user_id,
        UserId(1)
    );
    assert_eq!(users, 0);
    assert_eq!(guilds, 0);
    assert_eq!(relationships, 0);
    assert_eq!(conversations, 0);
    assert_eq!(messages, 0);

    app.shutdown().await;
    let _ = std::fs::remove_dir_all(path);
}

#[tokio::test]
async fn disconnected_source_rejects_current_user_without_another_read() {
    let (backend, transport, secrets) = setup();
    secrets.set(
        SecretKey::DiscordUserSessionToken,
        Secret::new("saved-account-one-token".into()),
    );
    let _socket = transport.next_socket();
    let (sink, _receiver) = ingest_channel(8);

    backend.connect(sink).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while transport.connects() != 1 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("gateway driver started");
    backend.disconnect().await.unwrap();
    let requests_before = transport.requests().len();

    assert!(backend.current_user().await.is_err());
    assert_eq!(
        transport.requests().len(),
        requests_before,
        "a stopped source must reject reads before touching the transport"
    );
}
