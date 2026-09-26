#![allow(clippy::unwrap_used, clippy::expect_used)]
//! End-to-end: demo backend → reactor → store → memory → view models →
//! user actions through the Action Engine.

use std::sync::Arc;
use std::time::Duration;

use discord_adapter::{fixtures, MockBackend};
use litecord_app::LitecordApp;
use litecord_core::config::LitecordConfig;
use litecord_types::memory::MemoryKind;
use litecord_types::Timestamp;

async fn eventually<T>(label: &str, mut f: impl FnMut() -> Option<T>) -> T {
    for _ in 0..200 {
        if let Some(v) = f() {
            return v;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("condition not met in time: {label}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn demo_app_hydrates_ingests_and_serves_views() {
    let now = Timestamp::now();
    let backend = Arc::new(MockBackend::new(fixtures::generate(7, now)));
    let app = LitecordApp::builder(LitecordConfig::default())
        .backend(backend.clone())
        .in_memory()
        .start()
        .await
        .unwrap();

    // Initial hydration populates canonical state through the reducer.
    let friends = eventually("friends", || {
        let vm = app.friends_view().ok()?;
        (vm.online.len() + vm.offline.len() > 0).then_some(vm)
    })
    .await;
    assert!(!friends.pending_incoming.is_empty());
    let convs = eventually("conversations", || {
        let vm = app.conversations_view(50).ok()?;
        (vm.conversations.len() >= 5).then_some(vm)
    })
    .await;
    let conv = &convs.conversations[0];
    let friend = conv.recipient_id.unwrap();

    // A live incoming message flows through the whole pipeline.
    let mut events = app.subscribe();
    backend
        .inject_incoming_message(
            conv.conversation_id,
            friend,
            "Can you review the zeppelin prototype?",
        )
        .await;
    let view = eventually("incoming message", || {
        let vm = app.conversation_view(conv.conversation_id, 50, None).ok()?;
        vm.messages
            .iter()
            .any(|m| m.render.content.contains("zeppelin"))
            .then_some(vm)
    })
    .await;
    assert!(view.capabilities.can_send);
    assert!(events.try_recv().is_ok(), "state changes were broadcast");

    // Heuristic memory extraction produced a pending-reply candidate.
    let mem = eventually("pending reply memory", || {
        let vm = app.memory_view(None, false).ok()?;
        vm.memories
            .iter()
            .any(|m| m.kind == MemoryKind::PendingReply && m.content.contains("zeppelin"))
            .then_some(vm)
    })
    .await;
    assert!(!mem.counts_by_status.is_empty());
    let inbox = app.agent_inbox_view().unwrap();
    assert!(!inbox.needs_attention.is_empty());

    // The user replies from the composer: executes immediately, audited.
    app.send_message(conv.conversation_id, "Sure, tonight!")
        .await
        .unwrap();
    assert_eq!(backend.calls("send_message"), 1);
    eventually("sent message", || {
        let vm = app.conversation_view(conv.conversation_id, 50, None).ok()?;
        vm.messages
            .iter()
            .any(|m| m.is_mine && m.render.content == "Sure, tonight!")
            .then_some(())
    })
    .await;

    // Command palette + feature intents.
    let palette = app.command_palette("compact", None).unwrap();
    assert!(palette
        .matches
        .iter()
        .any(|m| m.id.to_string() == "appearance.toggle_compact"));
    app.run_command("appearance.toggle_compact", None)
        .await
        .unwrap();
    let settings = app.settings_view().unwrap();
    let compact = settings
        .sections
        .iter()
        .flat_map(|(_, rows)| rows)
        .find(|r| r.descriptor.key == "appearance.compact")
        .unwrap();
    assert_eq!(compact.value, serde_json::json!(true));

    let diag = app.diagnostics_view().unwrap();
    assert!(diag.revision.get() > 0);
    assert!(diag.counts.messages > 0);

    let report = app.shutdown().await;
    assert!(
        report.aborted.is_empty(),
        "all tasks stopped cleanly: {report:?}"
    );
}
