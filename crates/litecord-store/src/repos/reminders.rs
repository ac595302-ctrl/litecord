//! Reminders: time-based (`At`) or condition-checked (`Conditional`), both
//! stored under one `due_at` column — for `Conditional` this is the trigger's
//! `check_at` (the moment the condition should next be evaluated).

use rusqlite::{params, types::Value, Connection, OptionalExtension, Row};

use litecord_core::events::UnifiedEvent;
use litecord_types::provenance::{Origin, SourceRef};
use litecord_types::tasks::{
    Reminder, ReminderCondition, ReminderDraft, ReminderStatus, ReminderTrigger,
};
use litecord_types::{ConversationId, ReminderId, TaskId, Timestamp};

use crate::db::WriteTx;
use crate::error::{StoreError, StoreResult};
use crate::sql::{col_err, push_in_str, rev, ts};

const SELECT_COLUMNS: &str =
    "id, title, note, trigger_kind, due_at, condition_json, status, origin, \
     conversation_id, task_id, source_entity, source_note, created_at, fired_at, revision";

/// Create a reminder in `Pending` status. Emits
/// [`UnifiedEvent::ReminderCreated`].
pub fn create(tx: &WriteTx<'_>, draft: &ReminderDraft, origin: Origin) -> StoreResult<ReminderId> {
    let title = draft.title.trim();
    if title.is_empty() {
        return Err(StoreError::Invariant(
            "reminder title must not be empty".into(),
        ));
    }
    let (trigger_kind, due_at, condition_json) = match &draft.trigger {
        ReminderTrigger::At { at } => ("at", *at, None),
        ReminderTrigger::Conditional {
            condition,
            check_at,
        } => (
            "conditional",
            *check_at,
            Some(serde_json::to_string(condition)?),
        ),
    };
    let (source_entity, source_note) = match &draft.source {
        Some(s) => (Some(s.entity.to_string()), s.note.clone()),
        None => (None, None),
    };
    let now = tx.now();
    let revision = tx.revision().get() as i64;
    tx.execute(
        "INSERT INTO reminders \
            (title, note, trigger_kind, due_at, condition_json, status, origin, conversation_id, \
             task_id, source_entity, source_note, created_at, fired_at, revision) \
         VALUES (?, ?, ?, ?, ?, 'pending', ?, ?, ?, ?, ?, ?, NULL, ?)",
        params![
            title,
            draft.note,
            trigger_kind,
            due_at.as_millis(),
            condition_json,
            origin.as_str(),
            draft.conversation_id.map(|c| c.to_sql()),
            draft.task_id.map(TaskId::get),
            source_entity,
            source_note,
            now.as_millis(),
            revision,
        ],
    )?;
    let id = ReminderId(tx.last_insert_rowid());
    tx.emit(UnifiedEvent::ReminderCreated { reminder_id: id }, origin)?;
    Ok(id)
}

fn row_to_reminder(row: &Row<'_>) -> rusqlite::Result<Reminder> {
    let id = ReminderId(row.get(0)?);
    let title: String = row.get(1)?;
    let note: Option<String> = row.get(2)?;
    let trigger_kind: String = row.get(3)?;
    let due_at: i64 = row.get(4)?;
    let condition_json: Option<String> = row.get(5)?;
    let trigger = match trigger_kind.as_str() {
        "at" => ReminderTrigger::At {
            at: Timestamp::from_millis(due_at),
        },
        "conditional" => {
            let json = condition_json.ok_or_else(|| {
                col_err(
                    5,
                    litecord_types::ValidationError::invalid(
                        "condition_json",
                        "missing for conditional reminder",
                    ),
                )
            })?;
            let condition: ReminderCondition =
                serde_json::from_str(&json).map_err(|e| col_err(5, e))?;
            ReminderTrigger::Conditional {
                condition,
                check_at: Timestamp::from_millis(due_at),
            }
        }
        other => {
            return Err(col_err(
                3,
                litecord_types::ValidationError::Parse {
                    what: "reminder trigger_kind",
                    input: other.to_string(),
                },
            ))
        }
    };
    let status: String = row.get(6)?;
    let status = ReminderStatus::parse(&status).map_err(|e| col_err(6, e))?;
    let origin_s: String = row.get(7)?;
    let origin = crate::sql::origin(7, &origin_s)?;
    let conversation_id: Option<i64> = row.get(8)?;
    let task_id: Option<i64> = row.get(9)?;
    let source_entity: Option<String> = row.get(10)?;
    let source_note: Option<String> = row.get(11)?;
    let source = match source_entity {
        Some(e) => Some(SourceRef {
            entity: e.parse().map_err(|e| col_err(10, e))?,
            note: source_note,
        }),
        None => None,
    };
    let created_at: i64 = row.get(12)?;
    let fired_at: Option<i64> = row.get(13)?;
    let revision: i64 = row.get(14)?;
    Ok(Reminder {
        id,
        title,
        note,
        trigger,
        status,
        origin,
        conversation_id: conversation_id.map(ConversationId::from_sql),
        task_id: task_id.map(TaskId),
        source,
        created_at: Timestamp::from_millis(created_at),
        fired_at: ts(fired_at),
        revision: rev(revision),
    })
}

/// Load a reminder by id.
pub fn get(conn: &Connection, id: ReminderId) -> StoreResult<Option<Reminder>> {
    let sql = format!("SELECT {SELECT_COLUMNS} FROM reminders WHERE id = ?");
    Ok(conn
        .query_row(&sql, params![id.get()], row_to_reminder)
        .optional()?)
}

/// Filter for [`list`]. `limit = 0` means unlimited.
#[derive(Debug, Clone, Default)]
pub struct ReminderFilter {
    pub statuses: Option<Vec<ReminderStatus>>,
    pub due_before: Option<Timestamp>,
    pub conversation_id: Option<ConversationId>,
    pub limit: u32,
}

/// List reminders matching `filter`, due date ascending.
pub fn list(conn: &Connection, filter: &ReminderFilter) -> StoreResult<Vec<Reminder>> {
    let mut sql = format!("SELECT {SELECT_COLUMNS} FROM reminders");
    let mut conditions = Vec::new();
    let mut params: Vec<Value> = Vec::new();

    if let Some(statuses) = &filter.statuses {
        push_in_str(
            &mut conditions,
            &mut params,
            "status",
            statuses.iter().map(|s| s.as_str()),
        );
    }
    if let Some(before) = filter.due_before {
        conditions.push("due_at < ?".to_string());
        params.push(Value::Integer(before.as_millis()));
    }
    if let Some(conv) = filter.conversation_id {
        conditions.push("conversation_id = ?".to_string());
        params.push(Value::Integer(conv.to_sql()));
    }
    if !conditions.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&conditions.join(" AND "));
    }
    sql.push_str(" ORDER BY due_at ASC");
    if filter.limit > 0 {
        sql.push_str(" LIMIT ?");
        params.push(Value::Integer(filter.limit as i64));
    }
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(params), row_to_reminder)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Pending reminders that are due at or before `now`, due date ascending.
/// `limit = 0` means unlimited.
pub fn due(conn: &Connection, now: Timestamp, limit: u32) -> StoreResult<Vec<Reminder>> {
    let mut sql = format!(
        "SELECT {SELECT_COLUMNS} FROM reminders WHERE status = 'pending' AND due_at <= ? ORDER BY due_at ASC"
    );
    let mut params: Vec<Value> = vec![Value::Integer(now.as_millis())];
    if limit > 0 {
        sql.push_str(" LIMIT ?");
        params.push(Value::Integer(limit as i64));
    }
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(params), row_to_reminder)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Change a reminder's status. `Fired` stamps `fired_at` and emits
/// [`UnifiedEvent::ReminderFired`]; any other change emits
/// [`UnifiedEvent::ReminderUpdated`]. No-op (no event) if the status is
/// unchanged.
pub fn set_status(
    tx: &WriteTx<'_>,
    id: ReminderId,
    status: ReminderStatus,
    origin: Origin,
) -> StoreResult<bool> {
    let revision = tx.revision().get() as i64;
    let n = if status == ReminderStatus::Fired {
        tx.execute(
            "UPDATE reminders SET status = ?, fired_at = ?, revision = ? WHERE id = ? AND status IS NOT ?",
            params![status.as_str(), tx.now().as_millis(), revision, id.get(), status.as_str()],
        )?
    } else {
        tx.execute(
            "UPDATE reminders SET status = ?, revision = ? WHERE id = ? AND status IS NOT ?",
            params![status.as_str(), revision, id.get(), status.as_str()],
        )?
    };
    let changed = n > 0;
    if changed {
        let event = if status == ReminderStatus::Fired {
            UnifiedEvent::ReminderFired { reminder_id: id }
        } else {
            UnifiedEvent::ReminderUpdated { reminder_id: id }
        };
        tx.emit(event, origin)?;
    }
    Ok(changed)
}

/// Move a pending reminder's due time (for `Conditional`, this reschedules
/// `check_at`; the condition itself is unchanged). No-op on a reminder that
/// is not pending.
pub fn reschedule(
    tx: &WriteTx<'_>,
    id: ReminderId,
    due_at: Timestamp,
    origin: Origin,
) -> StoreResult<bool> {
    let revision = tx.revision().get() as i64;
    let n = tx.execute(
        "UPDATE reminders SET due_at = ?, revision = ? \
         WHERE id = ? AND status = 'pending' AND due_at IS NOT ?",
        params![due_at.as_millis(), revision, id.get(), due_at.as_millis()],
    )?;
    let changed = n > 0;
    if changed {
        tx.emit(UnifiedEvent::ReminderUpdated { reminder_id: id }, origin)?;
    }
    Ok(changed)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::db::Database;
    use litecord_types::UserId;

    fn at_draft(title: &str, at: i64) -> ReminderDraft {
        ReminderDraft {
            title: title.to_string(),
            note: None,
            trigger: ReminderTrigger::At {
                at: Timestamp::from_millis(at),
            },
            conversation_id: None,
            task_id: None,
            source: None,
        }
    }

    #[test]
    fn at_and_conditional_roundtrip() {
        let db = Database::open_in_memory().unwrap();
        let id_at = db
            .write(|tx| create(tx, &at_draft("ping bob", 1_000), Origin::UserProvided))
            .unwrap()
            .value;
        let r = db.read(|r| get(r, id_at)).unwrap().unwrap();
        assert_eq!(
            r.trigger,
            ReminderTrigger::At {
                at: Timestamp::from_millis(1_000)
            }
        );

        let cond_draft = ReminderDraft {
            title: "check reply".to_string(),
            note: None,
            trigger: ReminderTrigger::Conditional {
                condition: ReminderCondition::NoReplyFrom {
                    user_id: UserId(1),
                    conversation_id: ConversationId(2),
                    since: Timestamp::from_millis(500),
                },
                check_at: Timestamp::from_millis(2_000),
            },
            conversation_id: None,
            task_id: None,
            source: None,
        };
        let id_cond = db
            .write(|tx| create(tx, &cond_draft, Origin::AgentDerived))
            .unwrap()
            .value;
        let r = db.read(|r| get(r, id_cond)).unwrap().unwrap();
        assert_eq!(r.trigger, cond_draft.trigger);
    }

    #[test]
    fn due_only_returns_pending_due_items_and_fired_emits_event() {
        let db = Database::open_in_memory().unwrap();
        let id1 = db
            .write(|tx| create(tx, &at_draft("a", 100), Origin::UserProvided))
            .unwrap()
            .value;
        let id2 = db
            .write(|tx| create(tx, &at_draft("b", 5_000), Origin::UserProvided))
            .unwrap()
            .value;

        let due_now = db.read(|r| due(r, Timestamp::from_millis(200), 0)).unwrap();
        assert_eq!(due_now.len(), 1);
        assert_eq!(due_now[0].id, id1);

        let committed = db
            .write(|tx| set_status(tx, id1, ReminderStatus::Fired, Origin::LocalApplication))
            .unwrap();
        assert!(committed.changed());
        assert!(matches!(
            committed.events[0],
            UnifiedEvent::ReminderFired { reminder_id } if reminder_id == id1
        ));
        let r = db.read(|r| get(r, id1)).unwrap().unwrap();
        assert!(r.fired_at.is_some());

        let due_now = db
            .read(|r| due(r, Timestamp::from_millis(10_000), 0))
            .unwrap();
        assert_eq!(due_now.len(), 1);
        assert_eq!(due_now[0].id, id2);
    }
}
