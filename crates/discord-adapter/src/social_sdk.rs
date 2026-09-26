//! **Skeleton only.** This is not a working Discord Social SDK backend.
//!
//! The native bridge in `discord-ffi` (`native/discord_bridge.cpp`) is a
//! sketch written against Discord Social SDK 1.x headers and has **not been
//! compiled or run against a real SDK checkout** — see
//! `crates/discord-ffi/README.md` and `docs/IMPLEMENTATION_STATUS.md`. Until
//! that bridge is verified, [`SocialSdkBackend`] must never fabricate or
//! guess at Discord state: every operation here returns
//! [`BackendError::NotConnected`] (for the methods `SocialBackend` requires
//! every backend to implement) or the trait's own default
//! [`BackendError::Unsupported`] (for every optional operation, left at its
//! default — see `litecord_core::ports::SocialBackend`), and
//! [`SocialSdkBackend::capabilities`] reports an entirely empty
//! [`CapabilitySet`] so nothing above this backend ever assumes a capability
//! it doesn't have.
//!
//! When the bridge is verified, this module is where it gets wired up:
//! `discord_ffi::Bridge::run_callbacks` pumped on a dedicated thread,
//! translating `discord_ffi::LcEvent`s into [`DiscordEvent`]s (resolving ids
//! via `discord_ffi::Bridge::current_user`/`get_message` and
//! [`crate::convert`]) and pushing them through the `IngestSender` given to
//! `connect`. Until then, this file intentionally contains no reference to
//! `discord_ffi::Bridge` at all: there is nothing here to "almost work".

use async_trait::async_trait;

use litecord_core::bus::IngestSender;
use litecord_core::ports::{BackendError, BackendResult, SocialBackend};
use litecord_core::secrets::SecretStore;
use litecord_types::capability::{BackendMode, CapabilitySet};
use litecord_types::provenance::DiscordSource;
use litecord_types::social::User;
use std::sync::Arc;

/// A backend over the real Discord Social SDK, gated behind the
/// `discord-social-sdk` feature. See the module docs: this is a skeleton.
pub struct SocialSdkBackend {
    application_id: u64,
    #[allow(
        dead_code,
        reason = "held for the future real implementation; unused by the skeleton"
    )]
    secrets: Arc<dyn SecretStore>,
}

impl std::fmt::Debug for SocialSdkBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SocialSdkBackend")
            .field("application_id", &self.application_id)
            .finish_non_exhaustive()
    }
}

impl SocialSdkBackend {
    /// Builds a skeleton backend for the given Discord application id. Does
    /// not connect, does not touch `discord-ffi`, and does not read any
    /// secret from `secrets` yet.
    pub fn new(application_id: u64, secrets: Arc<dyn SecretStore>) -> Self {
        Self {
            application_id,
            secrets,
        }
    }
}

#[async_trait]
impl SocialBackend for SocialSdkBackend {
    fn source(&self) -> DiscordSource {
        DiscordSource::SocialSdk
    }

    fn mode(&self) -> BackendMode {
        BackendMode::FullSocialSdk
    }

    /// Deliberately empty: this skeleton supports nothing yet. See the
    /// module docs.
    fn capabilities(&self) -> CapabilitySet {
        CapabilitySet::default()
    }

    async fn connect(&self, _sink: IngestSender) -> BackendResult<()> {
        Err(BackendError::NotConnected)
    }

    async fn disconnect(&self) -> BackendResult<()> {
        Err(BackendError::NotConnected)
    }

    async fn current_user(&self) -> BackendResult<User> {
        Err(BackendError::NotConnected)
    }

    // Every other `SocialBackend` method keeps the trait's default
    // implementation, which returns `BackendError::Unsupported` with the
    // matching `Capability` — consistent with `capabilities()` reporting
    // none of them as supported.
}

#[cfg(test)]
mod tests {
    use super::*;
    use litecord_core::secrets::InMemorySecretStore;
    use litecord_types::capability::Capability;

    #[tokio::test]
    async fn skeleton_never_fakes_data() {
        let backend = SocialSdkBackend::new(123, Arc::new(InMemorySecretStore::default()));
        assert!(backend.capabilities().entries.is_empty());
        assert_eq!(
            backend.current_user().await,
            Err(BackendError::NotConnected)
        );
        assert_eq!(
            backend.relationships().await,
            Err(BackendError::Unsupported {
                capability: Capability::Friends
            })
        );
    }
}
