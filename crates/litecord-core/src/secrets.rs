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
    DatabaseKey,
}

impl SecretKey {
    pub const fn as_str(self) -> &'static str {
        match self {
            SecretKey::DiscordAccessToken => "discord.access_token",
            SecretKey::DiscordRefreshToken => "discord.refresh_token",
            SecretKey::DiscordBotToken => "discord.bot_token",
            SecretKey::DatabaseKey => "database.key",
        }
    }
}

/// Storage for credentials. A production implementation should use the OS
/// keychain; that is deferred (see docs/IMPLEMENTATION_STATUS.md).
pub trait SecretStore: Send + Sync + fmt::Debug {
    fn get(&self, key: SecretKey) -> Option<Secret<String>>;
    fn set(&self, key: SecretKey, value: Secret<String>);
    fn remove(&self, key: SecretKey);
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
}
