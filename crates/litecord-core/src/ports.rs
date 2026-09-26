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
use litecord_types::capability::{AuthStep, BackendMode, Capability, CapabilitySet, DiscordTarget};
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
    #[error("permission denied: {what}")]
    PermissionDenied { what: String },
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
            BackendError::PermissionDenied { .. } => ErrorKind::Discord,
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

/// Largest page a [`SocialBackend::history_page`] call returns (Discord's
/// own REST bound).
pub const MAX_HISTORY_PAGE: u32 = 100;

/// One request for a page of a conversation's history.
///
/// Cursor semantics (both exclusive, by message id order):
/// * neither cursor: the newest `limit` messages;
/// * `before`: the newest `limit` messages strictly older than `before`
///   (walking backwards);
/// * `after`: the oldest `limit` messages strictly newer than `after`
///   (catching up forwards);
/// * both: the oldest `limit` messages strictly between them.
///
/// `limit` is clamped to `1..=`[`MAX_HISTORY_PAGE`] (see
/// [`HistoryPageRequest::effective_limit`]).
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryPageRequest {
    pub conversation_id: ConversationId,
    pub before: Option<MessageId>,
    pub after: Option<MessageId>,
    pub limit: u32,
}

impl HistoryPageRequest {
    /// The newest `limit` messages (no cursor).
    pub fn latest(conversation_id: ConversationId, limit: u32) -> Self {
        Self {
            conversation_id,
            before: None,
            after: None,
            limit,
        }
    }

    /// `limit` messages strictly older than `before`.
    pub fn before(conversation_id: ConversationId, before: MessageId, limit: u32) -> Self {
        Self {
            before: Some(before),
            ..Self::latest(conversation_id, limit)
        }
    }

    /// `limit` messages strictly newer than `after`.
    pub fn after(conversation_id: ConversationId, after: MessageId, limit: u32) -> Self {
        Self {
            after: Some(after),
            ..Self::latest(conversation_id, limit)
        }
    }

    /// `limit` clamped to `1..=MAX_HISTORY_PAGE`.
    pub fn effective_limit(&self) -> u32 {
        self.limit.clamp(1, MAX_HISTORY_PAGE)
    }
}

/// A page of history, oldest first.
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryPage {
    /// Oldest first.
    pub messages: Vec<Message>,
    /// Whether more messages exist beyond this page in the requested
    /// direction (older for `before`/no cursor, newer for `after`).
    pub has_more: bool,
}

impl HistoryPage {
    /// Id of the oldest message in the page.
    pub fn oldest(&self) -> Option<MessageId> {
        self.messages.iter().map(|m| m.id).min()
    }

    /// Id of the newest message in the page.
    pub fn newest(&self) -> Option<MessageId> {
        self.messages.iter().map(|m| m.id).max()
    }
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

    /// Bind an account source to the database's previously connected account.
    fn bind_account(&self, _account: UserId) -> BackendResult<()> {
        Ok(())
    }

    /// Epoch used to reject responses from a cancelled account session.
    fn session_generation(&self) -> Option<(std::sync::Arc<std::sync::atomic::AtomicU64>, u64)> {
        None
    }

    /// Start delivering live events into `sink`. Idempotent. Implementations
    /// on callback threads must use `IngestSender::try_send`.
    async fn connect(&self, sink: IngestSender) -> BackendResult<()>;

    async fn disconnect(&self) -> BackendResult<()>;

    async fn current_user(&self) -> BackendResult<User>;

    /// Start signing in. Backends that need no interactive auth (demo,
    /// bot token) return `AlreadySignedIn`.
    async fn begin_sign_in(&self) -> BackendResult<AuthStep> {
        Ok(AuthStep::AlreadySignedIn)
    }

    /// Finish signing in with the OAuth redirect URL. Implementations must
    /// verify the `state` parameter and keep tokens in a `SecretStore`;
    /// nothing about the tokens may be returned.
    async fn complete_sign_in(&self, _redirect_url: &str) -> BackendResult<()> {
        Err(BackendError::Authentication(
            "this backend has no interactive sign-in".into(),
        ))
    }

    /// Explicit account-owner supplied session credential. This is a distinct
    /// experimental auth flow, never an OAuth redirect or a model-facing tool.
    async fn authenticate_session(
        &self,
        _credential: crate::secrets::Secret<String>,
    ) -> BackendResult<()> {
        Err(BackendError::Authentication(
            "this backend does not accept session credentials".into(),
        ))
    }

    /// Sign out: forget credentials and disconnect. Emits
    /// `SessionChanged{LoggedOut}`.
    async fn sign_out(&self) -> BackendResult<()> {
        self.disconnect().await
    }

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

    /// One page of history (see [`HistoryPageRequest`] for cursor
    /// semantics). The default serves only the cursor-less "latest" page via
    /// [`SocialBackend::messages`] (`has_more` = the page came back full);
    /// cursor paging is unsupported unless a backend overrides this.
    async fn history_page(&self, req: &HistoryPageRequest) -> BackendResult<HistoryPage> {
        if req.before.is_some() || req.after.is_some() {
            return unsupported(Capability::DmHistory);
        }
        let limit = req.effective_limit();
        let messages = self.messages(req.conversation_id, limit).await?;
        let has_more = messages.len() == limit as usize;
        Ok(HistoryPage { messages, has_more })
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

    /// Send `content` as a reply to `reply_to`. Backends without reply
    /// support must refuse rather than send a plain message.
    async fn send_reply(
        &self,
        _target: &MessageTarget,
        _content: &str,
        _reply_to: MessageId,
    ) -> BackendResult<Message> {
        unsupported(Capability::Replies)
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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// A backend that only implements the required methods plus `messages`.
    #[derive(Debug)]
    struct Minimal;

    #[async_trait]
    impl SocialBackend for Minimal {
        fn source(&self) -> DiscordSource {
            DiscordSource::Synthetic
        }
        fn mode(&self) -> BackendMode {
            BackendMode::Demo
        }
        fn capabilities(&self) -> CapabilitySet {
            CapabilitySet::default()
        }
        async fn connect(&self, _sink: IngestSender) -> BackendResult<()> {
            Ok(())
        }
        async fn disconnect(&self) -> BackendResult<()> {
            Ok(())
        }
        async fn current_user(&self) -> BackendResult<User> {
            Err(BackendError::NotConnected)
        }
        async fn messages(
            &self,
            conversation_id: ConversationId,
            limit: u32,
        ) -> BackendResult<Vec<Message>> {
            // Three messages exist; return the newest `limit`.
            let all: Vec<Message> = (1..=3u64)
                .map(|i| Message {
                    id: MessageId(i),
                    conversation_id,
                    author_id: UserId(9),
                    content: "m".into(),
                    sent_at: litecord_types::Timestamp::from_millis(i as i64),
                    edited_at: None,
                    reply_to: None,
                    extras: Vec::new(),
                })
                .collect();
            let start = all.len().saturating_sub(limit as usize);
            Ok(all[start..].to_vec())
        }
    }

    #[tokio::test]
    async fn default_history_page_serves_latest_only() {
        let b = Minimal;
        let conv = ConversationId(1);

        let page = b
            .history_page(&HistoryPageRequest::latest(conv, 2))
            .await
            .unwrap();
        assert_eq!(page.messages.len(), 2);
        assert!(page.has_more);
        assert_eq!(page.oldest(), Some(MessageId(2)));
        assert_eq!(page.newest(), Some(MessageId(3)));

        let page = b
            .history_page(&HistoryPageRequest::latest(conv, 10))
            .await
            .unwrap();
        assert_eq!(page.messages.len(), 3);
        assert!(!page.has_more);

        let err = b
            .history_page(&HistoryPageRequest::before(conv, MessageId(2), 10))
            .await
            .unwrap_err();
        assert_eq!(
            err,
            BackendError::Unsupported {
                capability: Capability::DmHistory
            }
        );
    }

    #[test]
    fn history_limit_is_clamped() {
        let conv = ConversationId(1);
        assert_eq!(HistoryPageRequest::latest(conv, 0).effective_limit(), 1);
        assert_eq!(HistoryPageRequest::latest(conv, 500).effective_limit(), 100);
        assert_eq!(HistoryPageRequest::latest(conv, 50).effective_limit(), 50);
        assert_eq!(
            HistoryPage {
                messages: vec![],
                has_more: false
            }
            .oldest(),
            None
        );
    }
}
