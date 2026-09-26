#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Optional application-bot source: separate identity, guild channels as
//! conversations, explicit identity on sends and approvals.

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;

use discord_adapter::{fixtures, MockBackend};
use litecord_agent::Caller;
use litecord_app::LitecordApp;
use litecord_core::clock::SystemClock;
use litecord_core::config::LitecordConfig;
use litecord_types::ids::ActionId;
use litecord_types::provenance::DiscordIdentity;
use litecord_types::social::{ConversationKind, SessionState};
use litecord_types::Timestamp;

async fn start() -> (LitecordApp, Arc<MockBackend>, Arc<MockBackend>) {
    let now = Timestamp::now();
    let user = Arc::new(MockBackend::new(fixtures::generate(11, now)));
    let bot = Arc::new(MockBackend::demo_bot(11, now, Arc::new(SystemClock)));
    let app = LitecordApp::builder(LitecordConfig::default())
        .backend(user.clone())
        .bot_backend(bot.clone())
        .in_memory()
        .start()
        .await
        .unwrap();
    for _ in 0..200 {
        let convs = app.conversations_view(100).unwrap().conversations;
        if convs
            .iter()
            .any(|c| c.kind == ConversationKind::GuildChannel)
            && convs
                .iter()
                .any(|c| c.kind == ConversationKind::DirectMessage)
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    (app, user, bot)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bot_source_is_a_separate_identity() {
    let (app, user, bot) = start().await;
    let convs = app.conversations_view(100).unwrap().conversations;
    let channel = convs
        .iter()
        .find(|c| c.kind == ConversationKind::GuildChannel)
        .expect("bot hydrated guild channels")
        .clone();
    let dm = convs
        .iter()
        .find(|c| c.kind == ConversationKind::DirectMessage)
        .unwrap()
        .clone();

    // Separate sessions and accounts.
    let d = app.diagnostics_view().unwrap();
    let status = d.bot.expect("bot status");
    assert_eq!(status.session, SessionState::Ready);
    assert_eq!(
        status.bot_user_id.map(|u| u.get()),
        Some(fixtures::DEMO_BOT_ID)
    );
    assert_eq!(app.session_state().unwrap(), SessionState::Ready);

    // Guild channel is served by the bot; composer shows the bot identity.
    let mut view = app
        .conversation_view(channel.conversation_id, 50, None)
        .unwrap();
    for _ in 0..100 {
        if !view.messages.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
        view = app
            .conversation_view(channel.conversation_id, 50, None)
            .unwrap();
    }
    assert!(
        !view.messages.is_empty(),
        "channel history hydrated via the bot"
    );
    assert_eq!(
        view.capabilities.send_identity,
        Some(DiscordIdentity::ApplicationBot)
    );
    let dm_view = app.conversation_view(dm.conversation_id, 10, None).unwrap();
    assert_eq!(
        dm_view.capabilities.send_identity,
        Some(DiscordIdentity::UserSocialSdk)
    );

    // The user identity cannot post into a guild channel (the SDK can't)...
    assert!(app
        .send_message(channel.conversation_id, "hello channel")
        .await
        .is_err());
    // ...but the user can explicitly send as the bot.
    app.send_message_as(
        channel.conversation_id,
        "hello from the bot",
        DiscordIdentity::ApplicationBot,
    )
    .await
    .unwrap();
    assert_eq!(bot.calls("send_message"), 1);
    assert_eq!(user.calls("send_message"), 0);
    // The bot never posts into the user's DMs.
    assert!(app
        .send_message_as(dm.conversation_id, "nope", DiscordIdentity::ApplicationBot)
        .await
        .is_err());
    app.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_bot_proposals_show_identity_and_need_approval() {
    let (app, _user, bot) = start().await;
    let channel = app
        .conversations_view(100)
        .unwrap()
        .conversations
        .into_iter()
        .find(|c| c.kind == ConversationKind::GuildChannel)
        .unwrap();
    let gateway = app.agent_gateway();
    let caller = Caller::new("test-agent");
    let out = gateway
        .call_tool(
            "propose_message",
            json!({
                "conversation_id": channel.conversation_id.to_string(),
                "content": "Release checklist looks good",
                "send_as": "bot"
            }),
            &caller,
        )
        .await
        .unwrap();
    assert_eq!(out["result"]["outcome"], "pending_approval");
    let id = ActionId(out["result"]["action_id"].as_i64().unwrap());
    assert_eq!(bot.calls("send_message"), 0);

    let row = app
        .agent_inbox_view()
        .unwrap()
        .pending_actions
        .into_iter()
        .find(|p| p.action_id == id)
        .unwrap();
    assert_eq!(row.identity, DiscordIdentity::ApplicationBot);

    app.approve_action(id, None).await.unwrap();
    assert_eq!(bot.calls("send_message"), 1);

    // Bot identity is limited to guild-channel messages.
    let err = gateway
        .call_tool(
            "propose_relationship_change",
            json!({"user_id": "1001", "action": "block", "send_as": "bot"}),
            &caller,
        )
        .await;
    assert!(err.is_err());
    app.shutdown().await;
}

/// Tasks and reminders extracted from a conversation carry its text, so
/// agent listings follow that conversation's visibility.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_task_listings_respect_conversation_visibility() {
    use litecord_store::repos;
    use litecord_types::provenance::Origin;
    use litecord_types::tasks::{TaskDraft, TaskPriority, TaskStatus};
    use litecord_types::trust::AgentVisibility;

    let (app, _, _) = start().await;
    let dm = app
        .conversations_view(100)
        .unwrap()
        .conversations
        .into_iter()
        .find(|c| c.kind == ConversationKind::DirectMessage)
        .unwrap()
        .conversation_id;
    let draft = TaskDraft {
        title: "secret plan from the DM".into(),
        description: None,
        priority: TaskPriority::default(),
        due_at: None,
        related_users: Vec::new(),
        conversation_id: Some(dm),
        parent_id: None,
        source: None,
    };
    app.database()
        .write(|tx| {
            repos::tasks::create(tx, &draft, TaskStatus::Candidate, Origin::LocalApplication)
        })
        .unwrap();

    let gateway = app.agent_gateway();
    let caller = Caller::new("test-agent");
    let listed = |v: serde_json::Value| v.to_string().contains("secret plan from the DM");
    let tasks = gateway
        .call_tool("list_tasks", json!({}), &caller)
        .await
        .unwrap();
    assert!(listed(tasks), "visible conversation: task is listed");

    app.set_conversation_visibility(dm, Some(AgentVisibility::Hidden))
        .unwrap();
    let tasks = gateway
        .call_tool("list_tasks", json!({}), &caller)
        .await
        .unwrap();
    assert!(!listed(tasks), "hidden conversation: task is withheld");
    assert!(!listed(gateway.read_resource("discord://tasks").unwrap()));
}
