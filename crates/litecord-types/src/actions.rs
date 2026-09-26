//! Agent actions and their audit trail.
//!
//! The Action Engine (`litecord-actions`) is the **only** agent-controlled path
//! toward external mutation. This module defines the data; policy, approval
//! tokens and execution live in that crate.
//!
//! `AgentAction` must stay free of hash maps and floats-with-NaN so its JSON
//! serialization is canonical: the approval hash is computed over it.

use serde::{Deserialize, Serialize};

use crate::capability::Capability;
use crate::ids::*;
use crate::provenance::DiscordIdentity;
use crate::social::{Activity, PresenceStatus};
use crate::tasks::{ReminderDraft, TaskDraft};
use crate::{Revision, Timestamp};

str_enum! {
    /// Risk class of an operation (V2 §26).
    pub enum CapabilityClass {
        Read => "read",
        LocalWrite => "local_write",
        DiscordWrite => "discord_write",
        Administrative => "administrative",
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MessageTarget {
    Conversation { conversation_id: ConversationId },
    User { user_id: UserId },
}

str_enum! {
    pub enum RelationshipAction {
        SendFriendRequest => "send_friend_request",
        AcceptFriendRequest => "accept_friend_request",
        RejectFriendRequest => "reject_friend_request",
        RemoveFriend => "remove_friend",
        Block => "block",
        Unblock => "unblock",
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresenceDraft {
    pub status: PresenceStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<Activity>,
}

/// Everything an agent (or the user, through the same engine) can ask to do.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentAction {
    CreateReminder {
        reminder: ReminderDraft,
    },
    CreateTask {
        task: TaskDraft,
    },
    CompleteTask {
        task_id: TaskId,
    },
    AddNote {
        user_id: UserId,
        note: String,
    },
    BookmarkMessage {
        message_id: MessageId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    DraftMessage {
        conversation_id: ConversationId,
        content: String,
    },
    SendMessage {
        target: MessageTarget,
        content: String,
    },
    EditMessage {
        message_id: MessageId,
        content: String,
    },
    DeleteMessage {
        message_id: MessageId,
    },
    ChangePresence {
        presence: PresenceDraft,
    },
    RelationshipChange {
        user_id: UserId,
        action: RelationshipAction,
    },
}

impl AgentAction {
    /// Stable machine name, used in audit logs and policy tables.
    pub const fn kind(&self) -> &'static str {
        match self {
            AgentAction::CreateReminder { .. } => "create_reminder",
            AgentAction::CreateTask { .. } => "create_task",
            AgentAction::CompleteTask { .. } => "complete_task",
            AgentAction::AddNote { .. } => "add_note",
            AgentAction::BookmarkMessage { .. } => "bookmark_message",
            AgentAction::DraftMessage { .. } => "draft_message",
            AgentAction::SendMessage { .. } => "send_message",
            AgentAction::EditMessage { .. } => "edit_message",
            AgentAction::DeleteMessage { .. } => "delete_message",
            AgentAction::ChangePresence { .. } => "change_presence",
            AgentAction::RelationshipChange { .. } => "relationship_change",
        }
    }

    /// Risk class. This mapping is the single source of truth; policy code
    /// builds on it rather than re-deriving risk per feature.
    pub const fn capability_class(&self) -> CapabilityClass {
        match self {
            AgentAction::CreateReminder { .. }
            | AgentAction::CreateTask { .. }
            | AgentAction::CompleteTask { .. }
            | AgentAction::AddNote { .. }
            | AgentAction::BookmarkMessage { .. }
            | AgentAction::DraftMessage { .. } => CapabilityClass::LocalWrite,
            AgentAction::SendMessage { .. }
            | AgentAction::EditMessage { .. }
            | AgentAction::DeleteMessage { .. }
            | AgentAction::ChangePresence { .. }
            | AgentAction::RelationshipChange { .. } => CapabilityClass::DiscordWrite,
        }
    }

    /// The backend capability a Discord write needs, if any.
    pub const fn required_capability(&self) -> Option<Capability> {
        match self {
            AgentAction::SendMessage { .. } => Some(Capability::DmSend),
            AgentAction::EditMessage { .. } => Some(Capability::DmEdit),
            AgentAction::DeleteMessage { .. } => Some(Capability::DmDelete),
            AgentAction::ChangePresence { .. } => Some(Capability::RichPresence),
            AgentAction::RelationshipChange { action, .. } => match action {
                RelationshipAction::Block | RelationshipAction::Unblock => {
                    Some(Capability::Blocking)
                }
                _ => Some(Capability::FriendRequests),
            },
            _ => None,
        }
    }
}

/// Who initiated something.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Actor {
    /// The local human user acting through the UI.
    User,
    /// An agent acting through a harness (MCP, scripted, ...).
    Agent {
        harness: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<AgentRunId>,
    },
    /// Deterministic local logic (reminder scheduler, heuristics).
    System,
}

impl Actor {
    pub fn label(&self) -> String {
        match self {
            Actor::User => "user".into(),
            Actor::Agent { harness, .. } => format!("agent:{harness}"),
            Actor::System => "system".into(),
        }
    }
}

str_enum! {
    pub enum ActionStatus {
        PendingApproval => "pending_approval",
        Approved => "approved",
        Rejected => "rejected",
        Executing => "executing",
        Executed => "executed",
        Failed => "failed",
        Expired => "expired",
        /// Approval invalidated by revalidation (state changed underneath).
        Invalidated => "invalidated",
    }
}

impl ActionStatus {
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            ActionStatus::Rejected
                | ActionStatus::Executed
                | ActionStatus::Failed
                | ActionStatus::Expired
                | ActionStatus::Invalidated
        )
    }
}

/// A persisted proposal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActionProposal {
    pub id: ActionId,
    pub action: AgentAction,
    pub actor: Actor,
    pub class: CapabilityClass,
    /// Which Discord identity would perform a Discord write.
    pub identity: DiscordIdentity,
    pub status: ActionStatus,
    /// Hex blake3 of the canonical action payload.
    pub payload_hash: String,
    /// Revision the proposer reasoned against.
    pub based_on_revision: Revision,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum AuditEvent {
    Proposed,
    AutoApproved,
    Approved { edited: bool },
    Rejected,
    ExecutionStarted,
    Executed { summary: String },
    Failed { error: String },
    Invalidated { reason: String },
    Expired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionAuditEntry {
    pub action_id: ActionId,
    pub at: Timestamp,
    pub actor: Actor,
    pub event: AuditEvent,
    pub revision: Revision,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classes() {
        let send = AgentAction::SendMessage {
            target: MessageTarget::User { user_id: UserId(1) },
            content: "hi".into(),
        };
        assert_eq!(send.capability_class(), CapabilityClass::DiscordWrite);
        assert_eq!(send.required_capability(), Some(Capability::DmSend));
        let draft = AgentAction::DraftMessage {
            conversation_id: ConversationId(1),
            content: "hi".into(),
        };
        assert_eq!(draft.capability_class(), CapabilityClass::LocalWrite);
    }

    #[test]
    fn action_json_is_tagged_and_stable() {
        let a = AgentAction::RelationshipChange {
            user_id: UserId(5),
            action: RelationshipAction::Block,
        };
        let s = serde_json::to_string(&a).unwrap();
        assert_eq!(
            s,
            r#"{"type":"relationship_change","user_id":"5","action":"block"}"#
        );
    }
}
