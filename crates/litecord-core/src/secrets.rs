//! Credential isolation.
//!
//! [`Secret<T>`] deliberately does **not** implement `Serialize`, `Display`
//! with contents, or `Clone`-into-string conversions; its `Debug` prints
//! `[REDACTED]`. A value must be explicitly `expose_secret()`ed at the single
//! point of use (the Discord adapter). Model-facing crates (context, agent,
//! MCP) do not depend on anything that holds a [`SecretStore`].

use std::collections::HashMap;
use std::fmt;
use std::sync::RwLock;

pub struct Secret<T>(T);

impl<T> Secret<T> {
    pub fn new(value: T) -> Self {
        Self(value)
    }

    /// Access the secret value. Call sites should be few and auditable.
    pub fn expose_secret(&self) -> &T {
        &self.0
    }
}

impl<T> fmt::Debug for Secret<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret([REDACTED])")
    }
}

impl<T> fmt::Display for Secret<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}

impl<T> From<T> for Secret<T> {
    fn from(v: T) -> Self {
        Secret(v)
    }
}

/// Well-known credential slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SecretKey {
    DiscordAccessToken,
    DiscordRefreshToken,
    DiscordBotToken,
    DiscordUserSessionToken,
    DatabaseKey,
}

impl SecretKey {
    pub const fn as_str(self) -> &'static str {
        match self {
            SecretKey::DiscordAccessToken => "discord.access_token",
            SecretKey::DiscordRefreshToken => "discord.refresh_token",
            SecretKey::DiscordBotToken => "discord.bot_token",
            SecretKey::DiscordUserSessionToken => "discord.user_session_token",
            SecretKey::DatabaseKey => "database.key",
        }
    }
}

/// Storage for credentials. Production account sessions use `OsSecretStore`
/// under the `os-keychain` feature; tests and demo sources use process memory.
pub trait SecretStore: Send + Sync + fmt::Debug {
    fn get(&self, key: SecretKey) -> Option<Secret<String>>;
    fn set(&self, key: SecretKey, value: Secret<String>);
    fn remove(&self, key: SecretKey);

    fn try_get(&self, key: SecretKey) -> Result<Option<Secret<String>>, SecretStoreError> {
        Ok(self.get(key))
    }
    fn try_set(&self, key: SecretKey, value: Secret<String>) -> Result<(), SecretStoreError> {
        self.set(key, value);
        Ok(())
    }
    fn try_remove(&self, key: SecretKey) -> Result<(), SecretStoreError> {
        self.remove(key);
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, thiserror::Error)]
#[error("OS credential storage is unavailable")]
pub struct SecretStoreError;

/// Native credential storage. The installation namespace prevents a second
/// Litecord database from reusing the first installation's account credential.
#[cfg(feature = "os-keychain")]
#[derive(Debug)]
pub struct OsSecretStore {
    service: String,
    lock: std::sync::Mutex<()>,
}

#[cfg(feature = "os-keychain")]
impl OsSecretStore {
    pub fn new(namespace: &str) -> Self {
        Self {
            service: format!("litecord.{namespace}"),
            lock: std::sync::Mutex::new(()),
        }
    }
    fn entry(&self, key: SecretKey) -> Result<keyring::Entry, SecretStoreError> {
        keyring::Entry::new(&self.service, key.as_str()).map_err(|_| SecretStoreError)
    }
}

#[cfg(feature = "os-keychain")]
impl SecretStore for OsSecretStore {
    fn get(&self, key: SecretKey) -> Option<Secret<String>> {
        self.try_get(key).ok().flatten()
    }
    fn set(&self, key: SecretKey, value: Secret<String>) {
        let _ = self.try_set(key, value);
    }
    fn remove(&self, key: SecretKey) {
        let _ = self.try_remove(key);
    }
    fn try_get(&self, key: SecretKey) -> Result<Option<Secret<String>>, SecretStoreError> {
        let _guard = self.lock.lock().map_err(|_| SecretStoreError)?;
        match self.entry(key)?.get_password() {
            Ok(value) => Ok(Some(Secret::new(value))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(_) => Err(SecretStoreError),
        }
    }
    fn try_set(&self, key: SecretKey, value: Secret<String>) -> Result<(), SecretStoreError> {
        let _guard = self.lock.lock().map_err(|_| SecretStoreError)?;
        self.entry(key)?
            .set_password(value.expose_secret())
            .map_err(|_| SecretStoreError)
    }
    fn try_remove(&self, key: SecretKey) -> Result<(), SecretStoreError> {
        let _guard = self.lock.lock().map_err(|_| SecretStoreError)?;
        match self.entry(key)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(_) => Err(SecretStoreError),
        }
    }
}

/// Process-memory secret store (tests, demo mode, env-provided bot tokens).
#[derive(Default)]
pub struct InMemorySecretStore {
    values: RwLock<HashMap<SecretKey, String>>,
}

impl fmt::Debug for InMemorySecretStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let n = self.values.read().map(|v| v.len()).unwrap_or(0);
        write!(f, "InMemorySecretStore({n} entries, [REDACTED])")
    }
}

impl SecretStore for InMemorySecretStore {
    fn get(&self, key: SecretKey) -> Option<Secret<String>> {
        self.values
            .read()
            .ok()
            .and_then(|m| m.get(&key).cloned())
            .map(Secret::new)
    }

    fn set(&self, key: SecretKey, value: Secret<String>) {
        if let Ok(mut m) = self.values.write() {
            m.insert(key, value.0);
        }
    }

    fn remove(&self, key: SecretKey) {
        if let Ok(mut m) = self.values.write() {
            m.remove(&key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_redact_in_debug_and_display() {
        let s = Secret::new("super-secret-token".to_string());
        assert!(!format!("{s:?}").contains("super-secret"));
        assert!(!format!("{s}").contains("super-secret"));
        assert_eq!(s.expose_secret(), "super-secret-token");
    }

    #[test]
    fn store_debug_does_not_leak() {
        let store = InMemorySecretStore::default();
        store.set(SecretKey::DiscordBotToken, Secret::new("bot-xyz".into()));
        assert!(!format!("{store:?}").contains("bot-xyz"));
        assert_eq!(
            store
                .get(SecretKey::DiscordBotToken)
                .map(|s| s.expose_secret().clone()),
            Some("bot-xyz".to_string())
        );
    }

    #[cfg(feature = "os-keychain")]
    #[test]
    #[ignore = "requires an available native credential store; run explicitly on the target desktop"]
    #[allow(clippy::expect_used)]
    fn native_credential_round_trip_and_cleanup() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let store = OsSecretStore::new(&format!("pipeline-test.{}.{stamp}", std::process::id()));
        let key = SecretKey::DiscordUserSessionToken;
        store
            .try_set(key, Secret::new("native-test-sentinel".into()))
            .expect("native test credential saved");
        // Capture before cleanup, then remove even if reading fails.
        let loaded = store.try_get(key);
        let removed = store.try_remove(key);
        assert!(removed.is_ok(), "native test credential removed");
        assert!(loaded
            .is_ok_and(|value| value
                .is_some_and(|secret| secret.expose_secret() == "native-test-sentinel")));
        assert!(store.try_get(key).is_ok_and(|value| value.is_none()));
    }
}
