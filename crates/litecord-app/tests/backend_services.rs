#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Backend services added for the UI: audio devices, files, scoped commands.

use std::sync::Arc;
use std::time::Duration;

use discord_adapter::{fixtures, MockBackend};
use litecord_app::view::{CommandScope, UiEffect};
use litecord_app::LitecordApp;
use litecord_core::config::LitecordConfig;
use litecord_core::events::{DiscordEvent, SourceEnvelope};
use litecord_core::ports::VoiceControl;
use litecord_store::reducer::{self, ReducerConfig};
use litecord_types::ids::MessageId;
use litecord_types::provenance::DiscordSource;
use litecord_types::social::{AudioDeviceKind, Message, MessageExtra};
use litecord_types::Timestamp;

async fn start() -> (LitecordApp, Arc<MockBackend>) {
    let backend = Arc::new(MockBackend::new(fixtures::generate(5, Timestamp::now())));
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
    (app, backend)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn audio_devices_are_listed_and_validated() {
    let (app, _backend) = start().await;
    let devices = app.audio_devices().await.unwrap();
    assert!(devices
        .iter()
        .any(|d| d.kind == AudioDeviceKind::Input && d.is_default));
    assert!(devices.iter().any(|d| d.kind == AudioDeviceKind::Output));
    let usb = devices.iter().find(|d| d.id == "in-usb").unwrap();
    let state = app
        .voice(VoiceControl::SetInputDevice(usb.id.clone()))
        .await
        .unwrap();
    assert_eq!(state.input_device.as_deref(), Some("in-usb"));
    assert!(
        app.voice(VoiceControl::SetInputDevice("out-usb".into()))
            .await
            .is_err(),
        "an output device cannot be used as input"
    );
    app.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn files_view_lists_attachments_newest_first_with_paging() {
    let (app, _backend) = start().await;
    let conv = app.conversations_view(10).unwrap().conversations[0].clone();
    let author = conv.recipient_id.unwrap();
    for (i, name) in ["a.png", "b.pdf", "c.zip"].iter().enumerate() {
        let message = Message {
            id: MessageId(990_000 + i as u64),
            conversation_id: conv.conversation_id,
            author_id: author,
            content: format!("file {name}").into(),
            sent_at: Timestamp::from_millis(1_000 + i as i64),
            edited_at: None,
            reply_to: None,
            extras: vec![MessageExtra::Attachment {
                filename: (*name).into(),
                content_type: None,
                size_bytes: 10 * (i as u64 + 1),
            }],
        };
        reducer::apply(
            app.database(),
            &SourceEnvelope::new(
                DiscordSource::Synthetic,
                Timestamp::now(),
                DiscordEvent::MessageCreated { message },
            ),
            &ReducerConfig::default(),
        )
        .unwrap();
    }
    let page = app
        .conversation_files_view(conv.conversation_id, 2, None)
        .unwrap();
    let names: Vec<_> = page.files.iter().map(|f| f.filename.as_str()).collect();
    assert_eq!(names, ["c.zip", "b.pdf"]);
    assert!(page.has_more);
    let next = app
        .conversation_files_view(conv.conversation_id, 2, Some(page.files[1].sent_at))
        .unwrap();
    assert_eq!(next.files.len(), 1);
    assert_eq!(next.files[0].filename, "a.png");
    assert!(!next.has_more);
    assert!(next.files[0].open_in_discord_url.contains("/channels/@me/"));
    app.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn message_commands_need_a_valid_selected_message() {
    let (app, _backend) = start().await;
    let conv = app.conversations_view(10).unwrap().conversations[0].clone();
    let view = app
        .conversation_view(conv.conversation_id, 10, None)
        .unwrap();
    // History hydrates on open; wait for a message.
    let mut msg = view.messages.last().map(|m| m.message_id);
    for _ in 0..100 {
        if msg.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
        msg = app
            .conversation_view(conv.conversation_id, 10, None)
            .unwrap()
            .messages
            .last()
            .map(|m| m.message_id);
    }
    let msg = msg.unwrap();

    let without = app
        .command_palette("copy selected", Some(conv.conversation_id))
        .unwrap();
    let m = without
        .matches
        .iter()
        .find(|m| m.id.to_string() == "message.copy_id")
        .unwrap();
    assert!(!m.available);

    let scope = CommandScope {
        active_conversation: Some(conv.conversation_id),
        selected_message: Some(msg),
    };
    let with = app.command_palette_in("copy selected", scope).unwrap();
    assert!(with
        .matches
        .iter()
        .any(|m| m.id.to_string() == "message.copy_id" && m.available));
    let effects = app.run_command_in("message.copy_id", scope).await.unwrap();
    assert_eq!(
        effects,
        vec![UiEffect::CopyToClipboard {
            text: msg.to_string()
        }]
    );

    // A message id from nowhere does not enable message commands.
    let bogus = CommandScope {
        active_conversation: Some(conv.conversation_id),
        selected_message: Some(MessageId(1)),
    };
    assert!(app.run_command_in("message.copy_id", bogus).await.is_err());
    app.shutdown().await;
}
