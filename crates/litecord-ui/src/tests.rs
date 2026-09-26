#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use crate::bridge::{execute, snapshot, Bridge, Command, Selection};
use discord_adapter::{fixtures, MockBackend};
use litecord_app::LitecordApp;
use litecord_core::config::LitecordConfig;
use litecord_layout::Destination;
use litecord_types::Timestamp;
use tokio::time::{sleep, timeout};

async fn wait_for_demo_conversation(
    app: &LitecordApp,
) -> (litecord_types::ConversationId, litecord_types::UserId) {
    for _ in 0..300 {
        let conversations = app.conversations_view(200).unwrap();
        if let Some(row) = conversations
            .conversations
            .iter()
            .find(|row| row.recipient_id.is_some())
        {
            let conversation_id = row.conversation_id;
            let contact_id = row.recipient_id.unwrap();
            if app
                .conversation_view(conversation_id, 200, None)
                .is_ok_and(|chat| chat.messages.len() >= 8)
            {
                return (conversation_id, contact_id);
            }
        }
        sleep(Duration::from_millis(20)).await;
    }
    panic!("demo conversation did not hydrate in time");
}

async fn wait_for_sent_message(
    app: &LitecordApp,
    conversation_id: litecord_types::ConversationId,
    content: &str,
) {
    for _ in 0..200 {
        if app
            .conversation_view(conversation_id, 200, None)
            .is_ok_and(|chat| {
                chat.messages
                    .iter()
                    .any(|message| message.is_mine && message.render.content == content)
            })
        {
            return;
        }
        sleep(Duration::from_millis(20)).await;
    }
    panic!("sent message did not appear in canonical app views");
}

fn selection(
    generation: u64,
    conversation_id: litecord_types::ConversationId,
    contact_id: litecord_types::UserId,
    before: Option<Timestamp>,
) -> Selection {
    Selection {
        generation,
        destination: Destination::Messages,
        conversation: Some(conversation_id),
        contact: Some(contact_id),
        before,
        palette_query: String::new(),
        task: None,
        omni_session: None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bridge_send_and_selection_snapshots_preserve_app_state() {
    let data = fixtures::generate(11, Timestamp::now());
    let backend = Arc::new(MockBackend::new(data));
    let app = LitecordApp::builder(LitecordConfig::default())
        .backend(backend)
        .in_memory()
        .start()
        .await
        .unwrap();
    let (conversation_id, contact_id) = wait_for_demo_conversation(&app).await;

    let latest = snapshot(&app, selection(41, conversation_id, contact_id, None)).unwrap();
    assert_eq!(latest.selection.generation, 41);
    assert_eq!(latest.selection.destination, Destination::Messages);
    assert_eq!(latest.selection.conversation, Some(conversation_id));
    assert_eq!(latest.selection.contact, Some(contact_id));
    assert_eq!(latest.selection.before, None);
    assert_eq!(
        latest.chat.as_ref().unwrap().conversation_id,
        conversation_id
    );
    assert_eq!(latest.contact.as_ref().unwrap().user_id, contact_id);

    let latest_messages = &latest.chat.as_ref().unwrap().messages;
    assert!(latest_messages.len() >= 8);
    let before = latest_messages[latest_messages.len() / 2].sent_at;
    let older = snapshot(
        &app,
        selection(42, conversation_id, contact_id, Some(before)),
    )
    .unwrap();
    assert_eq!(older.selection.generation, 42);
    assert_eq!(older.selection.conversation, Some(conversation_id));
    assert_eq!(older.selection.contact, Some(contact_id));
    assert_eq!(older.selection.before, Some(before));
    let older_messages = &older.chat.as_ref().unwrap().messages;
    assert!(!older_messages.is_empty());
    assert!(older_messages.len() < latest_messages.len());
    assert!(older_messages
        .iter()
        .all(|message| message.sent_at < before));

    let content = "bridge send canonical marker";
    let completion = execute(
        &app,
        Command::SendAs(
            conversation_id,
            content.into(),
            litecord_types::provenance::DiscordIdentity::UserSocialSdk,
        ),
    )
    .await
    .unwrap();
    assert_eq!(completion.sent, Some((conversation_id, content.into())));
    wait_for_sent_message(&app, conversation_id, content).await;

    let before_activation = app
        .conversation_view(conversation_id, 200, None)
        .unwrap()
        .messages
        .into_iter()
        .map(|message| (message.message_id, message.render.content))
        .collect::<Vec<_>>();

    let profiles = app.layout_profiles_view().unwrap();
    let profile_id = app
        .create_layout_profile("Bridge test profile", &profiles.storage_token)
        .unwrap();
    let after_create = app.layout_profiles_view().unwrap();
    let activation = execute(
        &app,
        Command::ActivateProfile(profile_id.clone(), after_create.storage_token),
    )
    .await
    .unwrap();
    assert!(activation.error.is_none());
    assert!(activation.sent.is_none());
    assert_eq!(
        app.layout_profiles_view()
            .unwrap()
            .profiles
            .active_profile_id,
        profile_id
    );

    let after_activation = app
        .conversation_view(conversation_id, 200, None)
        .unwrap()
        .messages
        .into_iter()
        .map(|message| (message.message_id, message.render.content))
        .collect::<Vec<_>>();
    assert_eq!(after_activation, before_activation);
    assert!(after_activation
        .iter()
        .any(|(_, message)| message == content));

    app.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_bridge_send_returns_error_without_a_sent_completion() {
    let data = fixtures::generate(13, Timestamp::now());
    let app = LitecordApp::builder(LitecordConfig::default())
        .backend(Arc::new(MockBackend::new(data)))
        .in_memory()
        .start()
        .await
        .unwrap();
    let (conversation_id, _) = wait_for_demo_conversation(&app).await;

    let mut bridge = Bridge::new(
        app.clone(),
        tokio::runtime::Handle::current(),
        eframe::egui::Context::default(),
    );
    bridge
        .commands
        .send(Command::SendAs(
            conversation_id,
            String::new(),
            litecord_types::provenance::DiscordIdentity::UserSocialSdk,
        ))
        .await
        .unwrap();
    let completion = timeout(Duration::from_secs(5), bridge.completions.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(completion.error.is_some());
    assert!(completion.sent.is_none());

    app.shutdown().await;
}
