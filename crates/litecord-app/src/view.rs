//! UI-framework-agnostic view models.
//!
//! Plain, `Serialize`-able snapshots produced by [`crate::LitecordApp`]
//! methods. A UI renders them and re-requests on
//! `ApplicationEvent::StateChanged` (compare `as_of_revision`). They contain
//! ids for every referenced entity so the UI never needs its own copy of
//! canonical state. Synthetic (demo) data is flagged via `origin` so it can be
//! labelled.

use serde::Serialize;

use litecord_core::metrics::MetricsSnapshot;
use litecord_features::command::CommandMatch;
use litecord_features::feature::{FeatureInfo, MessageAction, RenderMessage, SettingDescriptor};
use litecord_types::actions::{ActionStatus, CapabilityClass};
use litecord_types::capability::{BackendMode, CapabilitySet, HistoryCapability};
use litecord_types::entity::EntityId;
use litecord_types::ids::*;
use litecord_types::memory::{Edge, MemoryItem, MemoryStatus};
use litecord_types::provenance::{DiscordIdentity, Origin};
use litecord_types::social::*;
use litecord_types::tasks::{Reminder, Task, TaskComment};
use litecord_types::trust::AgentVisibility;
use litecord_types::{Revision, Timestamp};

#[derive(Debug, Clone, Serialize)]
pub struct FriendRow {
    pub user_id: UserId,
    pub display_name: String,
    pub username: String,
    pub avatar_url: Option<String>,
    pub status: PresenceStatus,
    pub activity: Option<Activity>,
    pub relationship: RelationshipKind,
    pub alias: Option<String>,
    pub favorite: bool,
    pub dm_conversation_id: Option<ConversationId>,
    pub origin: Origin,
}

#[derive(Debug, Clone, Serialize)]
pub struct FriendsViewModel {
    pub as_of_revision: Revision,
    pub online: Vec<FriendRow>,
    pub offline: Vec<FriendRow>,
    pub pending_incoming: Vec<FriendRow>,
    pub pending_outgoing: Vec<FriendRow>,
    pub blocked: Vec<FriendRow>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConversationRow {
    pub conversation_id: ConversationId,
    pub kind: ConversationKind,
    pub title: String,
    pub recipient_id: Option<UserId>,
    pub recipient_status: Option<PresenceStatus>,
    pub last_activity_at: Option<Timestamp>,
    pub last_message_preview: Option<String>,
    pub awaiting_reply: bool,
    pub agent_visibility: AgentVisibility,
    pub origin: Origin,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConversationListViewModel {
    pub as_of_revision: Revision,
    pub conversations: Vec<ConversationRow>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MessageRow {
    pub message_id: MessageId,
    pub author_id: UserId,
    pub is_mine: bool,
    pub sent_at: Timestamp,
    pub edited: bool,
    pub bookmarked: bool,
    pub extras: Vec<MessageExtra>,
    pub origin: Origin,
    /// Feature-transformed presentation (compact, highlight, privacy blur).
    pub render: RenderMessage,
    /// Context-menu entries contributed by features.
    pub actions: Vec<MessageAction>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConversationCapabilities {
    pub can_send: bool,
    pub can_edit: bool,
    pub can_delete: bool,
    pub history: Option<HistoryCapability>,
    /// Deep link when content is only available in Discord.
    pub open_in_discord_url: String,
    /// Which Discord identity a message typed here would be sent as
    /// (`application_bot` for guild channels served by the bot). `None`
    /// when no connected identity can post here. Show this in the composer.
    pub send_identity: Option<DiscordIdentity>,
    /// `send_identity` can reply to a message here (bot: yes; the Social
    /// SDK sends plain messages only).
    pub can_reply: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConversationViewModel {
    pub as_of_revision: Revision,
    pub conversation_id: ConversationId,
    pub title: String,
    /// Oldest first; a window of the most recent messages (virtualize in UI).
    pub messages: Vec<MessageRow>,
    /// Pass the oldest `sent_at` as `before` to page further back.
    pub has_more: bool,
    pub capabilities: ConversationCapabilities,
    pub agent_visibility: AgentVisibility,
    pub open_drafts: Vec<litecord_types::tasks::Draft>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InboxItem {
    PendingReply {
        conversation_id: ConversationId,
        from: String,
        preview: String,
        at: Timestamp,
    },
    ReminderDue {
        reminder_id: ReminderId,
        title: String,
        due_at: Timestamp,
        conversation_id: Option<ConversationId>,
    },
    TaskCandidate {
        task_id: TaskId,
        title: String,
        origin: Origin,
    },
    Commitment {
        memory_id: MemoryId,
        text: String,
        due_at: Option<Timestamp>,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct PendingActionRow {
    pub action_id: ActionId,
    pub kind: &'static str,
    pub class: CapabilityClass,
    pub status: ActionStatus,
    /// Human-readable one-liner ("Send to ada: …").
    pub summary: String,
    pub target_label: Option<String>,
    /// Full content for review (the user must see exactly what is approved).
    pub content: Option<String>,
    pub actor: String,
    /// The Discord identity that would perform the action. Always shown to
    /// the user before approval; bot and user are never interchangeable.
    pub identity: DiscordIdentity,
    pub rationale: Option<String>,
    pub created_at: Timestamp,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentInboxViewModel {
    pub as_of_revision: Revision,
    pub needs_attention: Vec<InboxItem>,
    pub pending_actions: Vec<PendingActionRow>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TasksViewModel {
    pub as_of_revision: Revision,
    pub open: Vec<Task>,
    pub candidates: Vec<Task>,
    pub reminders: Vec<Reminder>,
}

/// A single task with its subtasks and comments, for a task detail screen.
#[derive(Debug, Clone, Serialize)]
pub struct TaskDetailViewModel {
    pub as_of_revision: Revision,
    pub task: Task,
    pub subtasks: Vec<Task>,
    pub comments: Vec<TaskComment>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MemoryViewModel {
    pub as_of_revision: Revision,
    pub entity: Option<EntityId>,
    pub memories: Vec<MemoryItem>,
    pub edges: Vec<Edge>,
    pub counts_by_status: Vec<(MemoryStatus, i64)>,
}

#[derive(Debug, Clone, Serialize)]
pub struct VoiceViewModel {
    pub state: VoiceState,
    pub participant_names: Vec<(UserId, String)>,
    pub voice_supported: bool,
    pub devices_supported: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SettingRow {
    pub descriptor: SettingDescriptor,
    pub value: serde_json::Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct SettingsViewModel {
    pub account_access: Option<litecord_types::capability::SessionAccessMode>,
    pub can_enable_account_writes: bool,
    pub outbound: Vec<litecord_store::repos::outbound::OutboundOperation>,
    pub sections: Vec<(String, Vec<SettingRow>)>,
    pub features: Vec<FeatureInfo>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChannelRow {
    pub channel: Channel,
    pub open_in_discord_url: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct GuildRow {
    pub guild: Guild,
    pub channels: Vec<ChannelRow>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GuildsViewModel {
    pub as_of_revision: Revision,
    pub guilds: Vec<GuildRow>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DiagnosticsViewModel {
    pub revision: Revision,
    pub session: SessionState,
    pub backend_mode: BackendMode,
    pub capabilities: CapabilitySet,
    pub hydration_pending: usize,
    pub hydration_active: usize,
    pub counts: StoreCounts,
    pub metrics: MetricsSnapshot,
    /// Optional application-bot source status.
    pub bot: Option<BotStatus>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BotStatus {
    pub session: SessionState,
    pub mode: BackendMode,
    pub capabilities: CapabilitySet,
    pub bot_user_id: Option<UserId>,
    pub hydration_pending: usize,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct StoreCounts {
    pub users: i64,
    pub messages: i64,
    pub memories_by_status: Vec<(MemoryStatus, i64)>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CommandPaletteViewModel {
    pub query: String,
    pub matches: Vec<CommandMatch>,
}

/// Effects the application cannot perform itself and hands to the UI.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UiEffect {
    Navigate {
        target: litecord_features::intent::NavTarget,
    },
    CopyToClipboard {
        text: String,
    },
    OpenUrl {
        url: String,
    },
    Notice {
        message: String,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct FileRow {
    pub message_id: MessageId,
    pub author_id: UserId,
    pub author_name: String,
    pub sent_at: Timestamp,
    pub filename: String,
    pub content_type: Option<String>,
    pub size_bytes: u64,
    pub origin: Origin,
    /// Downloads are not cached locally; open the message in Discord.
    pub open_in_discord_url: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct FilesViewModel {
    pub as_of_revision: Revision,
    pub conversation_id: ConversationId,
    pub files: Vec<FileRow>,
    /// Pass the oldest `sent_at` as `before` to page further back.
    pub has_more: bool,
}

/// Where a command runs: the UI's current focus.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CommandScope {
    pub active_conversation: Option<ConversationId>,
    pub selected_message: Option<MessageId>,
}
