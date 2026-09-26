//! Executors perform approved actions.
//!
//! Local writes go to the store; Discord writes go to the `SocialBackend`.
//! An executor is only ever invoked by the engine after policy, approval and
//! revalidation — never directly by features, tools or UI.

use std::sync::Arc;

use async_trait::async_trait;
use serde::Serialize;
use tracing::Instrument;

use litecord_core::ports::SocialBackend;
use litecord_store::repos;
use litecord_store::Database;
use litecord_types::actions::{Actor, AgentAction};
use litecord_types::capability::{BackendMode, CapabilitySet};
use litecord_types::entity::EntityId;
use litecord_types::ids::ActionId;
use litecord_types::notes::Bookmark;
use litecord_types::provenance::{DiscordIdentity, Origin};
use litecord_types::tasks::TaskStatus;

use crate::error::ActionError;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ExecutionOutcome {
    /// Short, content-free summary for the audit log.
    pub summary: String,
    /// Object created or affected, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entity: Option<EntityId>,
}

#[async_trait]
pub trait ActionExecutor: Send + Sync + std::fmt::Debug {
    /// Backend capabilities (used for validation). Local-only executors
    /// report an empty set.
    fn capabilities(&self, identity: DiscordIdentity) -> CapabilitySet;

    async fn execute(
        &self,
        id: ActionId,
        action: &AgentAction,
        actor: &Actor,
        identity: DiscordIdentity,
    ) -> Result<ExecutionOutcome, ActionError>;
}

/// Provenance for local writes performed on behalf of `actor`.
pub fn origin_for(actor: &Actor) -> Origin {
    match actor {
        Actor::User => Origin::UserProvided,
        Actor::Agent { .. } => Origin::AgentDerived,
        Actor::System => Origin::LocalApplication,
    }
}

/// Executes local writes against the store and Discord writes through an
/// optional backend. Without a backend (e.g. in the MCP process) Discord
/// writes fail with `Unsupported`; they are only ever *proposed* there.
#[derive(Debug, Clone)]
pub struct DefaultExecutor {
    db: Database,
    backend: Option<Arc<dyn SocialBackend>>,
    /// Optional application-bot backend; used only for proposals whose
    /// identity is `ApplicationBot`.
    bot: Option<Arc<dyn SocialBackend>>,
}

impl DefaultExecutor {
    pub fn new(db: Database, backend: Option<Arc<dyn SocialBackend>>) -> Self {
        Self {
            db,
            backend,
            bot: None,
        }
    }

    /// Attach the application-bot backend.
    pub fn with_bot(mut self, bot: Arc<dyn SocialBackend>) -> Self {
        self.bot = Some(bot);
        self
    }

    fn backend_for(&self, identity: DiscordIdentity) -> Option<&Arc<dyn SocialBackend>> {
        match identity {
            DiscordIdentity::UserSocialSdk => self
                .backend
                .as_ref()
                .filter(|backend| backend.mode() != BackendMode::UserSession),
            // The user-session backend is a read-only source. Keep it out of
            // proposal execution even if it is the configured backend.
            DiscordIdentity::UserSession => None,
            DiscordIdentity::ApplicationBot => self.bot.as_ref(),
        }
    }

    fn local(&self, action: &AgentAction, origin: Origin) -> Result<ExecutionOutcome, ActionError> {
        let outcome = self
            .db
            .write(|tx| -> Result<ExecutionOutcome, ActionError> {
                Ok(match action {
                    AgentAction::CreateReminder { reminder } => {
                        let id = repos::reminders::create(tx, reminder, origin)?;
                        ExecutionOutcome {
                            summary: "reminder created".into(),
                            entity: Some(EntityId::Reminder(id)),
                        }
                    }
                    AgentAction::CreateTask { task } => {
                        // Agent-created tasks start as candidates the user confirms.
                        let status = if origin == Origin::AgentDerived {
                            TaskStatus::Candidate
                        } else {
                            TaskStatus::Open
                        };
                        let id = repos::tasks::create(tx, task, status, origin)?;
                        ExecutionOutcome {
                            summary: format!("task created ({})", status.as_str()),
                            entity: Some(EntityId::Task(id)),
                        }
                    }
                    AgentAction::CompleteTask { task_id } => {
                        repos::tasks::set_status(tx, *task_id, TaskStatus::Done, origin)?;
                        ExecutionOutcome {
                            summary: "task completed".into(),
                            entity: Some(EntityId::Task(*task_id)),
                        }
                    }
                    AgentAction::AddNote { user_id, note } => {
                        repos::notes::append_note(tx, *user_id, note, origin)?;
                        ExecutionOutcome {
                            summary: "note added".into(),
                            entity: Some(EntityId::User(*user_id)),
                        }
                    }
                    AgentAction::BookmarkMessage { message_id, note } => {
                        let msg = repos::messages::get(tx, *message_id)?
                            .ok_or_else(|| ActionError::Invalid("message not found".into()))?;
                        repos::notes::add_bookmark(
                            tx,
                            &Bookmark {
                                message_id: *message_id,
                                conversation_id: msg.message.conversation_id,
                                note: note.clone(),
                                created_at: tx.now(),
                            },
                            origin,
                        )?;
                        ExecutionOutcome {
                            summary: "message bookmarked".into(),
                            entity: Some(EntityId::Message(*message_id)),
                        }
                    }
                    AgentAction::DraftMessage {
                        conversation_id,
                        content,
                    } => {
                        let id = repos::drafts::create(tx, *conversation_id, content, origin)?;
                        ExecutionOutcome {
                            summary: format!("draft {id} saved"),
                            entity: Some(EntityId::Conversation(*conversation_id)),
                        }
                    }
                    _ => {
                        return Err(ActionError::Internal(format!(
                            "`{}` is not a local action",
                            action.kind()
                        )))
                    }
                })
            })?;
        Ok(outcome.value)
    }
}

#[async_trait]
impl ActionExecutor for DefaultExecutor {
    fn capabilities(&self, identity: DiscordIdentity) -> CapabilitySet {
        self.backend_for(identity)
            .map(|b| b.capabilities())
            .unwrap_or_default()
    }

    async fn execute(
        &self,
        id: ActionId,
        action: &AgentAction,
        actor: &Actor,
        identity: DiscordIdentity,
    ) -> Result<ExecutionOutcome, ActionError> {
        let span = tracing::info_span!(
            "action_execute",
            action_id = %id,
            kind = action.kind(),
            identity = identity.as_str()
        );
        self.execute_inner(action, actor, identity)
            .instrument(span)
            .await
    }
}

impl DefaultExecutor {
    async fn execute_inner(
        &self,
        action: &AgentAction,
        actor: &Actor,
        identity: DiscordIdentity,
    ) -> Result<ExecutionOutcome, ActionError> {
        let backend = || {
            self.backend_for(identity).ok_or_else(|| {
                ActionError::Execution(format!("no {} backend in this process", identity.as_str()))
            })
        };
        let exec_err =
            |e: litecord_core::ports::BackendError| ActionError::Execution(e.to_string());
        match action {
            AgentAction::SendMessage {
                target,
                content,
                reply_to,
            } => {
                let backend = backend()?;
                let msg = match reply_to {
                    Some(reply_to) => backend.send_reply(target, content, *reply_to).await,
                    None => backend.send_message(target, content).await,
                }
                .map_err(exec_err)?;
                Ok(ExecutionOutcome {
                    summary: "message sent".into(),
                    entity: Some(EntityId::Message(msg.id)),
                })
            }
            AgentAction::EditMessage {
                message_id,
                content,
            } => {
                backend()?
                    .edit_message(*message_id, content)
                    .await
                    .map_err(exec_err)?;
                Ok(ExecutionOutcome {
                    summary: "message edited".into(),
                    entity: Some(EntityId::Message(*message_id)),
                })
            }
            AgentAction::DeleteMessage { message_id } => {
                backend()?
                    .delete_message(*message_id)
                    .await
                    .map_err(exec_err)?;
                Ok(ExecutionOutcome {
                    summary: "message deleted".into(),
                    entity: Some(EntityId::Message(*message_id)),
                })
            }
            AgentAction::ChangePresence { presence } => {
                backend()?.set_presence(presence).await.map_err(exec_err)?;
                Ok(ExecutionOutcome {
                    summary: format!("presence set to {}", presence.status),
                    entity: None,
                })
            }
            AgentAction::RelationshipChange { user_id, action } => {
                backend()?
                    .relationship_action(*user_id, *action)
                    .await
                    .map_err(exec_err)?;
                Ok(ExecutionOutcome {
                    summary: format!("relationship action {}", action.as_str()),
                    entity: Some(EntityId::User(*user_id)),
                })
            }
            local => self.local(local, origin_for(actor)),
        }
    }
}
