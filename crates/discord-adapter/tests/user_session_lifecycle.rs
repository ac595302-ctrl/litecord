#![allow(clippy::unwrap_used)]

use discord_adapter::bot::transport::fake::FakeTransport;
use discord_adapter::user_session::UserSessionBackend;
use litecord_core::bus::ingest_channel;
use litecord_core::clock::SystemClock;
use litecord_core::events::DiscordEvent;
use litecord_core::ports::SocialBackend;
use litecord_core::secrets::{
    InMemorySecretStore, Secret, SecretKey, SecretStore, SecretStoreError,
};
use litecord_types::social::SessionState;
use serde_json::json;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(Debug, Default)]
struct FailingStore {
    inner: InMemorySecretStore,
    fail_set: AtomicBool,
    fail_remove: AtomicBool,
}

impl SecretStore for FailingStore {
    fn get(&self, key: SecretKey) -> Option<Secret<String>> {
        self.inner.get(key)
    }
    fn set(&self, key: SecretKey, value: Secret<String>) {
        self.inner.set(key, value);
    }
    fn remove(&self, key: SecretKey) {
        self.inner.remove(key);
    }
    fn try_set(&self, key: SecretKey, value: Secret<String>) -> Result<(), SecretStoreError> {
        if self.fail_set.load(Ordering::Relaxed) {
            return Err(SecretStoreError);
        }
        self.inner.try_set(key, value)
    }
    fn try_remove(&self, key: SecretKey) -> Result<(), SecretStoreError> {
        if self.fail_remove.load(Ordering::Relaxed) {
            return Err(SecretStoreError);
        }
        self.inner.try_remove(key)
    }
}

#[tokio::test]
async fn failed_durable_logout_stops_access_and_reports_error_until_retry_succeeds() {
    let transport = FakeTransport::new();
    transport.respond("GET /users/@me", json!({"id":"1","username":"owner"}));
    let store = Arc::new(FailingStore::default());
    store.set(
        SecretKey::DiscordUserSessionToken,
        Secret::new("dummy".into()),
    );
    store.fail_remove.store(true, Ordering::Relaxed);
    let backend =
        UserSessionBackend::new(Arc::new(transport), store.clone(), Arc::new(SystemClock));
    let (sink, mut rx) = ingest_channel(64);
    backend.connect(sink).await.unwrap();
    let error = backend.sign_out().await.unwrap_err();
    assert!(error.to_string().contains("Credential removal failed"));
    assert!(store.get(SecretKey::DiscordUserSessionToken).is_some());
    assert!(backend.current_user().await.is_err());
    let mut saw_error = false;
    while let Ok(Some(env)) =
        tokio::time::timeout(std::time::Duration::from_millis(20), rx.recv()).await
    {
        if let DiscordEvent::SessionChanged { state } = env.event {
            assert_ne!(state, SessionState::LoggedOut);
            saw_error |= matches!(state, SessionState::Error { .. });
        }
    }
    assert!(saw_error);
    store.fail_remove.store(false, Ordering::Relaxed);
    backend.sign_out().await.unwrap();
    assert!(store.get(SecretKey::DiscordUserSessionToken).is_none());
    assert!(matches!(
        rx.recv().await.unwrap().event,
        DiscordEvent::SessionChanged {
            state: SessionState::LoggedOut
        }
    ));
}

#[tokio::test]
async fn failed_persistence_does_not_pin_the_unaccepted_account() {
    let transport = FakeTransport::new();
    let store = Arc::new(FailingStore::default());
    store.fail_set.store(true, Ordering::Relaxed);
    let backend =
        UserSessionBackend::new(Arc::new(transport.clone()), store, Arc::new(SystemClock));
    for id in ["1", "2"] {
        transport.respond("GET /users/@me", json!({"id":id,"username":"owner"}));
        let error = backend
            .authenticate_session(Secret::new("dummy".into()))
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("could not save"),
            "a rejected credential must not establish an account pin: {error}"
        );
    }
    backend.bind_account(litecord_types::UserId(1)).unwrap();
    let error = backend
        .authenticate_session(Secret::new("dummy".into()))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("different account"));
}
