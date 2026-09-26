#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use discord_adapter::{fixtures, MockBackend};
use litecord_app::LitecordApp;
use litecord_core::config::LitecordConfig;
use litecord_core::error::ErrorKind;
use litecord_core::events::{ApplicationEvent, UnifiedEvent};
use litecord_layout::{
    default_workspace, Destination, LayoutNode, LayoutProfiles, Placement, FORMAT_VERSION,
    SETTINGS_KEY,
};
use litecord_store::repos;
use litecord_types::Timestamp;
use tokio::sync::broadcast::{self, error::TryRecvError};
use tokio::time::sleep;

fn test_config() -> LitecordConfig {
    let mut config = LitecordConfig::default();
    config.hydration.reconcile_interval_secs = 3_600;
    config
}

fn empty_backend() -> Arc<MockBackend> {
    let data = fixtures::generate(1, Timestamp::now());
    Arc::new(MockBackend::empty(data.current_user))
}

async fn start_app(config: LitecordConfig) -> LitecordApp {
    LitecordApp::builder(config)
        .backend(empty_backend())
        .start()
        .await
        .unwrap()
}

async fn start_in_memory() -> LitecordApp {
    LitecordApp::builder(test_config())
        .backend(empty_backend())
        .in_memory()
        .start()
        .await
        .unwrap()
}

async fn wait_until_quiet(app: &LitecordApp) {
    let mut previous = app.database().current_revision().unwrap();
    let mut stable_polls = 0;
    for _ in 0..250 {
        sleep(Duration::from_millis(20)).await;
        let current = app.database().current_revision().unwrap();
        if app.hydrator().pending_len() == 0
            && app.hydrator().active_len() == 0
            && current == previous
        {
            stable_polls += 1;
            if stable_polls >= 4 {
                return;
            }
        } else {
            stable_polls = 0;
        }
        previous = current;
    }
    panic!("app did not settle after startup hydration");
}

fn event_has_layout_change(event: &ApplicationEvent) -> bool {
    matches!(event, ApplicationEvent::StateChanged { changes, .. }
        if changes.iter().any(|change| matches!(change,
            UnifiedEvent::SettingChanged { key } if key == SETTINGS_KEY)))
}

fn assert_layout_change_event(events: &mut broadcast::Receiver<ApplicationEvent>) {
    for _ in 0..32 {
        match events.try_recv() {
            Ok(event) if event_has_layout_change(&event) => return,
            Ok(_) | Err(TryRecvError::Lagged(_)) => {}
            Err(TryRecvError::Empty | TryRecvError::Closed) => break,
        }
    }
    panic!("expected a workspace layout setting change event");
}

fn assert_no_layout_change_event(events: &mut broadcast::Receiver<ApplicationEvent>) {
    loop {
        match events.try_recv() {
            Ok(event) => assert!(!event_has_layout_change(&event)),
            Err(TryRecvError::Lagged(_)) => continue,
            Err(TryRecvError::Empty | TryRecvError::Closed) => break,
        }
    }
}

fn raw_layout_value(app: &LitecordApp) -> Option<String> {
    app.database()
        .read(|conn| repos::settings::get_raw(conn, SETTINGS_KEY))
        .unwrap()
}

#[tokio::test]
async fn typed_profile_crud_and_docked_layout_survive_restart() {
    let temp = tempfile::tempdir().unwrap();
    let mut config = test_config();
    config.data_dir = temp.path().join("data");

    let app = start_app(config.clone()).await;
    wait_until_quiet(&app).await;
    let mut events = app.subscribe();

    let initial = app.layout_profiles_view().unwrap();
    let original_id = app
        .create_layout_profile("Workspace", &initial.storage_token)
        .unwrap();
    assert_layout_change_event(&mut events);

    let created = app.layout_profiles_view().unwrap();
    app.rename_layout_profile(&original_id, "Main workspace", &created.storage_token)
        .unwrap();
    let renamed = app.layout_profiles_view().unwrap();
    let duplicate_id = app
        .duplicate_layout_profile(&original_id, "Workspace copy", &renamed.storage_token)
        .unwrap();
    let duplicated = app.layout_profiles_view().unwrap();
    app.activate_layout_profile(&duplicate_id, &duplicated.storage_token)
        .unwrap();

    let active = app.layout_profiles_view().unwrap();
    let mut profile = active.profiles.active().unwrap().clone();
    let messages = profile
        .destinations
        .get_mut(&Destination::Messages)
        .unwrap();
    messages
        .dock("inspector", "main", Placement::Bottom)
        .unwrap();
    app.save_layout_profile(profile, &active.storage_token)
        .unwrap();

    let saved = app.layout_profiles_view().unwrap();
    assert_eq!(saved.profiles.active_profile_id, duplicate_id);
    let saved_messages = saved
        .profiles
        .active()
        .unwrap()
        .destinations
        .get(&Destination::Messages)
        .unwrap();
    assert!(matches!(
        saved_messages.find("inspector"),
        Some(LayoutNode::Panel {
            placement: Placement::Bottom,
            ..
        })
    ));

    app.shutdown().await;
    drop(app);

    let reopened = start_app(config).await;
    wait_until_quiet(&reopened).await;
    let restored = reopened.layout_profiles_view().unwrap();
    assert_eq!(restored.profiles.active_profile_id, duplicate_id);
    let restored_messages = restored
        .profiles
        .active()
        .unwrap()
        .destinations
        .get(&Destination::Messages)
        .unwrap();
    assert!(matches!(
        restored_messages.find("inspector"),
        Some(LayoutNode::Panel {
            placement: Placement::Bottom,
            ..
        })
    ));

    reopened
        .reset_layout_profile(&duplicate_id, &restored.storage_token)
        .unwrap();
    let reset = reopened.layout_profiles_view().unwrap();
    let reset_messages = reset
        .profiles
        .active()
        .unwrap()
        .destinations
        .get(&Destination::Messages)
        .unwrap();
    assert_eq!(reset_messages, &default_workspace(Destination::Messages));

    reopened
        .delete_layout_profile(&duplicate_id, &reset.storage_token)
        .unwrap();
    let after_active_delete = reopened.layout_profiles_view().unwrap();
    assert_eq!(
        after_active_delete.profiles.active_profile_id,
        "profile_default"
    );
    reopened
        .delete_layout_profile(&original_id, &after_active_delete.storage_token)
        .unwrap();

    let before_final_delete = reopened.layout_profiles_view().unwrap();
    assert_eq!(before_final_delete.profiles.profiles.len(), 1);
    assert!(reopened
        .delete_layout_profile("profile_default", &before_final_delete.storage_token)
        .is_err());
    let after_final_delete = reopened.layout_profiles_view().unwrap();
    assert_eq!(after_final_delete.profiles, before_final_delete.profiles);
    assert_eq!(
        after_final_delete.profiles.active_profile_id,
        before_final_delete.profiles.active_profile_id
    );
    assert_eq!(
        after_final_delete.storage_token,
        before_final_delete.storage_token
    );
    reopened.shutdown().await;
    drop(reopened);
}

#[tokio::test]
async fn stale_tokens_and_failed_mutations_preserve_the_saved_document() {
    let app = start_in_memory().await;
    wait_until_quiet(&app).await;

    let initial = app.layout_profiles_view().unwrap();
    let original_token = initial.storage_token.clone();
    let first_id = app
        .create_layout_profile("Fresh profile", &original_token)
        .unwrap();
    let after_first = app.layout_profiles_view().unwrap();
    let before_stale_revision = app.database().current_revision().unwrap();

    let stale_result = app.create_layout_profile("Stale profile", &original_token);
    assert_eq!(stale_result.unwrap_err().kind(), ErrorKind::Validation);
    let after_stale = app.layout_profiles_view().unwrap();
    assert_eq!(after_stale.profiles, after_first.profiles);
    assert_eq!(
        after_stale.profiles.active_profile_id,
        after_first.profiles.active_profile_id
    );
    assert_eq!(after_stale.storage_token, after_first.storage_token);
    assert_eq!(
        app.database().current_revision().unwrap(),
        before_stale_revision
    );

    assert!(app
        .delete_layout_profile("profile_default", &after_stale.storage_token)
        .is_ok());
    let only_profile = app.layout_profiles_view().unwrap();
    assert_eq!(only_profile.profiles.profiles.len(), 1);
    let before_last_delete = only_profile;
    assert!(app
        .delete_layout_profile(&first_id, &before_last_delete.storage_token)
        .is_err());
    let after_last_delete = app.layout_profiles_view().unwrap();
    assert_eq!(after_last_delete.profiles, before_last_delete.profiles);
    assert_eq!(
        after_last_delete.profiles.active_profile_id,
        before_last_delete.profiles.active_profile_id
    );
    app.shutdown().await;
}

#[tokio::test]
async fn no_op_update_preserves_revision_and_publishes_no_layout_event() {
    let app = start_in_memory().await;
    wait_until_quiet(&app).await;

    let mut events = app.subscribe();
    let initial = app.layout_profiles_view().unwrap();
    let id = app
        .create_layout_profile("Same name", &initial.storage_token)
        .unwrap();
    assert_layout_change_event(&mut events);

    let before = app.layout_profiles_view().unwrap();
    let revision = app.database().current_revision().unwrap();
    app.rename_layout_profile(&id, "Same name", &before.storage_token)
        .unwrap();
    let after = app.layout_profiles_view().unwrap();

    assert_eq!(after.profiles, before.profiles);
    assert_eq!(after.storage_token, before.storage_token);
    assert_eq!(app.database().current_revision().unwrap(), revision);
    assert_no_layout_change_event(&mut events);
    app.shutdown().await;
}

#[tokio::test]
async fn corrupt_and_future_layout_data_is_preserved_until_explicit_reset() {
    let app = start_in_memory().await;
    wait_until_quiet(&app).await;

    app.database()
        .write(|tx| -> litecord_core::Result<()> {
            tx.execute(
                "INSERT INTO settings (key, value, updated_at) VALUES ('workspace.layout_profiles', '{', 0) \
                 ON CONFLICT(key) DO UPDATE SET value = '{', updated_at = 0",
                [],
            )
            .map_err(|error| litecord_core::Error::internal(error.to_string()))?;
            Ok(())
        })
        .unwrap();
    let malformed_raw = raw_layout_value(&app).unwrap();
    assert_eq!(malformed_raw, "{");

    let malformed = app.layout_profiles_view().unwrap();
    assert!(malformed.recovery_notice.is_some());
    assert_eq!(malformed.profiles, LayoutProfiles::default());
    assert!(app
        .create_layout_profile("Blocked until reset", &malformed.storage_token)
        .is_err());
    let still_malformed = app.layout_profiles_view().unwrap();
    assert_eq!(
        raw_layout_value(&app).as_deref(),
        Some(malformed_raw.as_str())
    );
    assert_eq!(still_malformed.profiles, malformed.profiles);
    assert_eq!(
        still_malformed.profiles.active_profile_id,
        malformed.profiles.active_profile_id
    );

    app.reset_all_layout_profiles(&still_malformed.storage_token)
        .unwrap();
    let reset_malformed = app.layout_profiles_view().unwrap();
    assert!(reset_malformed.recovery_notice.is_none());
    assert_eq!(reset_malformed.profiles, LayoutProfiles::default());
    assert_ne!(
        raw_layout_value(&app).as_deref(),
        Some(malformed_raw.as_str())
    );

    let future_profiles = LayoutProfiles {
        version: FORMAT_VERSION + 1,
        ..LayoutProfiles::default()
    };
    let future_value = serde_json::to_value(future_profiles).unwrap();
    app.database()
        .write(|tx| repos::settings::set_json(tx, SETTINGS_KEY, &future_value))
        .unwrap();
    let future_raw = raw_layout_value(&app).unwrap();
    let future = app.layout_profiles_view().unwrap();
    assert!(future.recovery_notice.is_some());
    assert_eq!(future.profiles, LayoutProfiles::default());
    assert!(app
        .rename_layout_profile("profile_default", "Blocked", &future.storage_token)
        .is_err());
    let still_future = app.layout_profiles_view().unwrap();
    assert_eq!(raw_layout_value(&app).as_deref(), Some(future_raw.as_str()));
    assert_eq!(still_future.profiles, future.profiles);
    assert_eq!(
        still_future.profiles.active_profile_id,
        future.profiles.active_profile_id
    );

    app.reset_all_layout_profiles(&still_future.storage_token)
        .unwrap();
    let reset_future = app.layout_profiles_view().unwrap();
    assert!(reset_future.recovery_notice.is_none());
    assert_eq!(reset_future.profiles, LayoutProfiles::default());
    assert_ne!(raw_layout_value(&app).as_deref(), Some(future_raw.as_str()));
    app.shutdown().await;
}

#[tokio::test]
async fn generic_setting_api_rejects_the_reserved_layout_key() {
    let app = start_in_memory().await;
    wait_until_quiet(&app).await;
    let before = app.layout_profiles_view().unwrap();

    let error = app
        .set_setting(SETTINGS_KEY, serde_json::json!({"version": 99}))
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Validation);

    let after = app.layout_profiles_view().unwrap();
    assert_eq!(after.profiles, before.profiles);
    assert_eq!(after.storage_token, before.storage_token);
    assert!(raw_layout_value(&app).is_none());
    app.shutdown().await;
}
