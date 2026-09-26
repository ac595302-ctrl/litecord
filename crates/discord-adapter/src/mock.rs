//! A real (not stubbed) [`SocialBackend`] over in-memory, synthetic state.
//!
//! [`MockBackend`] is the backend used for demo mode and for testing anything
//! above `discord-adapter`: it implements every operation for real against a
//! `Mutex<MockState>`, including write mutation and event emission, so
//! higher layers (hydration, the reducer, actions) can be developed and
//! tested without any Discord connectivity at all.
//!
//! Locking discipline: the mutex is **never held across an `.await`**. Every
//! method takes the lock, mutates/reads state, clones out whatever it needs
//! (including the `IngestSender`, which is cheap to clone), drops the guard,
//! and only then awaits (to emit an event). This matches the concurrency
//! note on `SocialBackend`: methods may be called concurrently.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use litecord_core::bus::IngestSender;
use litecord_core::clock::{SharedClock, SystemClock};
use litecord_core::events::{DiscordEvent, SourceEnvelope};
use litecord_core::ports::{BackendError, BackendResult, SocialBackend, VoiceControl};
use litecord_types::actions::{MessageTarget, PresenceDraft, RelationshipAction};
use litecord_types::capability::{
    AuthStep, BackendMode, Capability, CapabilitySet, HistoryCapability, SupportLevel,
};
use litecord_types::ids::*;
use litecord_types::provenance::DiscordSource;
use litecord_types::social::*;
use litecord_types::Timestamp;

use crate::fixtures::{self, DemoData};

const MOCK_CLIENT_ID: u64 = 1;
const MOCK_REDIRECT_URI: &str = "http://127.0.0.1:53134/callback";

/// Mutable state behind [`MockBackend`]. Kept behind a single `Mutex` since
/// this is a test/demo backend, not a hot path.
#[derive(Debug)]
struct MockState {
    current_user: User,
    users: HashMap<UserId, User>,
    relationships: HashMap<UserId, Relationship>,
    presences: HashMap<UserId, Presence>,
    guilds: Vec<Guild>,
    channels: Vec<Channel>,
    conversations: HashMap<ConversationId, Conversation>,
    /// Messages per conversation, kept sorted oldest-first.
    messages: HashMap<ConversationId, Vec<Message>>,
    lobbies: HashMap<LobbyId, Lobby>,
    voice: VoiceState,
    next_conversation_id: u64,
    next_message_id: u64,
    sink: Option<IngestSender>,
    /// Set true by `connect`, false by `disconnect`. Distinct from `offline`:
    /// a connected backend can still be toggled offline and back.
    connected: bool,
    offline: bool,
    /// Errors to return from the next N *read* calls (`fail_next`).
    fail_queue: VecDeque<BackendError>,
    /// Invocation counts per trait method name, for `calls()`.
    calls: HashMap<&'static str, usize>,
    /// Interactive sign-in simulation (`with_sign_in_required`).
    auth_required: bool,
    signed_in: bool,
    pending_sign_in: Option<crate::oauth::PkceSession>,
}

/// A fully working, in-memory [`SocialBackend`] over deterministic synthetic
/// data. Source is always [`DiscordSource::Synthetic`]; everything it
/// produces is persisted with `Origin::Synthetic` by the reducer, so it can
/// never be mistaken for real Discord content.
pub struct MockBackend {
    state: Mutex<MockState>,
    clock: SharedClock,
    /// `Synthetic` (user view) or `SyntheticBot` (application-bot view).
    source: DiscordSource,
}

impl std::fmt::Debug for MockBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MockBackend").finish_non_exhaustive()
    }
}

impl MockBackend {
    /// Builds a backend from already-constructed [`DemoData`], using the
    /// system clock.
    pub fn new(data: DemoData) -> Self {
        Self::with_clock(data, Arc::new(SystemClock))
    }

    /// Builds a backend from [`DemoData`], with an injected clock (tests can
    /// pass `litecord_core::clock::ManualClock`).
    pub fn with_clock(data: DemoData, clock: SharedClock) -> Self {
        let mut messages: HashMap<ConversationId, Vec<Message>> = HashMap::new();
        for m in data.messages {
            messages.entry(m.conversation_id).or_default().push(m);
        }
        for v in messages.values_mut() {
            v.sort_by_key(|m| m.sent_at);
        }

        let next_conversation_id = data
            .conversations
            .iter()
            .map(|c| c.id.get())
            .max()
            .map_or(5001, |m| m + 1);
        let next_message_id = messages
            .values()
            .flatten()
            .map(|m| m.id.get())
            .max()
            .map_or(900_000, |m| m + 1);

        let state = MockState {
            current_user: data.current_user,
            users: data.users.into_iter().map(|u| (u.id, u)).collect(),
            relationships: data
                .relationships
                .into_iter()
                .map(|r| (r.user_id, r))
                .collect(),
            presences: data.presences.into_iter().collect(),
            guilds: data.guilds,
            channels: data.channels,
            conversations: data.conversations.into_iter().map(|c| (c.id, c)).collect(),
            messages,
            lobbies: HashMap::new(),
            voice: data.voice,
            next_conversation_id,
            next_message_id,
            sink: None,
            connected: false,
            offline: false,
            fail_queue: VecDeque::new(),
            calls: HashMap::new(),
            auth_required: false,
            signed_in: true,
            pending_sign_in: None,
        };

        Self {
            state: Mutex::new(state),
            clock,
            source: DiscordSource::Synthetic,
        }
    }

    /// A synthetic **application bot** source: the same demo guilds seen
    /// through a bot installed in them (guild channels are readable and
    /// writable conversations; no friends, DMs or presence of its own).
    /// Everything it emits is `DiscordSource::SyntheticBot` → origin
    /// `synthetic`, identity `application_bot`.
    pub fn demo_bot(seed: u64, now: Timestamp, clock: SharedClock) -> Self {
        let mut backend = Self::with_clock(fixtures::generate_bot(seed, now), clock);
        backend.source = DiscordSource::SyntheticBot;
        backend
    }

    fn is_bot(&self) -> bool {
        self.source == DiscordSource::SyntheticBot
    }

    /// Builds a backend from deterministic fixture data for `(seed, now)`.
    /// See [`fixtures::generate`].
    pub fn demo(seed: u64, now: Timestamp) -> Self {
        Self::new(fixtures::generate(seed, now))
    }

    /// Builds a backend with no social graph at all beyond `current_user`.
    /// Useful for tests that want a clean slate.
    pub fn empty(current_user: User) -> Self {
        Self::new(DemoData {
            current_user,
            users: Vec::new(),
            relationships: Vec::new(),
            presences: Vec::new(),
            guilds: Vec::new(),
            channels: Vec::new(),
            conversations: Vec::new(),
            messages: Vec::new(),
            voice: VoiceState::default(),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, MockState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn record_call(state: &mut MockState, name: &'static str) {
        *state.calls.entry(name).or_insert(0) += 1;
    }

    /// Common read-path guard: records the call, then applies offline
    /// simulation and injected failures, in that order.
    fn check_read(state: &mut MockState, name: &'static str) -> BackendResult<()> {
        Self::record_call(state, name);
        if state.offline {
            return Err(BackendError::Offline);
        }
        if !state.signed_in {
            return Err(BackendError::NotConnected);
        }
        if let Some(err) = state.fail_queue.pop_front() {
            return Err(err);
        }
        Ok(())
    }

    /// Common write-path guard: records the call, then applies offline
    /// simulation only (`fail_next` targets reads, per its own docs).
    fn check_write(state: &mut MockState, name: &'static str) -> BackendResult<()> {
        Self::record_call(state, name);
        if state.offline {
            return Err(BackendError::Offline);
        }
        if !state.signed_in {
            return Err(BackendError::NotConnected);
        }
        Ok(())
    }

    fn envelope(&self, event: DiscordEvent) -> SourceEnvelope {
        SourceEnvelope::new(self.source, self.clock.now(), event)
    }

    /// Best-effort emission: a closed ingest channel only means the
    /// application is shutting down, which is not this backend's problem to
    /// report as an operation failure.
    async fn emit(&self, sink: &IngestSender, event: DiscordEvent) {
        let env = self.envelope(event);
        if sink.send(env).await.is_err() {
            tracing::warn!("discord-adapter mock: ingest channel closed while emitting event");
        }
    }

    // ---- Test/demo helpers -------------------------------------------------

    /// Failure injection: the next `n` *read* calls return `err`.
    /// Synthetic audio devices (stable ids for tests and demos).
    pub fn demo_devices() -> Vec<AudioDevice> {
        let d = |id: &str, name: &str, kind, is_default| AudioDevice {
            id: id.into(),
            name: name.into(),
            kind,
            is_default,
        };
        vec![
            d(
                "in-default",
                "Default microphone",
                AudioDeviceKind::Input,
                true,
            ),
            d(
                "in-usb",
                "USB headset microphone",
                AudioDeviceKind::Input,
                false,
            ),
            d(
                "out-default",
                "Default speakers",
                AudioDeviceKind::Output,
                true,
            ),
            d("out-usb", "USB headset", AudioDeviceKind::Output, false),
        ]
    }

    fn has_device(id: &str, kind: AudioDeviceKind) -> bool {
        Self::demo_devices()
            .iter()
            .any(|d| d.id == id && d.kind == kind)
    }

    /// Emit Connecting → Ready and the initial presence burst.
    async fn announce_ready(&self, sink: &IngestSender) {
        let sink = sink.clone();
        self.emit(
            &sink,
            DiscordEvent::SessionChanged {
                state: SessionState::Connecting,
            },
        )
        .await;
        self.emit(
            &sink,
            DiscordEvent::SessionChanged {
                state: SessionState::Ready,
            },
        )
        .await;
        // Like the SDK's presence callbacks after connecting, report the
        // current presence of every known user.
        let presences: Vec<(UserId, Presence)> = {
            let s = self.lock();
            let mut v: Vec<_> = s.presences.iter().map(|(u, p)| (*u, p.clone())).collect();
            v.sort_by_key(|(u, _)| *u);
            v
        };
        // These mimic callback-thread delivery, so they use the non-blocking
        // path (drop + resync on a full queue) instead of awaiting capacity.
        let now = self.clock.now();
        for (user_id, presence) in presences {
            let _ = sink.try_send(SourceEnvelope::new(
                self.source,
                now,
                DiscordEvent::PresenceChanged { user_id, presence },
            ));
        }
    }

    /// Require interactive sign-in (simulates a fresh install): the backend
    /// reports `LoggedOut` and refuses reads/writes until
    /// `begin_sign_in` / `complete_sign_in` succeed.
    pub fn with_sign_in_required(self) -> Self {
        {
            let mut s = self.lock();
            s.auth_required = true;
            s.signed_in = false;
        }
        self
    }

    pub fn fail_next(&self, n: usize, err: BackendError) {
        let mut s = self.lock();
        for _ in 0..n {
            s.fail_queue.push_back(err.clone());
        }
    }

    /// Number of times a trait method named `method` (e.g. `"relationships"`,
    /// `"messages"`) has been invoked, regardless of outcome.
    pub fn calls(&self, method: &str) -> usize {
        self.lock().calls.get(method).copied().unwrap_or(0)
    }

    /// Flips offline simulation. Every backend method returns
    /// [`BackendError::Offline`] while offline. Emits
    /// `SessionChanged{Offline}` on the way down, and
    /// `SessionChanged{Reconnecting}` (then `SessionChanged{Ready}` if the
    /// backend is currently connected) on the way back up.
    pub async fn set_offline(&self, offline: bool) {
        let (changed, sink, connected) = {
            let mut s = self.lock();
            let changed = s.offline != offline;
            s.offline = offline;
            (changed, s.sink.clone(), s.connected)
        };
        if !changed {
            return;
        }
        let Some(sink) = sink else { return };
        if offline {
            self.emit(
                &sink,
                DiscordEvent::SessionChanged {
                    state: SessionState::Offline,
                },
            )
            .await;
        } else {
            self.emit(
                &sink,
                DiscordEvent::SessionChanged {
                    state: SessionState::Reconnecting,
                },
            )
            .await;
            if connected {
                self.emit(
                    &sink,
                    DiscordEvent::SessionChanged {
                        state: SessionState::Ready,
                    },
                )
                .await;
            }
        }
    }

    /// Simulates an incoming DM from `author` (e.g. a friend), as if the
    /// real backend had observed it. Stores it and emits `MessageCreated`.
    pub async fn inject_incoming_message(
        &self,
        conversation_id: ConversationId,
        author: UserId,
        content: &str,
    ) -> Message {
        let now = self.clock.now();
        let (message, sink) = {
            let mut s = self.lock();
            let message_id = MessageId(s.next_message_id);
            s.next_message_id += 1;
            let message = Message {
                id: message_id,
                conversation_id,
                author_id: author,
                content: Arc::from(content),
                sent_at: now,
                edited_at: None,
                reply_to: None,
                extras: Vec::new(),
            };
            s.messages
                .entry(conversation_id)
                .or_default()
                .push(message.clone());
            if let Some(conv) = s.conversations.get_mut(&conversation_id) {
                conv.last_message_id = Some(message_id);
                conv.last_activity_at = Some(now);
            }
            (message, s.sink.clone())
        };
        if let Some(sink) = &sink {
            self.emit(
                sink,
                DiscordEvent::MessageCreated {
                    message: message.clone(),
                },
            )
            .await;
        }
        message
    }

    /// Simulates a presence update for `user` (e.g. a friend going idle).
    pub async fn inject_presence(&self, user: UserId, presence: Presence) {
        let sink = {
            let mut s = self.lock();
            s.presences.insert(user, presence.clone());
            s.sink.clone()
        };
        if let Some(sink) = &sink {
            self.emit(
                sink,
                DiscordEvent::PresenceChanged {
                    user_id: user,
                    presence,
                },
            )
            .await;
        }
    }

    /// `(users, conversations, messages)` counts, for quick assertions.
    pub fn snapshot_counts(&self) -> (usize, usize, usize) {
        let s = self.lock();
        let users = s.users.len() + 1; // + current user
        let conversations = s.conversations.len();
        let messages = s.messages.values().map(Vec::len).sum();
        (users, conversations, messages)
    }
}

#[async_trait]
impl SocialBackend for MockBackend {
    fn source(&self) -> DiscordSource {
        self.source
    }

    fn mode(&self) -> BackendMode {
        if self.is_bot() {
            BackendMode::BotBridge
        } else {
            BackendMode::Demo
        }
    }

    fn capabilities(&self) -> CapabilitySet {
        if self.is_bot() {
            // A bot reads and writes guild channels it can see; it has no
            // social graph, DMs with the user's friends, or voice here.
            return CapabilitySet::default()
                .with(Capability::CurrentUser, SupportLevel::Full)
                .with(Capability::GuildListing, SupportLevel::Full)
                .with(Capability::GuildChannels, SupportLevel::Full)
                .with(Capability::GuildMessages, SupportLevel::Full);
        }
        let mut set = CapabilitySet::default()
            .with(Capability::CurrentUser, SupportLevel::Full)
            .with(Capability::Friends, SupportLevel::Full)
            .with(Capability::FriendRequests, SupportLevel::Full)
            .with(Capability::Blocking, SupportLevel::Full)
            .with(Capability::Presence, SupportLevel::Full)
            .with(Capability::RichPresence, SupportLevel::Full)
            .with(Capability::DmList, SupportLevel::Full)
            .with(
                Capability::DmHistory,
                SupportLevel::Partial {
                    note: "recent history only".into(),
                },
            )
            .with(Capability::DmSend, SupportLevel::Full)
            .with(Capability::DmEdit, SupportLevel::Full)
            .with(Capability::DmDelete, SupportLevel::Full)
            .with(Capability::GuildListing, SupportLevel::Full)
            .with(Capability::GuildChannels, SupportLevel::Full)
            .with(Capability::GuildMessages, SupportLevel::Unsupported)
            .with(
                Capability::LinkedChannels,
                SupportLevel::Partial {
                    note: "linked channels proxy Discord content; not every message type renders"
                        .into(),
                },
            )
            .with(Capability::Lobbies, SupportLevel::Full)
            .with(Capability::Voice, SupportLevel::Full)
            .with(Capability::VoiceDevices, SupportLevel::Full);
        set.dm_history = Some(HistoryCapability::Recent {
            max_messages: 200,
            max_age_secs: None,
        });
        set
    }

    async fn connect(&self, sink: IngestSender) -> BackendResult<()> {
        let signed_in = {
            let mut s = self.lock();
            Self::record_call(&mut s, "connect");
            s.sink = Some(sink.clone());
            s.connected = true;
            s.signed_in
        };
        if !signed_in {
            self.emit(
                &sink,
                DiscordEvent::SessionChanged {
                    state: SessionState::LoggedOut,
                },
            )
            .await;
            return Ok(());
        }
        self.announce_ready(&sink).await;
        Ok(())
    }

    async fn begin_sign_in(&self) -> BackendResult<AuthStep> {
        let (step, sink) = {
            let mut s = self.lock();
            Self::record_call(&mut s, "begin_sign_in");
            if s.signed_in {
                return Ok(AuthStep::AlreadySignedIn);
            }
            let session = crate::oauth::PkceSession::new()
                .map_err(|e| BackendError::Authentication(e.to_string()))?;
            let cfg = crate::oauth::OAuthConfig::new(MOCK_CLIENT_ID, MOCK_REDIRECT_URI);
            let url = session.authorization_url(&cfg);
            s.pending_sign_in = Some(session);
            (
                AuthStep::OpenBrowser {
                    url,
                    redirect_uri: MOCK_REDIRECT_URI.to_owned(),
                },
                s.sink.clone(),
            )
        };
        if let Some(sink) = &sink {
            self.emit(
                sink,
                DiscordEvent::SessionChanged {
                    state: SessionState::Authorizing,
                },
            )
            .await;
        }
        Ok(step)
    }

    async fn complete_sign_in(&self, redirect_url: &str) -> BackendResult<()> {
        let sink = {
            let mut s = self.lock();
            Self::record_call(&mut s, "complete_sign_in");
            let session = s
                .pending_sign_in
                .take()
                .ok_or_else(|| BackendError::Authentication("no sign-in in progress".into()))?;
            // The mock has no token endpoint: a valid code is accepted as-is.
            session
                .parse_redirect(redirect_url)
                .map_err(|e| BackendError::Authentication(e.to_string()))?;
            s.signed_in = true;
            s.sink.clone()
        };
        if let Some(sink) = &sink {
            self.announce_ready(sink).await;
        }
        Ok(())
    }

    async fn sign_out(&self) -> BackendResult<()> {
        let sink = {
            let mut s = self.lock();
            Self::record_call(&mut s, "sign_out");
            s.signed_in = false;
            s.pending_sign_in = None;
            s.sink.clone()
        };
        if let Some(sink) = &sink {
            self.emit(
                sink,
                DiscordEvent::SessionChanged {
                    state: SessionState::LoggedOut,
                },
            )
            .await;
        }
        Ok(())
    }

    async fn disconnect(&self) -> BackendResult<()> {
        let sink = {
            let mut s = self.lock();
            Self::record_call(&mut s, "disconnect");
            s.connected = false;
            s.sink.take()
        };
        if let Some(sink) = &sink {
            self.emit(
                sink,
                DiscordEvent::SessionChanged {
                    state: SessionState::LoggedOut,
                },
            )
            .await;
        }
        Ok(())
    }

    async fn current_user(&self) -> BackendResult<User> {
        let mut s = self.lock();
        Self::check_read(&mut s, "current_user")?;
        Ok(s.current_user.clone())
    }

    async fn user(&self, user_id: UserId) -> BackendResult<User> {
        let mut s = self.lock();
        Self::check_read(&mut s, "user")?;
        if user_id == s.current_user.id {
            return Ok(s.current_user.clone());
        }
        s.users
            .get(&user_id)
            .cloned()
            .ok_or_else(|| BackendError::NotFound {
                what: format!("user {user_id}"),
            })
    }

    async fn relationships(&self) -> BackendResult<Vec<(Relationship, Option<User>)>> {
        let mut s = self.lock();
        Self::check_read(&mut s, "relationships")?;
        Ok(s.relationships
            .values()
            .cloned()
            .map(|r| {
                let user = s.users.get(&r.user_id).cloned();
                (r, user)
            })
            .collect())
    }

    async fn presence(&self, user_id: UserId) -> BackendResult<Presence> {
        let mut s = self.lock();
        Self::check_read(&mut s, "presence")?;
        s.presences
            .get(&user_id)
            .cloned()
            .ok_or_else(|| BackendError::NotFound {
                what: format!("presence for user {user_id}"),
            })
    }

    async fn guilds(&self) -> BackendResult<Vec<Guild>> {
        let mut s = self.lock();
        Self::check_read(&mut s, "guilds")?;
        Ok(s.guilds.clone())
    }

    async fn guild_channels(&self, guild_id: GuildId) -> BackendResult<Vec<Channel>> {
        let mut s = self.lock();
        Self::check_read(&mut s, "guild_channels")?;
        if !s.guilds.iter().any(|g| g.id == guild_id) {
            return Err(BackendError::NotFound {
                what: format!("guild {guild_id}"),
            });
        }
        Ok(s.channels
            .iter()
            .filter(|c| c.guild_id == guild_id)
            .cloned()
            .collect())
    }

    async fn conversations(&self) -> BackendResult<Vec<Conversation>> {
        let mut s = self.lock();
        Self::check_read(&mut s, "conversations")?;
        Ok(s.conversations.values().cloned().collect())
    }

    async fn messages(
        &self,
        conversation_id: ConversationId,
        limit: u32,
    ) -> BackendResult<Vec<Message>> {
        let mut s = self.lock();
        Self::check_read(&mut s, "messages")?;
        if !s.conversations.contains_key(&conversation_id) {
            return Err(BackendError::NotFound {
                what: format!("conversation {conversation_id}"),
            });
        }
        let all = s
            .messages
            .get(&conversation_id)
            .cloned()
            .unwrap_or_default();
        let limit = limit as usize;
        let start = all.len().saturating_sub(limit);
        Ok(all[start..].to_vec())
    }

    async fn lobby(&self, lobby_id: LobbyId) -> BackendResult<Lobby> {
        let mut s = self.lock();
        Self::check_read(&mut s, "lobby")?;
        s.lobbies
            .get(&lobby_id)
            .cloned()
            .ok_or_else(|| BackendError::NotFound {
                what: format!("lobby {lobby_id}"),
            })
    }

    async fn voice_state(&self) -> BackendResult<VoiceState> {
        let mut s = self.lock();
        Self::check_read(&mut s, "voice_state")?;
        Ok(s.voice.clone())
    }

    async fn audio_devices(&self) -> BackendResult<Vec<AudioDevice>> {
        let mut s = self.lock();
        Self::check_read(&mut s, "audio_devices")?;
        Ok(Self::demo_devices())
    }

    async fn voice_control(&self, control: VoiceControl) -> BackendResult<VoiceState> {
        let (voice, sink) = {
            let mut s = self.lock();
            Self::check_write(&mut s, "voice_control")?;
            match control {
                VoiceControl::JoinLobby(lobby_id) => {
                    s.voice.connected = true;
                    s.voice.lobby_id = Some(lobby_id);
                }
                VoiceControl::Leave => {
                    s.voice.connected = false;
                    s.voice.lobby_id = None;
                    s.voice.participants.clear();
                }
                VoiceControl::SetMuted(muted) => s.voice.muted = muted,
                VoiceControl::SetDeafened(deafened) => s.voice.deafened = deafened,
                VoiceControl::SetInputDevice(dev) => {
                    if !Self::has_device(&dev, AudioDeviceKind::Input) {
                        return Err(BackendError::NotFound {
                            what: format!("input device {dev}"),
                        });
                    }
                    s.voice.input_device = Some(dev);
                }
                VoiceControl::SetOutputDevice(dev) => {
                    if !Self::has_device(&dev, AudioDeviceKind::Output) {
                        return Err(BackendError::NotFound {
                            what: format!("output device {dev}"),
                        });
                    }
                    s.voice.output_device = Some(dev);
                }
                VoiceControl::SetOutputVolume(v) => s.voice.output_volume = v,
                VoiceControl::SetNoiseSuppression(v) => s.voice.noise_suppression = v,
                VoiceControl::SetPushToTalk(v) => s.voice.push_to_talk = v,
            }
            (s.voice.clone(), s.sink.clone())
        };
        if let Some(sink) = &sink {
            self.emit(
                sink,
                DiscordEvent::VoiceStateChanged {
                    voice: voice.clone(),
                },
            )
            .await;
        }
        Ok(voice)
    }

    async fn send_message(&self, target: &MessageTarget, content: &str) -> BackendResult<Message> {
        let now = self.clock.now();
        let (message, sink, new_conversation) = {
            let mut s = self.lock();
            Self::check_write(&mut s, "send_message")?;

            let (conversation_id, new_conversation) = match target {
                MessageTarget::Conversation { conversation_id } => {
                    if !s.conversations.contains_key(conversation_id) {
                        return Err(BackendError::NotFound {
                            what: format!("conversation {conversation_id}"),
                        });
                    }
                    (*conversation_id, None)
                }
                MessageTarget::User { user_id } => {
                    let existing = s
                        .conversations
                        .values()
                        .find(|c| {
                            c.kind == ConversationKind::DirectMessage
                                && c.recipient_id == Some(*user_id)
                        })
                        .map(|c| c.id);
                    match existing {
                        Some(id) => (id, None),
                        None => {
                            let id = ConversationId(s.next_conversation_id);
                            s.next_conversation_id += 1;
                            let conv = Conversation {
                                id,
                                kind: ConversationKind::DirectMessage,
                                recipient_id: Some(*user_id),
                                guild_id: None,
                                lobby_id: None,
                                title: None,
                                last_message_id: None,
                                last_activity_at: None,
                            };
                            s.conversations.insert(id, conv.clone());
                            (id, Some(conv))
                        }
                    }
                }
            };

            let message_id = MessageId(s.next_message_id);
            s.next_message_id += 1;
            let author_id = s.current_user.id;
            let message = Message {
                id: message_id,
                conversation_id,
                author_id,
                content: Arc::from(content),
                sent_at: now,
                edited_at: None,
                reply_to: None,
                extras: Vec::new(),
            };
            s.messages
                .entry(conversation_id)
                .or_default()
                .push(message.clone());
            if let Some(conv) = s.conversations.get_mut(&conversation_id) {
                conv.last_message_id = Some(message_id);
                conv.last_activity_at = Some(now);
            }
            let new_conversation = new_conversation.map(|mut c| {
                c.last_message_id = Some(message_id);
                c.last_activity_at = Some(now);
                c
            });

            (message, s.sink.clone(), new_conversation)
        };

        if let Some(sink) = &sink {
            if let Some(conversation) = new_conversation {
                self.emit(sink, DiscordEvent::ConversationUpserted { conversation })
                    .await;
            }
            self.emit(
                sink,
                DiscordEvent::MessageCreated {
                    message: message.clone(),
                },
            )
            .await;
        }

        Ok(message)
    }

    async fn edit_message(&self, message_id: MessageId, content: &str) -> BackendResult<()> {
        let now = self.clock.now();
        let (message, sink) = {
            let mut s = self.lock();
            Self::check_write(&mut s, "edit_message")?;
            let current_user_id = s.current_user.id;
            let msg = s
                .messages
                .values_mut()
                .flat_map(|v| v.iter_mut())
                .find(|m| m.id == message_id)
                .ok_or_else(|| BackendError::NotFound {
                    what: format!("message {message_id}"),
                })?;
            if msg.author_id != current_user_id {
                return Err(BackendError::Sdk(
                    "cannot edit another user's message".into(),
                ));
            }
            msg.content = Arc::from(content);
            msg.edited_at = Some(now);
            (msg.clone(), s.sink.clone())
        };
        if let Some(sink) = &sink {
            self.emit(sink, DiscordEvent::MessageUpdated { message })
                .await;
        }
        Ok(())
    }

    async fn delete_message(&self, message_id: MessageId) -> BackendResult<()> {
        let (conversation_id, sink) = {
            let mut s = self.lock();
            Self::check_write(&mut s, "delete_message")?;
            let current_user_id = s.current_user.id;

            let mut found: Option<ConversationId> = None;
            for (conv_id, msgs) in s.messages.iter_mut() {
                if let Some(pos) = msgs.iter().position(|m| m.id == message_id) {
                    if msgs[pos].author_id != current_user_id {
                        return Err(BackendError::Sdk(
                            "cannot edit another user's message".into(),
                        ));
                    }
                    msgs.remove(pos);
                    found = Some(*conv_id);
                    break;
                }
            }
            let conversation_id = found.ok_or_else(|| BackendError::NotFound {
                what: format!("message {message_id}"),
            })?;

            let new_last = s
                .messages
                .get(&conversation_id)
                .and_then(|v| v.last())
                .cloned();
            if let Some(conv) = s.conversations.get_mut(&conversation_id) {
                conv.last_message_id = new_last.as_ref().map(|m| m.id);
                conv.last_activity_at = new_last.as_ref().map(|m| m.sent_at);
            }

            (conversation_id, s.sink.clone())
        };
        if let Some(sink) = &sink {
            self.emit(
                sink,
                DiscordEvent::MessageDeleted {
                    message_id,
                    conversation_id,
                },
            )
            .await;
        }
        Ok(())
    }

    async fn set_presence(&self, presence: &PresenceDraft) -> BackendResult<()> {
        let (value, sink, user_id) = {
            let mut s = self.lock();
            Self::check_write(&mut s, "set_presence")?;
            let user_id = s.current_user.id;
            let value = Presence {
                status: presence.status,
                activity: presence.activity.clone(),
            };
            s.presences.insert(user_id, value.clone());
            (value, s.sink.clone(), user_id)
        };
        if let Some(sink) = &sink {
            self.emit(
                sink,
                DiscordEvent::PresenceChanged {
                    user_id,
                    presence: value,
                },
            )
            .await;
        }
        Ok(())
    }

    async fn relationship_action(
        &self,
        user_id: UserId,
        action: RelationshipAction,
    ) -> BackendResult<()> {
        let (event, sink) = {
            let mut s = self.lock();
            Self::check_write(&mut s, "relationship_action")?;
            let now = self.clock.now();
            match action {
                RelationshipAction::SendFriendRequest => {
                    let rel = Relationship {
                        user_id,
                        discord: RelationshipKind::PendingOutgoing,
                        game: RelationshipKind::None,
                        since: Some(now),
                    };
                    s.relationships.insert(user_id, rel.clone());
                    let user = s.users.get(&user_id).cloned();
                    (
                        DiscordEvent::RelationshipUpserted {
                            relationship: rel,
                            user,
                        },
                        s.sink.clone(),
                    )
                }
                RelationshipAction::AcceptFriendRequest => {
                    let rel = Relationship {
                        user_id,
                        discord: RelationshipKind::Friend,
                        game: RelationshipKind::None,
                        since: Some(now),
                    };
                    s.relationships.insert(user_id, rel.clone());
                    let user = s.users.get(&user_id).cloned();
                    (
                        DiscordEvent::RelationshipUpserted {
                            relationship: rel,
                            user,
                        },
                        s.sink.clone(),
                    )
                }
                RelationshipAction::RejectFriendRequest | RelationshipAction::RemoveFriend => {
                    s.relationships.remove(&user_id);
                    (
                        DiscordEvent::RelationshipRemoved { user_id },
                        s.sink.clone(),
                    )
                }
                RelationshipAction::Block => {
                    let rel = Relationship {
                        user_id,
                        discord: RelationshipKind::Blocked,
                        game: RelationshipKind::None,
                        since: Some(now),
                    };
                    s.relationships.insert(user_id, rel.clone());
                    let user = s.users.get(&user_id).cloned();
                    (
                        DiscordEvent::RelationshipUpserted {
                            relationship: rel,
                            user,
                        },
                        s.sink.clone(),
                    )
                }
                RelationshipAction::Unblock => {
                    s.relationships.remove(&user_id);
                    (
                        DiscordEvent::RelationshipRemoved { user_id },
                        s.sink.clone(),
                    )
                }
            }
        };
        if let Some(sink) = &sink {
            self.emit(sink, event).await;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use litecord_core::bus::ingest_channel;

    fn user(id: u64, name: &str) -> User {
        User {
            id: UserId(id),
            username: Arc::from(name),
            global_name: None,
            avatar_url: None,
            is_bot: false,
            is_provisional: false,
        }
    }

    fn now() -> Timestamp {
        Timestamp::from_millis(1_700_000_000_000)
    }

    #[tokio::test]
    async fn connect_emits_connecting_then_ready() {
        let backend = MockBackend::demo(1, now());
        let (tx, mut rx) = ingest_channel(8);
        backend.connect(tx).await.unwrap();

        let first = rx.recv().await.unwrap();
        assert_eq!(
            first.event,
            DiscordEvent::SessionChanged {
                state: SessionState::Connecting
            }
        );
        let second = rx.recv().await.unwrap();
        assert_eq!(
            second.event,
            DiscordEvent::SessionChanged {
                state: SessionState::Ready
            }
        );
    }

    #[tokio::test]
    async fn send_message_emits_and_returns_authored_message() {
        let backend = MockBackend::demo(2, now());
        let (tx, mut rx) = ingest_channel(32);
        backend.connect(tx).await.unwrap();
        // Drain the connect events.
        rx.recv().await;
        rx.recv().await;

        let current = backend.current_user().await.unwrap();
        let target = MessageTarget::User {
            user_id: UserId(1001),
        };
        let sent = backend.send_message(&target, "hello there").await.unwrap();
        assert_eq!(sent.author_id, current.id);
        assert_eq!(sent.content.as_ref(), "hello there");

        // The new-DM path also emits ConversationUpserted (recipient 1001
        // has an existing demo DM, so this asserts on message emission at
        // least).
        let mut saw_message_created = false;
        while let Some(env) = rx.try_recv() {
            if let DiscordEvent::MessageCreated { message } = env.event {
                if message.id == sent.id {
                    saw_message_created = true;
                }
            }
        }
        assert!(saw_message_created);
    }

    #[tokio::test]
    async fn editing_someone_elses_message_fails() {
        let backend = MockBackend::demo(3, now());
        let conversations = backend.conversations().await.unwrap();
        let conv = conversations.first().cloned().unwrap();
        let messages = backend.messages(conv.id, 50).await.unwrap();
        let other_msg = messages
            .iter()
            .find(|m| m.author_id != UserId(1000))
            .cloned()
            .unwrap();

        let result = backend.edit_message(other_msg.id, "hacked").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn offline_mode_returns_offline_error() {
        let backend = MockBackend::demo(4, now());
        backend.set_offline(true).await;
        let result = backend.current_user().await;
        assert_eq!(result, Err(BackendError::Offline));
    }

    #[tokio::test]
    async fn fail_next_injects_exactly_n_failures() {
        let backend = MockBackend::demo(5, now());
        backend.fail_next(2, BackendError::Sdk("boom".into()));

        assert!(backend.current_user().await.is_err());
        assert!(backend.current_user().await.is_err());
        assert!(backend.current_user().await.is_ok());
    }

    #[tokio::test]
    async fn calls_counts_invocations() {
        let backend = MockBackend::demo(6, now());
        assert_eq!(backend.calls("relationships"), 0);
        let _ = backend.relationships().await;
        let _ = backend.relationships().await;
        assert_eq!(backend.calls("relationships"), 2);
        assert_eq!(backend.calls("messages"), 0);
    }

    #[tokio::test]
    async fn relationship_block_and_remove_emit_events() {
        let backend = MockBackend::demo(7, now());
        let (tx, mut rx) = ingest_channel(32);
        backend.connect(tx).await.unwrap();
        // Drain session + initial presence events.
        while rx.try_recv().is_some() {}

        backend
            .relationship_action(UserId(1001), RelationshipAction::Block)
            .await
            .unwrap();
        let env = rx.recv().await.unwrap();
        match env.event {
            DiscordEvent::RelationshipUpserted { relationship, .. } => {
                assert_eq!(relationship.discord, RelationshipKind::Blocked);
            }
            other => panic!("unexpected event: {other:?}"),
        }

        backend
            .relationship_action(UserId(1002), RelationshipAction::RemoveFriend)
            .await
            .unwrap();
        let env = rx.recv().await.unwrap();
        match env.event {
            DiscordEvent::RelationshipRemoved { user_id } => {
                assert_eq!(user_id, UserId(1002));
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[tokio::test]
    async fn capabilities_report_guild_messages_unsupported() {
        let backend = MockBackend::demo(8, now());
        let caps = backend.capabilities();
        assert_eq!(
            caps.support(Capability::GuildMessages),
            SupportLevel::Unsupported
        );
        assert!(caps.is_usable(Capability::DmSend));
    }

    #[tokio::test]
    async fn empty_backend_has_only_current_user() {
        let backend = MockBackend::empty(user(42, "solo"));
        let (users, conversations, messages) = backend.snapshot_counts();
        assert_eq!(users, 1);
        assert_eq!(conversations, 0);
        assert_eq!(messages, 0);
        assert_eq!(
            backend.relationships().await.unwrap(),
            Vec::<(Relationship, Option<User>)>::new()
        );
    }

    #[tokio::test]
    async fn inject_incoming_message_is_stored_and_emitted() {
        let backend = MockBackend::demo(9, now());
        let (tx, mut rx) = ingest_channel(32);
        backend.connect(tx).await.unwrap();
        // Drain session + initial presence events.
        while rx.try_recv().is_some() {}

        let conversations = backend.conversations().await.unwrap();
        let conv = conversations.first().cloned().unwrap();
        let msg = backend
            .inject_incoming_message(conv.id, UserId(1001), "surprise!")
            .await;

        let stored = backend.messages(conv.id, 1).await.unwrap();
        assert_eq!(stored.last().map(|m| m.id), Some(msg.id));

        let env = rx.recv().await.unwrap();
        assert_eq!(env.event, DiscordEvent::MessageCreated { message: msg });
    }
}
