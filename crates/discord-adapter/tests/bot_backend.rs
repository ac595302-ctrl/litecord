#![allow(clippy::unwrap_used, clippy::expect_used)]
//! BotBackend over a scripted transport: gateway session → normalized events,
//! REST reads/writes, token isolation, fatal close handling.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use discord_adapter::bot::transport::fake::FakeTransport;
use discord_adapter::bot::{BotBackend, BotConfig, SocketEvent};
use litecord_core::bus::{ingest_channel, IngestReceiver};
use litecord_core::clock::SystemClock;
use litecord_core::events::DiscordEvent;
use litecord_core::ports::SocialBackend;
use litecord_core::secrets::Secret;
use litecord_types::actions::MessageTarget;
use litecord_types::ids::*;
use litecord_types::provenance::DiscordSource;
use litecord_types::social::SessionState;

const TOKEN: &str = "bot-token-SUPER-SECRET-123";

fn backend(t: &FakeTransport) -> BotBackend {
    t.respond(
        "GET /gateway/bot",
        json!({"url": "wss://gateway.discord.gg"}),
    );
    BotBackend::new(
        Arc::new(t.clone()),
        Secret::new(TOKEN.into()),
        BotConfig::default(),
        Arc::new(SystemClock),
    )
}

async fn next_event(rx: &mut IngestReceiver) -> (DiscordSource, DiscordEvent) {
    let env = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .unwrap()
        .unwrap();
    (env.source, env.event)
}

fn frame(op: u8, d: Value, s: Option<u64>, t: Option<&str>) -> SocketEvent {
    SocketEvent::Text(json!({"op": op, "d": d, "s": s, "t": t}).to_string())
}

#[tokio::test]
async fn gateway_session_produces_bot_events() {
    let t = FakeTransport::new();
    let mut sock = t.next_socket();
    let bot = backend(&t);
    let (sink, mut rx) = ingest_channel(64);
    bot.connect(sink).await.unwrap();

    let (_, e) = next_event(&mut rx).await;
    assert_eq!(
        e,
        DiscordEvent::SessionChanged {
            state: SessionState::Connecting
        }
    );

    sock.to_client
        .send(frame(10, json!({"heartbeat_interval": 45000}), None, None))
        .await
        .unwrap();
    let identify: Value = serde_json::from_str(&sock.from_client.recv().await.unwrap()).unwrap();
    assert_eq!(identify["op"], 2);
    assert!(
        identify["d"]["intents"].as_u64().unwrap() & (1 << 15) != 0,
        "MESSAGE_CONTENT intent"
    );

    sock.to_client
        .send(frame(
            0,
            json!({"session_id": "s1", "resume_gateway_url": "wss://resume",
                   "user": {"id": "7000", "username": "litecord-bot", "bot": true}}),
            Some(1),
            Some("READY"),
        ))
        .await
        .unwrap();
    let (src, e) = next_event(&mut rx).await;
    assert_eq!(src, DiscordSource::BotGateway);
    assert!(matches!(e, DiscordEvent::CurrentUser { ref user } if user.is_bot));

    sock.to_client
        .send(frame(
            0,
            json!({
                "id": "900", "channel_id": "55", "guild_id": "1",
                "author": {"id": "42", "username": "ada"},
                "content": "hello bot", "timestamp": "2026-09-24T15:00:00.000000+00:00",
                "attachments": [], "embeds": []
            }),
            Some(2),
            Some("MESSAGE_CREATE"),
        ))
        .await
        .unwrap();
    let mut saw_message = false;
    for _ in 0..4 {
        if let (_, DiscordEvent::MessageCreated { message }) = next_event(&mut rx).await {
            assert_eq!(message.conversation_id, ConversationId(55));
            saw_message = true;
            break;
        }
    }
    assert!(saw_message);

    // The message→channel index lets edits find the channel.
    t.respond("PATCH /channels/55/messages/900", json!({}));
    bot.edit_message(MessageId(900), "edited").await.unwrap();
    assert!(
        bot.edit_message(MessageId(12345), "x").await.is_err(),
        "unknown channel"
    );

    // The token is never in Debug output.
    assert!(!format!("{bot:?}").contains(TOKEN));
    bot.disconnect().await.unwrap();
}

#[tokio::test]
async fn fatal_close_stops_with_a_safe_error() {
    let t = FakeTransport::new();
    let sock = t.next_socket();
    let bot = backend(&t);
    let (sink, mut rx) = ingest_channel(64);
    bot.connect(sink).await.unwrap();
    let _ = next_event(&mut rx).await; // Connecting
    sock.to_client
        .send(frame(10, json!({"heartbeat_interval": 45000}), None, None))
        .await
        .unwrap();
    sock.to_client
        .send(SocketEvent::Closed(Some(4004)))
        .await
        .unwrap();
    loop {
        let (_, e) = next_event(&mut rx).await;
        if let DiscordEvent::SessionChanged {
            state: SessionState::Error { message },
        } = e
        {
            assert!(!message.contains(TOKEN));
            break;
        }
    }
    assert_eq!(t.connects(), 1, "authentication failure is not retried");
    bot.disconnect().await.unwrap();
}

#[tokio::test]
async fn rest_reads_and_writes() {
    let t = FakeTransport::new();
    let bot = backend(&t);
    t.respond(
        "GET /users/@me/guilds",
        json!([{"id": "1", "name": "Rustaceans"}]),
    );
    t.respond(
        "GET /guilds/1/channels",
        json!([
            {"id": "55", "type": 0, "name": "general", "position": 0, "guild_id": "1"},
            {"id": "56", "type": 2, "name": "voice", "position": 1, "guild_id": "1"}
        ]),
    );
    let convs = bot.conversations().await.unwrap();
    assert_eq!(convs.len(), 1, "only the text channel is a conversation");
    assert_eq!(convs[0].id, ConversationId(55));

    t.respond(
        "GET /channels/55/messages",
        json!([
            {"id": "902", "channel_id": "55", "author": {"id": "42", "username": "ada"},
             "content": "newer", "timestamp": "2026-09-24T15:02:00+00:00"},
            {"id": "901", "channel_id": "55", "author": {"id": "42", "username": "ada"},
             "content": "older", "timestamp": "2026-09-24T15:01:00+00:00"}
        ]),
    );
    let msgs = bot.messages(ConversationId(55), 50).await.unwrap();
    assert_eq!(
        msgs.iter().map(|m| m.content.as_ref()).collect::<Vec<_>>(),
        ["older", "newer"]
    );

    t.respond(
        "POST /channels/55/messages",
        json!({"id": "903", "channel_id": "55",
               "author": {"id": "7000", "username": "litecord-bot", "bot": true},
               "content": "hi all", "timestamp": "2026-09-24T15:03:00+00:00"}),
    );
    let sent = bot
        .send_message(
            &MessageTarget::Conversation {
                conversation_id: ConversationId(55),
            },
            "hi all",
        )
        .await
        .unwrap();
    assert_eq!(sent.id, MessageId(903));
    let post = t
        .requests()
        .into_iter()
        .find(|r| r.path.ends_with("/channels/55/messages") && r.body.is_some())
        .unwrap();
    assert_eq!(
        post.body.unwrap()["allowed_mentions"]["parse"],
        json!([]),
        "no mass mentions"
    );
    t.respond("DELETE /channels/55/messages/903", Value::Null);
    bot.delete_message(MessageId(903)).await.unwrap();
    assert!(
        bot.send_message(
            &MessageTarget::User {
                user_id: UserId(42)
            },
            "dm"
        )
        .await
        .is_err(),
        "bot does not DM users"
    );
}

#[tokio::test]
async fn history_page_sends_cursor_query_and_returns_oldest_first() {
    use litecord_core::ports::HistoryPageRequest;

    let t = FakeTransport::new();
    let bot = backend(&t);
    // FakeTransport matches on METHOD + path without the query string; the
    // query is asserted on the recorded requests instead.
    t.respond(
        "GET /channels/55/messages",
        json!([
            {"id": "902", "channel_id": "55", "author": {"id": "42", "username": "ada"},
             "content": "newer", "timestamp": "2026-09-24T15:02:00+00:00"},
            {"id": "901", "channel_id": "55", "author": {"id": "42", "username": "ada"},
             "content": "older", "timestamp": "2026-09-24T15:01:00+00:00"}
        ]),
    );

    let conv = ConversationId(55);
    let page = bot
        .history_page(&HistoryPageRequest::before(conv, MessageId(903), 2))
        .await
        .unwrap();
    assert_eq!(
        page.messages.iter().map(|m| m.id.get()).collect::<Vec<_>>(),
        [901, 902]
    );
    assert!(page.has_more, "a full page may have more");
    assert_eq!(page.oldest(), Some(MessageId(901)));

    let page = bot
        .history_page(&HistoryPageRequest::after(conv, MessageId(900), 500))
        .await
        .unwrap();
    assert_eq!(page.messages.len(), 2);
    assert!(!page.has_more, "a short page is the end");

    // Both cursors: only `after` goes on the wire; `before` is applied
    // locally and cuts the page short.
    let page = bot
        .history_page(&HistoryPageRequest {
            conversation_id: conv,
            before: Some(MessageId(902)),
            after: Some(MessageId(900)),
            limit: 2,
        })
        .await
        .unwrap();
    assert_eq!(
        page.messages.iter().map(|m| m.id.get()).collect::<Vec<_>>(),
        [901]
    );
    assert!(!page.has_more);

    let paths: Vec<String> = t
        .requests()
        .into_iter()
        .map(|r| r.path)
        .filter(|p| p.starts_with("/channels/55/messages"))
        .collect();
    assert_eq!(
        paths,
        [
            "/channels/55/messages?limit=2&before=903",
            "/channels/55/messages?limit=100&after=900",
            "/channels/55/messages?limit=2&after=900",
        ]
    );

    // Paged messages are remembered for later edits/deletes.
    t.respond("DELETE /channels/55/messages/901", json!(null));
    bot.delete_message(MessageId(901)).await.unwrap();
}
