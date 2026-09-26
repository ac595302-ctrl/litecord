//! Ports: interfaces the core needs from the outside world.
//!
//! [`SocialBackend`] is the *only* way the rest of Litecord talks to Discord.
//! Implementations live in `discord-adapter` (mock/demo, Social SDK) and later
//! a bot bridge. Hydration reads through it; the Action Engine's executor
//! writes through it. UI, memory, retrieval, context and MCP never see it.
//!
//! Optional operations have default implementations returning
//! [`BackendError::Unsupported`] — a backend declares support explicitly by
//! overriding them *and* reporting it in [`SocialBackend::capabilities`].
//! Nothing is faked.

use async_trait::async_trait;

use litecord_types::actions::{MessageTarget, PresenceDraft, RelationshipAction};
use litecord_types::capability::{BackendMode, Capability, CapabilitySet, DiscordTarget};
use litecord_types::ids::*;
use litecord_types::provenance::DiscordSource;
use litecord_types::social::*;
use litecord_types::DurationMs;

use crate::bus::IngestSender;
use crate::error::{Error, ErrorKind};

/// Errors a backend may return. Messages are sanitized (never tokens).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BackendError {
    #[error("backend offline")]
    Offline,
    #[error("backend not connected")]
    NotConnected,
    #[error("capability {capability:?} is not supported by this backend")]
    Unsupported { capability: Capability },
    #[error("{what} not found")]
    NotFound { what: String },
    #[error("rate limited; retry after {retry_after:?}")]
    RateLimited { retry_after: DurationMs },
    #[error("authentication failed: {0}")]
    Authentication(String),
    #[error("sdk error: {0}")]
    Sdk(String),
}

impl BackendError {
    /// Whether retrying later (with backoff) may succeed.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            BackendError::Offline
                | BackendError::NotConnected
                | BackendError::RateLimited { .. }
                | BackendError::Sdk(_)
        )
    }
}

impl From<BackendError> for Error {
    fn from(e: BackendError) -> Self {
        let kind = match &e {
            BackendError::Offline | BackendError::NotConnected => ErrorKind::Offline,
            BackendError::Unsupported { .. } => ErrorKind::Unsupported,
            BackendError::NotFound { .. } => ErrorKind::NotFound,
            BackendError::Authentication(_) => ErrorKind::Authentication,
            BackendError::RateLimited { .. } | BackendError::Sdk(_) => ErrorKind::Discord,
        };
        Error::with_source(kind, e.to_string(), e)
    }
}

pub type BackendResult<T> = Result<T, BackendError>;

/// Local voice controls. Always user-initiated.
#[derive(Debug, Clone, PartialEq)]
pub enum VoiceControl {
    JoinLobby(LobbyId),
    Leave,
    SetMuted(bool),
    SetDeafened(bool),
    SetInputDevice(String),
    SetOutputDevice(String),
    SetOutputVolume(f32),
    SetNoiseSuppression(bool),
    SetPushToTalk(bool),
}

fn unsupported<T>(capability: Capability) -> BackendResult<T> {
    Err(BackendError::Unsupported { capability })
}

/// A Discord-facing data source/sink.
///
/// Thread-safety: implementations must be `Send + Sync`; methods take `&self`
/// and may be called concurrently (hydration runs a small bounded number of
/// jobs in parallel).
#[async_trait]
pub trait SocialBackend: Send + Sync + std::fmt::Debug + 'static {
    /// Provenance stamped onto everything this backend produces.
    fn source(&self) -> DiscordSource;

    fn mode(&self) -> BackendMode;

    /// What this backend can actually do right now.
    fn capabilities(&self) -> CapabilitySet;

    /// Start delivering live events into `sink`. Idempotent. Implementations
    /// on callback threads must use `IngestSender::try_send`.
    async fn connect(&self, sink: IngestSender) -> BackendResult<()>;

    async fn disconnect(&self) -> BackendResult<()>;

    async fn current_user(&self) -> BackendResult<User>;

    async fn user(&self, _user_id: UserId) -> BackendResult<User> {
        unsupported(Capability::CurrentUser)
    }

    /// Relationships with the related user profile when available.
    async fn relationships(&self) -> BackendResult<Vec<(Relationship, Option<User>)>> {
        unsupported(Capability::Friends)
    }

    async fn presence(&self, _user_id: UserId) -> BackendResult<Presence> {
        unsupported(Capability::Presence)
    }

    async fn guilds(&self) -> BackendResult<Vec<Guild>> {
        unsupported(Capability::GuildListing)
    }

    async fn guild_channels(&self, _guild_id: GuildId) -> BackendResult<Vec<Channel>> {
        unsupported(Capability::GuildChannels)
    }

    /// DM/conversation summaries.
    async fn conversations(&self) -> BackendResult<Vec<Conversation>> {
        unsupported(Capability::DmList)
    }

    /// Most recent messages (newest last), bounded by `limit` and the
    /// backend's history capability.
    async fn messages(
        &self,
        _conversation_id: ConversationId,
        _limit: u32,
    ) -> BackendResult<Vec<Message>> {
        unsupported(Capability::DmHistory)
    }

    async fn lobby(&self, _lobby_id: LobbyId) -> BackendResult<Lobby> {
        unsupported(Capability::Lobbies)
    }

    async fn voice_state(&self) -> BackendResult<VoiceState> {
        unsupported(Capability::Voice)
    }

    async fn voice_control(&self, _control: VoiceControl) -> BackendResult<VoiceState> {
        unsupported(Capability::Voice)
    }

    /// Available audio devices (inputs and outputs).
    async fn audio_devices(&self) -> BackendResult<Vec<AudioDevice>> {
        unsupported(Capability::VoiceDevices)
    }

    // ---- Writes. Only the Action Engine's executor may call these. ----

    async fn send_message(
        &self,
        _target: &MessageTarget,
        _content: &str,
    ) -> BackendResult<Message> {
        unsupported(Capability::DmSend)
    }

    async fn edit_message(&self, _message_id: MessageId, _content: &str) -> BackendResult<()> {
        unsupported(Capability::DmEdit)
    }

    async fn delete_message(&self, _message_id: MessageId) -> BackendResult<()> {
        unsupported(Capability::DmDelete)
    }

    async fn set_presence(&self, _presence: &PresenceDraft) -> BackendResult<()> {
        unsupported(Capability::RichPresence)
    }

    async fn relationship_action(
        &self,
        _user_id: UserId,
        action: RelationshipAction,
    ) -> BackendResult<()> {
        unsupported(match action {
            RelationshipAction::Block | RelationshipAction::Unblock => Capability::Blocking,
            _ => Capability::FriendRequests,
        })
    }

    /// External fallback link for content this backend cannot show.
    fn external_link(&self, target: &DiscordTarget) -> String {
        target.web_url()
    }
}
