//! Operational memory services: reminders and the task lifecycle (V2 §22).
//!
//! Both services are deliberately thin wrappers over `repos::{reminders,
//! tasks}` — the interesting invariants (a task is only ever *created* as
//! `Candidate`/`Open`; a reminder's `Fired` transition stamps `fired_at`) are
//! already enforced at the store layer. What lives here is the
//! *local-application logic* layered on top: evaluating conditions with no
//! LLM involved, and the confirm/dismiss state machine a candidate task goes
//! through before it becomes a real commitment.

use litecord_store::repos::tasks::TaskFilter;
use litecord_store::{repos, Database};
use litecord_types::provenance::Origin;
use litecord_types::tasks::{
    ReminderCondition, ReminderStatus, ReminderTrigger, Task, TaskComment, TaskPriority, TaskStatus,
};
use litecord_types::{ConversationId, ReminderId, TaskId, Timestamp};

use crate::error::{MemoryError, MemoryResult};

/// One reminder that actually fired during a [`ReminderEngine::tick`] (a
/// conditional reminder whose condition turned out to hold is marked
/// `Satisfied` instead and is *not* included here — nothing needs to notify
/// anyone about it).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ReminderFired {
    pub reminder_id: ReminderId,
    pub title: String,
    pub conversation_id: Option<ConversationId>,
}

/// Evaluates due reminders. `At` reminders always fire once due; `Conditional`
/// reminders are checked against current message state and either fire (the
/// condition still holds — someone needs to act) or resolve as `Satisfied`
/// (the condition no longer holds — nothing to do).
#[derive(Clone, Debug)]
pub struct ReminderEngine {
    db: Database,
}

/// Reminders handled per [`ReminderEngine::tick`] call. Matches
/// `repos::reminders::due`'s own cap; ticking is expected to run frequently
/// enough that a backlog beyond this is unusual.
const TICK_BATCH: u32 = 100;

impl ReminderEngine {
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    /// Evaluate every reminder due at or before `now`.
    ///
    /// Implementation note: all due reminders for this tick are resolved
    /// inside **one** write transaction (one revision bump for the whole
    /// batch) rather than one transaction per reminder. Ticks are expected to
    /// be small and this keeps a tick atomic — either the whole batch is
    /// accounted for or (on error) none of it is — at the cost of holding the
    /// write lock slightly longer than the strictly minimal per-row approach
    /// would.
    pub fn tick(&self, now: Timestamp) -> MemoryResult<Vec<ReminderFired>> {
        let committed = self.db.write(|tx| -> MemoryResult<Vec<ReminderFired>> {
            let due = repos::reminders::due(tx, now, TICK_BATCH)?;
            let mut fired = Vec::new();
            for reminder in due {
                let resolved = match &reminder.trigger {
                    ReminderTrigger::At { .. } => ReminderStatus::Fired,
                    ReminderTrigger::Conditional { condition, .. } => match condition {
                        ReminderCondition::NoReplyFrom {
                            user_id,
                            conversation_id,
                            since,
                        } => {
                            let replied = repos::messages::has_message_from_since(
                                tx,
                                *conversation_id,
                                *user_id,
                                *since,
                            )?;
                            if replied {
                                ReminderStatus::Satisfied
                            } else {
                                ReminderStatus::Fired
                            }
                        }
                    },
                };
                repos::reminders::set_status(tx, reminder.id, resolved, Origin::LocalApplication)?;
                if resolved == ReminderStatus::Fired {
                    fired.push(ReminderFired {
                        reminder_id: reminder.id,
                        title: reminder.title.clone(),
                        conversation_id: reminder.conversation_id,
                    });
                }
            }
            Ok(fired)
        })?;
        Ok(committed.value)
    }

    /// The due time of the earliest pending reminder, if any.
    pub fn next_due(&self) -> MemoryResult<Option<Timestamp>> {
        self.db.read(|r| -> MemoryResult<_> {
            let pending = repos::reminders::list(
                r,
                &litecord_store::repos::reminders::ReminderFilter {
                    statuses: Some(vec![ReminderStatus::Pending]),
                    limit: 1,
                    ..Default::default()
                },
            )?;
            Ok(pending.first().map(|r| r.trigger.due_at()))
        })
    }
}

/// The task confirm/dismiss/complete lifecycle. Extraction only ever creates
/// `Candidate` tasks (see
/// [`crate::service::MemoryService::on_message_created`]); everything past
/// that point is the user acting on a suggestion, which is what this service
/// mediates.
#[derive(Clone, Debug)]
pub struct TaskService {
    db: Database,
}

impl TaskService {
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    /// Promote a candidate task to `Open`. The resulting status change is
    /// recorded as `Origin::UserProvided`: confirming a candidate is a user
    /// decision, even though the task itself was suggested locally.
    ///
    /// Errors with [`MemoryError::NotFound`] if `id` does not exist, or
    /// [`MemoryError::Invalid`] if it is not currently `Candidate`.
    pub fn confirm_candidate(&self, id: TaskId) -> MemoryResult<()> {
        self.db.write(|tx| -> MemoryResult<()> {
            let task = require_candidate(tx, id)?;
            repos::tasks::set_status(tx, task.id, TaskStatus::Open, Origin::UserProvided)?;
            Ok(())
        })?;
        Ok(())
    }

    /// Dismiss a candidate task (the user does not want it). Same error
    /// conditions as [`Self::confirm_candidate`].
    pub fn dismiss(&self, id: TaskId) -> MemoryResult<()> {
        self.db.write(|tx| -> MemoryResult<()> {
            let task = require_candidate(tx, id)?;
            repos::tasks::set_status(tx, task.id, TaskStatus::Dismissed, Origin::UserProvided)?;
            Ok(())
        })?;
        Ok(())
    }

    /// Mark a task `Done` under the given `origin` (a user completing it is
    /// `UserProvided`; an agent or automation completing it on the user's
    /// behalf records its own origin instead).
    pub fn complete(&self, id: TaskId, origin: Origin) -> MemoryResult<bool> {
        let committed = self.db.write(|tx| -> MemoryResult<bool> {
            Ok(repos::tasks::set_status(tx, id, TaskStatus::Done, origin)?)
        })?;
        Ok(committed.value)
    }

    /// `Open` and `Candidate` tasks, due date ascending (see
    /// `repos::tasks::list`'s ordering), for the "what needs attention" view.
    pub fn open_and_candidates(&self, limit: u32) -> MemoryResult<Vec<Task>> {
        self.db.read(|r| -> MemoryResult<_> {
            Ok(repos::tasks::list(
                r,
                &TaskFilter {
                    statuses: Some(vec![TaskStatus::Open, TaskStatus::Candidate]),
                    limit,
                    ..Default::default()
                },
            )?)
        })
    }

    /// Change a task's priority, recorded under `origin` (a user changing it
    /// is `UserProvided`).
    pub fn set_priority(
        &self,
        id: TaskId,
        priority: TaskPriority,
        origin: Origin,
    ) -> MemoryResult<bool> {
        let committed = self.db.write(|tx| -> MemoryResult<bool> {
            Ok(repos::tasks::set_priority(tx, id, priority, origin)?)
        })?;
        Ok(committed.value)
    }

    /// Add a comment to a task. Comments typed by the user are recorded as
    /// `Origin::UserProvided`.
    pub fn add_comment(&self, id: TaskId, body: &str, origin: Origin) -> MemoryResult<i64> {
        let committed = self.db.write(|tx| -> MemoryResult<i64> {
            Ok(repos::tasks::add_comment(tx, id, body, origin)?)
        })?;
        Ok(committed.value)
    }

    /// Direct subtasks of `id`.
    pub fn subtasks(&self, id: TaskId) -> MemoryResult<Vec<Task>> {
        self.db.read(|r| Ok(repos::tasks::subtasks(r, id)?))
    }

    /// A task's comments, oldest first.
    pub fn comments(&self, id: TaskId) -> MemoryResult<Vec<TaskComment>> {
        self.db.read(|r| Ok(repos::tasks::comments(r, id)?))
    }
}

fn require_candidate(tx: &litecord_store::WriteTx<'_>, id: TaskId) -> MemoryResult<Task> {
    let task =
        repos::tasks::get(tx, id)?.ok_or_else(|| MemoryError::NotFound(format!("task {id:?}")))?;
    if task.status != TaskStatus::Candidate {
        return Err(MemoryError::Invalid(format!(
            "task {id:?} is not a candidate (status: {})",
            task.status
        )));
    }
    Ok(task)
}
