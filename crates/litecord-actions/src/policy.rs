//! Centralized action policy.
//!
//! This is the **single** place that decides whether an action may execute
//! immediately, needs explicit user approval, or is denied. Features and tools
//! never implement their own "does this need confirmation?" checks; they hand
//! the action to the engine, which asks this policy.
//!
//! Hard invariants (enforced here and covered by exhaustive tests):
//! * An agent's Discord write is **never** executed without approval
//!   (Discord requires user-initiated sends/relationship changes).
//! * Agents may never perform administrative actions.
//! * Configuration can only make agent permissions *stricter* than these
//!   invariants, never looser.

use serde::Serialize;

use litecord_core::config::AgentConfig;
use litecord_types::actions::{Actor, AgentAction, CapabilityClass};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum PolicyDecision {
    /// Execute now (still audited).
    Execute,
    /// Persist as a proposal awaiting explicit user approval.
    RequireApproval,
    /// Refuse; the reason is safe to show to agents.
    Deny { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionPolicy {
    pub auto_approve_local_writes: bool,
    pub allow_message_proposals: bool,
    pub allow_presence_proposals: bool,
    pub allow_relationship_proposals: bool,
}

impl Default for ActionPolicy {
    fn default() -> Self {
        Self::from_config(&AgentConfig::default())
    }
}

impl ActionPolicy {
    pub fn from_config(cfg: &AgentConfig) -> Self {
        Self {
            auto_approve_local_writes: cfg.auto_approve_local_writes,
            allow_message_proposals: cfg.allow_message_proposals,
            allow_presence_proposals: cfg.allow_presence_proposals,
            allow_relationship_proposals: cfg.allow_relationship_proposals,
        }
    }

    pub fn decide(&self, action: &AgentAction, actor: &Actor) -> PolicyDecision {
        let class = action.capability_class();
        match actor {
            // The user acting through the UI *is* the explicit approval.
            Actor::User => PolicyDecision::Execute,
            Actor::System => match class {
                CapabilityClass::Read | CapabilityClass::LocalWrite => PolicyDecision::Execute,
                // e.g. a reminder suggesting a message: surface it, never send.
                CapabilityClass::DiscordWrite => PolicyDecision::RequireApproval,
                CapabilityClass::Administrative => {
                    deny("system actors cannot run administrative actions")
                }
            },
            Actor::Agent { .. } => self.decide_agent(action, class),
        }
    }

    fn decide_agent(&self, action: &AgentAction, class: CapabilityClass) -> PolicyDecision {
        match class {
            CapabilityClass::Read => PolicyDecision::Execute,
            CapabilityClass::LocalWrite => {
                if self.auto_approve_local_writes {
                    PolicyDecision::Execute
                } else {
                    PolicyDecision::RequireApproval
                }
            }
            CapabilityClass::DiscordWrite => {
                let allowed = match action {
                    AgentAction::SendMessage { .. }
                    | AgentAction::EditMessage { .. }
                    | AgentAction::DeleteMessage { .. } => self.allow_message_proposals,
                    AgentAction::ChangePresence { .. } => self.allow_presence_proposals,
                    AgentAction::RelationshipChange { .. } => self.allow_relationship_proposals,
                    // Any future Discord write defaults to denied until
                    // explicitly categorized here.
                    _ => false,
                };
                if allowed {
                    PolicyDecision::RequireApproval
                } else {
                    deny(&format!(
                        "agents are not permitted to propose `{}` (see agent settings)",
                        action.kind()
                    ))
                }
            }
            CapabilityClass::Administrative => deny("agents cannot run administrative actions"),
        }
    }
}

fn deny(reason: &str) -> PolicyDecision {
    PolicyDecision::Deny {
        reason: reason.to_owned(),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use litecord_types::actions::*;
    use litecord_types::ids::*;
    use litecord_types::social::PresenceStatus;
    use litecord_types::tasks::{ReminderDraft, ReminderTrigger, TaskDraft, TaskPriority};
    use litecord_types::Timestamp;

    /// One instance of every action variant.
    pub(crate) fn all_actions() -> Vec<AgentAction> {
        vec![
            AgentAction::CreateReminder {
                reminder: ReminderDraft {
                    title: "r".into(),
                    note: None,
                    trigger: ReminderTrigger::At { at: Timestamp(1) },
                    conversation_id: None,
                    task_id: None,
                    source: None,
                },
            },
            AgentAction::CreateTask {
                task: TaskDraft {
                    title: "t".into(),
                    description: None,
                    priority: TaskPriority::default(),
                    due_at: None,
                    related_users: vec![],
                    conversation_id: None,
                    parent_id: None,
                    source: None,
                },
            },
            AgentAction::CompleteTask { task_id: TaskId(1) },
            AgentAction::AddNote {
                user_id: UserId(1),
                note: "n".into(),
            },
            AgentAction::BookmarkMessage {
                message_id: MessageId(1),
                note: None,
            },
            AgentAction::DraftMessage {
                conversation_id: ConversationId(1),
                content: "d".into(),
            },
            AgentAction::SendMessage {
                target: MessageTarget::User { user_id: UserId(1) },
                content: "s".into(),
            },
            AgentAction::EditMessage {
                message_id: MessageId(1),
                content: "e".into(),
            },
            AgentAction::DeleteMessage {
                message_id: MessageId(1),
            },
            AgentAction::ChangePresence {
                presence: PresenceDraft {
                    status: PresenceStatus::Idle,
                    activity: None,
                },
            },
            AgentAction::RelationshipChange {
                user_id: UserId(1),
                action: RelationshipAction::Block,
            },
        ]
    }

    fn agent() -> Actor {
        Actor::Agent {
            harness: "test".into(),
            run_id: None,
        }
    }

    #[test]
    fn agent_discord_writes_never_execute_under_any_config() {
        for bits in 0..16u8 {
            let policy = ActionPolicy {
                auto_approve_local_writes: bits & 1 != 0,
                allow_message_proposals: bits & 2 != 0,
                allow_presence_proposals: bits & 4 != 0,
                allow_relationship_proposals: bits & 8 != 0,
            };
            for action in all_actions() {
                let d = policy.decide(&action, &agent());
                if action.capability_class() == CapabilityClass::DiscordWrite {
                    assert_ne!(d, PolicyDecision::Execute, "{}", action.kind());
                }
            }
        }
    }

    #[test]
    fn local_writes_follow_auto_approve_setting() {
        let mut p = ActionPolicy::default();
        let a = &all_actions()[0];
        p.auto_approve_local_writes = true;
        assert_eq!(p.decide(a, &agent()), PolicyDecision::Execute);
        p.auto_approve_local_writes = false;
        assert_eq!(p.decide(a, &agent()), PolicyDecision::RequireApproval);
    }

    #[test]
    fn relationship_proposals_denied_by_default() {
        let p = ActionPolicy::default();
        let block = all_actions().pop().unwrap();
        assert!(matches!(
            p.decide(&block, &agent()),
            PolicyDecision::Deny { .. }
        ));
        let send = &all_actions()[6];
        assert_eq!(p.decide(send, &agent()), PolicyDecision::RequireApproval);
    }

    #[test]
    fn user_actions_execute_and_system_discord_writes_need_approval() {
        let p = ActionPolicy::default();
        let send = &all_actions()[6];
        assert_eq!(p.decide(send, &Actor::User), PolicyDecision::Execute);
        assert_eq!(
            p.decide(send, &Actor::System),
            PolicyDecision::RequireApproval
        );
    }
}
