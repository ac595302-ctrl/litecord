//! Event vocabularies.
//!
//! ```text
//! SDK callback ─► adapter ─► SourceEnvelope{DiscordEvent} ─► ingest queue
//!     ─► reducer (one SQLite tx, one revision) ─► Vec<UnifiedEvent> (event log)
//!     ─► ApplicationEvent::StateChanged broadcast ─► UI / hydrator / memory jobs
//! ```
//!
//! * [`DiscordEvent`] carries normalized Litecord payloads — never SDK
//!   objects. Adapters resolve ids to full objects before emitting when they
//!   can; otherwise they emit [`DiscordEvent::Invalidated`] and hydration
//!   fetches the object.
//! * [`UnifiedEvent`] is compact (ids only) and persisted in the append-only
//!   `events` table.
//! * [`ApplicationEvent`] is what subscribers (UI, features, agents) see.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use litecord_types::entity::EntityId;
use litecord_types::ids::*;
use litecord_types::provenance::DiscordSource;
use litecord_types::social::*;
use litecord_types::{Revision, Timestamp};

/// What can be (re)hydrated from a backend. Also the deduplication key for the
/// hydration scheduler.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HydrationKey {
    CurrentUser,
    Relationships,
    Guilds,
    GuildChannels { guild_id: GuildId },
    DmSummaries,
    DmConversation { conversation_id: ConversationId },
    Lobby { lobby_id: LobbyId },
    User { user_id: UserId },
    VoiceState,
}

impl HydrationKey {
    /// Stable string used as the `sync_state` primary key.
    pub fn storage_key(&self) -> String {
        match self {
            HydrationKey::CurrentUser => "current_user".into(),
            HydrationKey::Relationships => "relationships".into(),
            HydrationKey::Guilds => "guilds".into(),
            HydrationKey::GuildChannels { guild_id } => format!("guild_channels:{guild_id}"),
            HydrationKey::DmSummaries => "dm_summaries".into(),
            HydrationKey::DmConversation { conversation_id } => {
                format!("dm_conversation:{conversation_id}")
            }
            HydrationKey::Lobby { lobby_id } => format!("lobby:{lobby_id}"),
            HydrationKey::User { user_id } => format!("user:{user_id}"),
            HydrationKey::VoiceState => "voice_state".into(),
        }
    }

    pub fn parse_storage_key(s: &str) -> Option<Self> {
        let (head, tail) = match s.split_once(':') {
            Some((h, t)) => (h, Some(t)),
            None => (s, None),
        };
        Some(match (head, tail) {
            ("current_user", None) => HydrationKey::CurrentUser,
            ("relationships", None) => HydrationKey::Relationships,
            ("guilds", None) => HydrationKey::Guilds,
            ("dm_summaries", None) => HydrationKey::DmSummaries,
            ("voice_state", None) => HydrationKey::VoiceState,
            ("guild_channels", Some(t)) => HydrationKey::GuildChannels {
                guild_id: t.parse().ok()?,
            },
            ("dm_conversation", Some(t)) => HydrationKey::DmConversation {
                conversation_id: t.parse().ok()?,
            },
            ("lobby", Some(t)) => HydrationKey::Lobby {
                lobby_id: t.parse().ok()?,
            },
            ("user", Some(t)) => HydrationKey::User {
                user_id: t.parse().ok()?,
            },
            _ => return None,
        })
    }
}

/// A normalized event from a Discord source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DiscordEvent {
    SessionChanged {
        state: SessionState,
    },
    CurrentUser {
        user: User,
    },
    UserUpserted {
        user: User,
    },
    PresenceChanged {
        user_id: UserId,
        presence: Presence,
    },
    RelationshipUpserted {
        relationship: Relationship,
        #[serde(default)]
        user: Option<User>,
    },
    RelationshipRemoved {
        user_id: UserId,
    },
    GuildUpserted {
        guild: Guild,
    },
    GuildRemoved {
        guild_id: GuildId,
    },
    ChannelUpserted {
        channel: Channel,
    },
    ConversationUpserted {
        conversation: Conversation,
    },
    MessageCreated {
        message: Message,
    },
    MessageUpdated {
        message: Message,
    },
    MessageDeleted {
        message_id: MessageId,
        conversation_id: ConversationId,
    },
    LobbyUpserted {
        lobby: Lobby,
    },
    VoiceStateChanged {
        voice: VoiceState,
    },
    /// Authoritative full relationship list (from hydration). Relationships
    /// not in the list are removed.
    RelationshipsSnapshot {
        entries: Vec<(Relationship, Option<User>)>,
    },
    /// Authoritative guild list. Guilds not in the list are marked departed.
    GuildsSnapshot {
        guilds: Vec<Guild>,
    },
    /// Authoritative channel list for one guild.
    GuildChannelsSnapshot {
        guild_id: GuildId,
        channels: Vec<Channel>,
    },
    /// Conversation summaries (DM list). Upsert only.
    ConversationsSnapshot {
        conversations: Vec<Conversation>,
    },
    /// A recent-history window. Upsert only; older local history is kept.
    MessagesSnapshot {
        conversation_id: ConversationId,
        messages: Vec<Message>,
    },
    /// One page of older (or catch-up) history, oldest first. Upsert only:
    /// messages missing from the page are **never** deleted — unlike the
    /// authoritative `*Snapshot` variants, a page says nothing about what
    /// lies outside it. Advances the conversation's `history_sync` cursor
    /// (when one exists) in the same transaction.
    MessagesPage {
        conversation_id: ConversationId,
        messages: Vec<Message>,
    },
    /// A verified forward REST page. Its independent recovery cursor commits
    /// with the rows; it never changes Stage C's backward-history checkpoint.
    MessagesCatchupPage {
        conversation_id: ConversationId,
        messages: Vec<Message>,
        has_more: bool,
    },
    /// The adapter knows something changed but could not resolve it; the
    /// hydrator should fetch `key`.
    Invalidated {
        key: HydrationKey,
    },
}

impl DiscordEvent {
    pub fn kind(&self) -> &'static str {
        match self {
            DiscordEvent::SessionChanged { .. } => "session_changed",
            DiscordEvent::CurrentUser { .. } => "current_user",
            DiscordEvent::UserUpserted { .. } => "user_upserted",
            DiscordEvent::PresenceChanged { .. } => "presence_changed",
            DiscordEvent::RelationshipUpserted { .. } => "relationship_upserted",
            DiscordEvent::RelationshipRemoved { .. } => "relationship_removed",
            DiscordEvent::GuildUpserted { .. } => "guild_upserted",
            DiscordEvent::GuildRemoved { .. } => "guild_removed",
            DiscordEvent::ChannelUpserted { .. } => "channel_upserted",
            DiscordEvent::ConversationUpserted { .. } => "conversation_upserted",
            DiscordEvent::MessageCreated { .. } => "message_created",
            DiscordEvent::MessageUpdated { .. } => "message_updated",
            DiscordEvent::MessageDeleted { .. } => "message_deleted",
            DiscordEvent::LobbyUpserted { .. } => "lobby_upserted",
            DiscordEvent::VoiceStateChanged { .. } => "voice_state_changed",
            DiscordEvent::RelationshipsSnapshot { .. } => "relationships_snapshot",
            DiscordEvent::GuildsSnapshot { .. } => "guilds_snapshot",
            DiscordEvent::GuildChannelsSnapshot { .. } => "guild_channels_snapshot",
            DiscordEvent::ConversationsSnapshot { .. } => "conversations_snapshot",
            DiscordEvent::MessagesSnapshot { .. } => "messages_snapshot",
            DiscordEvent::MessagesPage { .. } => "messages_page",
            DiscordEvent::MessagesCatchupPage { .. } => "messages_catchup_page",
            DiscordEvent::Invalidated { .. } => "invalidated",
        }
    }
}

/// A source event plus provenance. The only input to the canonical reducer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceEnvelope {
    pub source: DiscordSource,
    pub observed_at: Timestamp,
    pub event: DiscordEvent,
}

impl SourceEnvelope {
    pub fn new(source: DiscordSource, observed_at: Timestamp, event: DiscordEvent) -> Self {
        Self {
            source,
            observed_at,
            event,
        }
    }
}

// ---------------------------------------------------------------------------
// Approximate in-memory size (byte budgets)
// ---------------------------------------------------------------------------

/// Fixed per-item overhead used by the `approx_bytes` estimates: covers the
/// struct itself, ids, timestamps and allocator slack. Deliberately coarse.
const ITEM_OVERHEAD: usize = 64;

fn opt_str_len<S: AsRef<str>>(s: &Option<S>) -> usize {
    s.as_ref().map_or(0, |s| s.as_ref().len())
}

fn user_bytes(u: &User) -> usize {
    ITEM_OVERHEAD + u.username.len() + opt_str_len(&u.global_name) + opt_str_len(&u.avatar_url)
}

fn guild_bytes(g: &Guild) -> usize {
    ITEM_OVERHEAD + g.name.len() + opt_str_len(&g.icon_url)
}

fn channel_bytes(c: &Channel) -> usize {
    ITEM_OVERHEAD + c.name.len()
}

fn conversation_bytes(c: &Conversation) -> usize {
    ITEM_OVERHEAD + opt_str_len(&c.title)
}

fn relationship_bytes(u: &Option<User>) -> usize {
    ITEM_OVERHEAD + u.as_ref().map_or(0, user_bytes)
}

fn presence_bytes(p: &Presence) -> usize {
    ITEM_OVERHEAD
        + p.activity.as_ref().map_or(0, |a| {
            a.name.len() + opt_str_len(&a.details) + opt_str_len(&a.state)
        })
}

/// Cheap estimate of a [`Message`]'s heap + inline footprint in bytes: a
/// fixed overhead plus content and extra-string lengths. No serialization.
/// Also used to size message values in byte-bounded caches.
pub fn message_approx_bytes(m: &Message) -> usize {
    ITEM_OVERHEAD
        + m.content.len()
        + m.extras
            .iter()
            .map(|e| {
                ITEM_OVERHEAD / 2
                    + match e {
                        MessageExtra::Attachment {
                            filename,
                            content_type,
                            ..
                        } => filename.len() + opt_str_len(content_type),
                        MessageExtra::Embed { title, url } => opt_str_len(title) + opt_str_len(url),
                        MessageExtra::Sticker { name } => name.len(),
                        MessageExtra::Unsupported { kind } => kind.len(),
                        MessageExtra::Poll | MessageExtra::VoiceMessage | MessageExtra::Thread => 0,
                    }
            })
            .sum::<usize>()
}

impl DiscordEvent {
    /// Cheap estimate of this event's memory footprint in bytes (fixed
    /// per-variant overhead + string/content lengths + per-item overhead for
    /// list payloads). Used for queue byte budgets; never serializes.
    pub fn approx_bytes(&self) -> usize {
        let payload = match self {
            DiscordEvent::SessionChanged { .. }
            | DiscordEvent::RelationshipRemoved { .. }
            | DiscordEvent::GuildRemoved { .. }
            | DiscordEvent::MessageDeleted { .. }
            | DiscordEvent::Invalidated { .. } => 0,
            DiscordEvent::CurrentUser { user } | DiscordEvent::UserUpserted { user } => {
                user_bytes(user)
            }
            DiscordEvent::PresenceChanged { presence, .. } => presence_bytes(presence),
            DiscordEvent::RelationshipUpserted { user, .. } => relationship_bytes(user),
            DiscordEvent::GuildUpserted { guild } => guild_bytes(guild),
            DiscordEvent::ChannelUpserted { channel } => channel_bytes(channel),
            DiscordEvent::ConversationUpserted { conversation } => conversation_bytes(conversation),
            DiscordEvent::MessageCreated { message } | DiscordEvent::MessageUpdated { message } => {
                message_approx_bytes(message)
            }
            DiscordEvent::LobbyUpserted { lobby } => {
                ITEM_OVERHEAD + lobby.member_ids.len() * std::mem::size_of::<UserId>()
            }
            DiscordEvent::VoiceStateChanged { voice } => {
                ITEM_OVERHEAD
                    + opt_str_len(&voice.input_device)
                    + opt_str_len(&voice.output_device)
                    + voice.participants.len() * ITEM_OVERHEAD
            }
            DiscordEvent::RelationshipsSnapshot { entries } => {
                entries.iter().map(|(_, u)| relationship_bytes(u)).sum()
            }
            DiscordEvent::GuildsSnapshot { guilds } => guilds.iter().map(guild_bytes).sum(),
            DiscordEvent::GuildChannelsSnapshot { channels, .. } => {
                channels.iter().map(channel_bytes).sum()
            }
            DiscordEvent::ConversationsSnapshot { conversations } => {
                conversations.iter().map(conversation_bytes).sum()
            }
            DiscordEvent::MessagesSnapshot { messages, .. }
            | DiscordEvent::MessagesPage { messages, .. }
            | DiscordEvent::MessagesCatchupPage { messages, .. } => {
                messages.iter().map(message_approx_bytes).sum()
            }
        };
        std::mem::size_of::<Self>() + payload
    }
}

impl SourceEnvelope {
    /// Cheap estimate of the envelope's memory footprint in bytes; see
    /// [`DiscordEvent::approx_bytes`].
    pub fn approx_bytes(&self) -> usize {
        std::mem::size_of::<DiscordSource>()
            + std::mem::size_of::<Timestamp>()
            + self.event.approx_bytes()
    }
}

/// Compact, persisted record of a state change. Ids only: consumers re-read
/// canonical state rather than trusting event payloads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UnifiedEvent {
    SessionChanged,
    CurrentUserChanged {
        user_id: UserId,
    },
    UserUpdated {
        user_id: UserId,
    },
    PresenceChanged {
        user_id: UserId,
    },
    RelationshipChanged {
        user_id: UserId,
    },
    RelationshipRemoved {
        user_id: UserId,
    },
    GuildObserved {
        guild_id: GuildId,
    },
    GuildRemoved {
        guild_id: GuildId,
    },
    ChannelObserved {
        channel_id: ChannelId,
    },
    ChannelRemoved {
        channel_id: ChannelId,
    },
    ConversationObserved {
        conversation_id: ConversationId,
    },
    MessageCreated {
        message_id: MessageId,
        conversation_id: ConversationId,
    },
    /// A message first learned from a historical snapshot or backfill page.
    /// Kept distinct from live creation so event-triggered automations do not
    /// treat old history as a newly received message.
    MessageImported {
        message_id: MessageId,
        conversation_id: ConversationId,
    },
    MessageUpdated {
        message_id: MessageId,
        conversation_id: ConversationId,
    },
    MessageDeleted {
        message_id: MessageId,
        conversation_id: ConversationId,
    },
    LobbyUpdated {
        lobby_id: LobbyId,
    },
    VoiceStateChanged,
    MemoryRecorded {
        memory_id: MemoryId,
    },
    MemoryUpdated {
        memory_id: MemoryId,
    },
    MemorySuperseded {
        old: MemoryId,
        new: MemoryId,
    },
    EdgeRecorded {
        from: EntityId,
        to: EntityId,
    },
    TaskCreated {
        task_id: TaskId,
    },
    TaskUpdated {
        task_id: TaskId,
    },
    ReminderCreated {
        reminder_id: ReminderId,
    },
    ReminderUpdated {
        reminder_id: ReminderId,
    },
    ReminderFired {
        reminder_id: ReminderId,
    },
    DraftSaved {
        draft_id: DraftId,
    },
    NoteUpdated {
        user_id: UserId,
    },
    BookmarkChanged {
        message_id: MessageId,
    },
    SettingChanged {
        key: String,
    },
    VisibilityChanged {
        conversation_id: ConversationId,
    },
    ActionProposed {
        action_id: ActionId,
    },
    ActionApproved {
        action_id: ActionId,
    },
    ActionRejected {
        action_id: ActionId,
    },
    ActionExecuted {
        action_id: ActionId,
    },
    ActionFailed {
        action_id: ActionId,
    },
    ActionInvalidated {
        action_id: ActionId,
    },
    AgentRunRecorded {
        run_id: AgentRunId,
    },
}

impl UnifiedEvent {
    /// Stable kind string stored in the `events.kind` column.
    pub fn kind(&self) -> &'static str {
        match self {
            UnifiedEvent::SessionChanged => "session_changed",
            UnifiedEvent::CurrentUserChanged { .. } => "current_user_changed",
            UnifiedEvent::UserUpdated { .. } => "user_updated",
            UnifiedEvent::PresenceChanged { .. } => "presence_changed",
            UnifiedEvent::RelationshipChanged { .. } => "relationship_changed",
            UnifiedEvent::RelationshipRemoved { .. } => "relationship_removed",
            UnifiedEvent::GuildObserved { .. } => "guild_observed",
            UnifiedEvent::GuildRemoved { .. } => "guild_removed",
            UnifiedEvent::ChannelObserved { .. } => "channel_observed",
            UnifiedEvent::ChannelRemoved { .. } => "channel_removed",
            UnifiedEvent::ConversationObserved { .. } => "conversation_observed",
            UnifiedEvent::MessageCreated { .. } => "message_created",
            UnifiedEvent::MessageImported { .. } => "message_imported",
            UnifiedEvent::MessageUpdated { .. } => "message_updated",
            UnifiedEvent::MessageDeleted { .. } => "message_deleted",
            UnifiedEvent::LobbyUpdated { .. } => "lobby_updated",
            UnifiedEvent::VoiceStateChanged => "voice_state_changed",
            UnifiedEvent::MemoryRecorded { .. } => "memory_recorded",
            UnifiedEvent::MemoryUpdated { .. } => "memory_updated",
            UnifiedEvent::MemorySuperseded { .. } => "memory_superseded",
            UnifiedEvent::EdgeRecorded { .. } => "edge_recorded",
            UnifiedEvent::TaskCreated { .. } => "task_created",
            UnifiedEvent::TaskUpdated { .. } => "task_updated",
            UnifiedEvent::ReminderCreated { .. } => "reminder_created",
            UnifiedEvent::ReminderUpdated { .. } => "reminder_updated",
            UnifiedEvent::ReminderFired { .. } => "reminder_fired",
            UnifiedEvent::DraftSaved { .. } => "draft_saved",
            UnifiedEvent::NoteUpdated { .. } => "note_updated",
            UnifiedEvent::BookmarkChanged { .. } => "bookmark_changed",
            UnifiedEvent::SettingChanged { .. } => "setting_changed",
            UnifiedEvent::VisibilityChanged { .. } => "visibility_changed",
            UnifiedEvent::ActionProposed { .. } => "action_proposed",
            UnifiedEvent::ActionApproved { .. } => "action_approved",
            UnifiedEvent::ActionRejected { .. } => "action_rejected",
            UnifiedEvent::ActionExecuted { .. } => "action_executed",
            UnifiedEvent::ActionFailed { .. } => "action_failed",
            UnifiedEvent::ActionInvalidated { .. } => "action_invalidated",
            UnifiedEvent::AgentRunRecorded { .. } => "agent_run_recorded",
        }
    }

    /// Primary entity the event is about, for indexing the event log.
    pub fn entity(&self) -> Option<EntityId> {
        Some(match self {
            UnifiedEvent::CurrentUserChanged { user_id }
            | UnifiedEvent::UserUpdated { user_id }
            | UnifiedEvent::PresenceChanged { user_id }
            | UnifiedEvent::RelationshipChanged { user_id }
            | UnifiedEvent::RelationshipRemoved { user_id }
            | UnifiedEvent::NoteUpdated { user_id } => EntityId::User(*user_id),
            UnifiedEvent::GuildObserved { guild_id } | UnifiedEvent::GuildRemoved { guild_id } => {
                EntityId::Guild(*guild_id)
            }
            UnifiedEvent::ChannelObserved { channel_id }
            | UnifiedEvent::ChannelRemoved { channel_id } => EntityId::Channel(*channel_id),
            UnifiedEvent::ConversationObserved { conversation_id }
            | UnifiedEvent::VisibilityChanged { conversation_id } => {
                EntityId::Conversation(*conversation_id)
            }
            UnifiedEvent::MessageCreated { message_id, .. }
            | UnifiedEvent::MessageImported { message_id, .. }
            | UnifiedEvent::MessageUpdated { message_id, .. }
            | UnifiedEvent::MessageDeleted { message_id, .. }
            | UnifiedEvent::BookmarkChanged { message_id } => EntityId::Message(*message_id),
            UnifiedEvent::LobbyUpdated { lobby_id } => EntityId::Lobby(*lobby_id),
            UnifiedEvent::MemoryRecorded { memory_id }
            | UnifiedEvent::MemoryUpdated { memory_id } => EntityId::Memory(*memory_id),
            UnifiedEvent::MemorySuperseded { new, .. } => EntityId::Memory(*new),
            UnifiedEvent::EdgeRecorded { from, .. } => *from,
            UnifiedEvent::TaskCreated { task_id } | UnifiedEvent::TaskUpdated { task_id } => {
                EntityId::Task(*task_id)
            }
            UnifiedEvent::ReminderCreated { reminder_id }
            | UnifiedEvent::ReminderUpdated { reminder_id }
            | UnifiedEvent::ReminderFired { reminder_id } => EntityId::Reminder(*reminder_id),
            _ => return None,
        })
    }
}

/// Events broadcast to in-process subscribers.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ApplicationEvent {
    /// Canonical/unified state advanced to `revision`.
    StateChanged {
        revision: Revision,
        changes: Arc<[UnifiedEvent]>,
    },
    SessionChanged {
        state: SessionState,
    },
    HydrationStatus {
        pending: usize,
        active: usize,
    },
    ReminderDue {
        reminder_id: ReminderId,
    },
    ActionAwaitingApproval {
        action_id: ActionId,
    },
    /// Events were dropped (queue overflow) or a subscriber lagged: views
    /// should reload from the store instead of trusting incremental updates.
    ResyncRequired {
        reason: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hydration_key_storage_roundtrip() {
        let keys = [
            HydrationKey::CurrentUser,
            HydrationKey::GuildChannels {
                guild_id: GuildId(9),
            },
            HydrationKey::DmConversation {
                conversation_id: ConversationId(4),
            },
            HydrationKey::User { user_id: UserId(3) },
            HydrationKey::VoiceState,
        ];
        for k in keys {
            assert_eq!(HydrationKey::parse_storage_key(&k.storage_key()), Some(k));
        }
        assert_eq!(HydrationKey::parse_storage_key("user:abc"), None);
    }

    #[test]
    fn discord_event_serializes_with_type_tag() {
        let e = DiscordEvent::MessageDeleted {
            message_id: MessageId(1),
            conversation_id: ConversationId(2),
        };
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["type"], "message_deleted");
        assert_eq!(e.kind(), "message_deleted");
    }

    #[test]
    fn unified_event_entity() {
        let e = UnifiedEvent::MessageCreated {
            message_id: MessageId(5),
            conversation_id: ConversationId(6),
        };
        assert_eq!(e.entity(), Some(EntityId::Message(MessageId(5))));
        assert_eq!(UnifiedEvent::SessionChanged.entity(), None);

        let imported = UnifiedEvent::MessageImported {
            message_id: MessageId(7),
            conversation_id: ConversationId(8),
        };
        assert_eq!(imported.kind(), "message_imported");
        assert_eq!(imported.entity(), Some(EntityId::Message(MessageId(7))));
    }
}
