#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use discord_adapter::{fixtures, MockBackend};
use litecord_app::people::{AccountViewModel, ContactViewModel};
use litecord_app::LitecordApp;
use litecord_core::config::LitecordConfig;
use litecord_core::ports::BackendError;
use litecord_store::repos;
use litecord_types::ids::UserId;
use litecord_types::notes::UserNote;
use litecord_types::provenance::Origin;
use litecord_types::social::PresenceStatus;
use litecord_types::Timestamp;
use tokio::time::sleep;

async fn wait_for_demo_people(
    app: &LitecordApp,
    self_id: UserId,
    contact_id: UserId,
    expected_presence: PresenceStatus,
) -> (AccountViewModel, ContactViewModel) {
    for _ in 0..300 {
        let account = app.account_view().unwrap();
        if let Some(contact) = app.contact_view(contact_id).unwrap() {
            if account.user_id == Some(self_id) && contact.presence.status == expected_presence {
                return (account, contact);
            }
        }
        sleep(Duration::from_millis(20)).await;
    }
    panic!("demo account and contact did not hydrate in time");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn people_views_use_hydrated_account_and_contact_data() {
    let data = fixtures::generate(7, Timestamp::now());
    let expected_self = data.current_user.clone();
    let expected_contact = data.users[0].clone();
    let expected_presence = data
        .presences
        .iter()
        .find(|(user_id, _)| *user_id == expected_contact.id)
        .unwrap()
        .1
        .status;
    let app = LitecordApp::builder(LitecordConfig::default())
        .backend(Arc::new(MockBackend::new(data)))
        .in_memory()
        .start()
        .await
        .unwrap();

    let (account, mut contact) = wait_for_demo_people(
        &app,
        expected_self.id,
        expected_contact.id,
        expected_presence,
    )
    .await;

    assert_eq!(account.user_id, Some(expected_self.id));
    assert_eq!(account.display_name, expected_self.display_name());
    assert_eq!(
        account.username.as_deref(),
        Some(expected_self.username.as_ref())
    );
    assert_eq!(account.origin, Some(Origin::Synthetic));
    assert!(account.as_of_revision.get() > 0);

    assert_eq!(contact.user_id, expected_contact.id);
    assert_eq!(contact.display_name, expected_contact.display_name());
    assert_eq!(
        contact.username.as_deref(),
        Some(expected_contact.username.as_ref())
    );
    assert_eq!(
        contact.avatar_url,
        expected_contact.avatar_url.map(|url| url.to_string())
    );
    assert_eq!(contact.presence.status, expected_presence);
    assert_eq!(contact.origin, Origin::Synthetic);
    assert!(!contact.is_stub);
    assert!(contact.as_of_revision.get() > 0);

    let local_note = UserNote {
        user_id: expected_contact.id,
        alias: Some("Ada contact".into()),
        note: Some("Local contact note".into()),
        favorite: true,
        updated_at: Timestamp::now(),
    };
    app.database()
        .write(|tx| repos::notes::upsert_note(tx, &local_note))
        .unwrap();
    contact = app.contact_view(expected_contact.id).unwrap().unwrap();
    assert_eq!(contact.alias.as_deref(), Some("Ada contact"));
    assert_eq!(contact.note.as_deref(), Some("Local contact note"));
    assert!(contact.favorite);

    let missing_id = UserId(987_654_321_098_765_432);
    assert!(app.contact_view(missing_id).unwrap().is_none());

    let stub_id = UserId(987_654_321_098_765_433);
    app.database()
        .write(|tx| repos::users::ensure_stub(tx, stub_id, Origin::Imported, tx.now()))
        .unwrap();
    let stub = app.contact_view(stub_id).unwrap().unwrap();
    assert_eq!(stub.display_name, "Unknown user");
    assert_eq!(stub.username, None);
    assert_eq!(stub.avatar_url, None);
    assert_eq!(stub.presence.status, PresenceStatus::Unknown);
    assert_eq!(stub.origin, Origin::Imported);
    assert!(stub.is_stub);
    assert!(stub.as_of_revision.get() > 0);

    app.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn account_view_does_not_invent_a_self_identity_when_not_signed_in() {
    let data = fixtures::generate(7, Timestamp::now());
    let backend = Arc::new(MockBackend::new(data));
    backend.fail_next(100, BackendError::Sdk("account unavailable".into()));
    let app = LitecordApp::builder(LitecordConfig::default())
        .backend(backend)
        .in_memory()
        .start()
        .await
        .unwrap();

    let account = app.account_view().unwrap();
    assert_eq!(account.user_id, None);
    assert_eq!(account.display_name, "Not signed in");
    assert_eq!(account.username, None);
    assert_eq!(account.avatar_url, None);
    assert_eq!(account.origin, None);
    assert!(account.as_of_revision.get() > 0);

    app.shutdown().await;
}
