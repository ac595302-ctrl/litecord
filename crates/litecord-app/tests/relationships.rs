#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use discord_adapter::{fixtures, MockBackend};
use litecord_actions::ProposeOutcome;
use litecord_app::view::FriendsViewModel;
use litecord_app::LitecordApp;
use litecord_core::config::LitecordConfig;
use litecord_types::actions::RelationshipAction;
use litecord_types::ids::UserId;
use litecord_types::social::RelationshipKind;
use litecord_types::Timestamp;
use tokio::time::sleep;

fn relationship_kind(view: &FriendsViewModel, user_id: UserId) -> Option<RelationshipKind> {
    view.online
        .iter()
        .chain(&view.offline)
        .chain(&view.pending_incoming)
        .chain(&view.pending_outgoing)
        .chain(&view.blocked)
        .find(|row| row.user_id == user_id)
        .map(|row| row.relationship)
}

async fn wait_for_relationship(
    app: &LitecordApp,
    user_id: UserId,
    expected: Option<RelationshipKind>,
    newer_than_revision: u64,
) -> FriendsViewModel {
    for _ in 0..250 {
        let view = app.friends_view().unwrap();
        if view.as_of_revision.get() > newer_than_revision
            && relationship_kind(&view, user_id) == expected
        {
            return view;
        }
        sleep(Duration::from_millis(20)).await;
    }
    panic!("relationship did not update to {expected:?} in time");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn user_relationship_actions_execute_and_reduce_backend_events() {
    let now = Timestamp::now();
    let data = fixtures::generate(31, now);
    let incoming_id = data
        .relationships
        .iter()
        .find(|relationship| relationship.discord == RelationshipKind::PendingIncoming)
        .unwrap()
        .user_id;
    let backend = Arc::new(MockBackend::new(data));
    let app = LitecordApp::builder(LitecordConfig::default())
        .backend(backend.clone())
        .in_memory()
        .start()
        .await
        .unwrap();

    let incoming = wait_for_relationship(
        &app,
        incoming_id,
        Some(RelationshipKind::PendingIncoming),
        0,
    )
    .await;
    assert!(app.actions().pending(50).unwrap().is_empty());

    let accepted = app
        .change_relationship(incoming_id, RelationshipAction::AcceptFriendRequest)
        .await
        .unwrap();
    assert!(matches!(accepted, ProposeOutcome::Executed { .. }));
    let friend = wait_for_relationship(
        &app,
        incoming_id,
        Some(RelationshipKind::Friend),
        incoming.as_of_revision.get(),
    )
    .await;
    assert!(app.actions().pending(50).unwrap().is_empty());

    let blocked = app
        .change_relationship(incoming_id, RelationshipAction::Block)
        .await
        .unwrap();
    assert!(matches!(blocked, ProposeOutcome::Executed { .. }));
    let blocked_view = wait_for_relationship(
        &app,
        incoming_id,
        Some(RelationshipKind::Blocked),
        friend.as_of_revision.get(),
    )
    .await;
    assert!(app.actions().pending(50).unwrap().is_empty());

    let unblocked = app
        .change_relationship(incoming_id, RelationshipAction::Unblock)
        .await
        .unwrap();
    assert!(matches!(unblocked, ProposeOutcome::Executed { .. }));
    wait_for_relationship(&app, incoming_id, None, blocked_view.as_of_revision.get()).await;

    assert_eq!(backend.calls("relationship_action"), 3);
    assert!(app.actions().pending(50).unwrap().is_empty());
    app.shutdown().await;
}
