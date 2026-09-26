//! Tasks: derived candidates that graduate into confirmed work items (V2
//! §22). A task is only ever *created* as `Candidate` or `Open`; every other
//! status is reached through [`set_status`].

use rusqlite::{params, types::Value, Connection, OptionalExtension, Row};

use litecord_core::events::UnifiedEvent;
use litecord_types::provenance::{Origin, SourceRef};
use litecord_types::tasks::{Task, TaskDraft, TaskStatus};
use litecord_types::{ConversationId, TaskId, Timestamp, UserId};

use crate::db::WriteTx;
use crate::error::{StoreError, StoreResult};
use crate::repos::fts::{self, FtsMode};
use crate::sql::{col_err, push_in_str, rev, ts};

const SELECT_COLUMNS: &str = "id, title, description, status, origin, source_entity, source_note, \
     conversation_id, due_at, created_at, completed_at, revision";

/// Create a task. `status` must be [`TaskStatus::Candidate`] or
/// [`TaskStatus::Open`]; any other starting status is rejected. `title` is
/// trimmed and must not end up empty. Emits [`UnifiedEvent::TaskCreated`].
pub fn create(
    tx: &WriteTx<'_>,
    draft: &TaskDraft,
    status: TaskStatus,
    origin: Origin,
) -> StoreResult<TaskId> {
    if !matches!(status, TaskStatus::Candidate | TaskStatus::Open) {
        return Err(StoreError::Invariant(
            "tasks may only be created as Candidate or Open".into(),
        ));
    }
    let title = draft.title.trim();
    if title.is_empty() {
        return Err(StoreError::Invariant("task title must not be empty".into()));
    }
    let (source_entity, source_note) = match &draft.source {
        Some(s) => (Some(s.entity.to_string()), s.note.clone()),
        None => (None, None),
    };
    let now = tx.now();
    let revision = tx.revision().get() as i64;
    tx.execute(
        "INSERT INTO tasks \
            (title, description, status, origin, source_entity, source_note, conversation_id, \
             due_at, created_at, updated_at, completed_at, revision) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, NULL, ?)",
        params![
            title,
            draft.description,
            status.as_str(),
            origin.as_str(),
            source_entity,
            source_note,
            draft.conversation_id.map(|c| c.to_sql()),
            draft.due_at.map(Timestamp::as_millis),
            now.as_millis(),
            now.as_millis(),
            revision,
        ],
    )?;
    let id = TaskId(tx.last_insert_rowid());
    for user in &draft.related_users {
        tx.execute(
            "INSERT OR IGNORE INTO task_users (task_id, user_id) VALUES (?, ?)",
            params![id.get(), user.to_sql()],
        )?;
    }
    tx.emit(UnifiedEvent::TaskCreated { task_id: id }, origin)?;
    Ok(id)
}

fn row_to_task(row: &Row<'_>) -> rusqlite::Result<Task> {
    let id = TaskId(row.get(0)?);
    let title: String = row.get(1)?;
    let description: Option<String> = row.get(2)?;
    let status: String = row.get(3)?;
    let status = TaskStatus::parse(&status).map_err(|e| col_err(3, e))?;
    let origin_s: String = row.get(4)?;
    let origin = crate::sql::origin(4, &origin_s)?;
    let source_entity: Option<String> = row.get(5)?;
    let source_note: Option<String> = row.get(6)?;
    let source = match source_entity {
        Some(e) => Some(SourceRef {
            entity: e.parse().map_err(|e| col_err(5, e))?,
            note: source_note,
        }),
        None => None,
    };
    let conversation_id: Option<i64> = row.get(7)?;
    let due_at: Option<i64> = row.get(8)?;
    let created_at: i64 = row.get(9)?;
    let completed_at: Option<i64> = row.get(10)?;
    let revision: i64 = row.get(11)?;
    Ok(Task {
        id,
        title,
        description,
        status,
        origin,
        source,
        related_users: Vec::new(),
        conversation_id: conversation_id.map(ConversationId::from_sql),
        due_at: ts(due_at),
        created_at: Timestamp::from_millis(created_at),
        completed_at: ts(completed_at),
        revision: rev(revision),
    })
}

fn load_related_users(conn: &Connection, id: TaskId) -> StoreResult<Vec<UserId>> {
    let mut stmt =
        conn.prepare("SELECT user_id FROM task_users WHERE task_id = ? ORDER BY user_id")?;
    let rows = stmt.query_map(params![id.get()], |r| Ok(UserId::from_sql(r.get(0)?)))?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

fn hydrate(conn: &Connection, mut task: Task) -> StoreResult<Task> {
    task.related_users = load_related_users(conn, task.id)?;
    Ok(task)
}

/// Load a task by id, including its related users.
pub fn get(conn: &Connection, id: TaskId) -> StoreResult<Option<Task>> {
    let sql = format!("SELECT {SELECT_COLUMNS} FROM tasks WHERE id = ?");
    let base = conn
        .query_row(&sql, params![id.get()], row_to_task)
        .optional()?;
    base.map(|t| hydrate(conn, t)).transpose()
}

/// Filter for [`list`]. `limit = 0` means unlimited.
#[derive(Debug, Clone, Default)]
pub struct TaskFilter {
    pub statuses: Option<Vec<TaskStatus>>,
    pub related_user: Option<UserId>,
    pub conversation_id: Option<ConversationId>,
    pub due_before: Option<Timestamp>,
    pub limit: u32,
}

/// List tasks matching `filter`: tasks without a due date last, then by due
/// date ascending, then newest first.
pub fn list(conn: &Connection, filter: &TaskFilter) -> StoreResult<Vec<Task>> {
    let mut sql = format!(
        "SELECT DISTINCT {} FROM tasks t",
        qualify(SELECT_COLUMNS, "t")
    );
    let mut conditions = Vec::new();
    let mut params: Vec<Value> = Vec::new();

    if filter.related_user.is_some() {
        sql.push_str(" JOIN task_users tu ON tu.task_id = t.id");
    }
    if let Some(statuses) = &filter.statuses {
        push_in_str(
            &mut conditions,
            &mut params,
            "t.status",
            statuses.iter().map(|s| s.as_str()),
        );
    }
    if let Some(user) = filter.related_user {
        conditions.push("tu.user_id = ?".to_string());
        params.push(Value::Integer(user.to_sql()));
    }
    if let Some(conv) = filter.conversation_id {
        conditions.push("t.conversation_id = ?".to_string());
        params.push(Value::Integer(conv.to_sql()));
    }
    if let Some(before) = filter.due_before {
        conditions.push("t.due_at IS NOT NULL AND t.due_at < ?".to_string());
        params.push(Value::Integer(before.as_millis()));
    }
    if !conditions.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&conditions.join(" AND "));
    }
    sql.push_str(" ORDER BY t.due_at IS NULL, t.due_at ASC, t.created_at DESC");
    if filter.limit > 0 {
        sql.push_str(" LIMIT ?");
        params.push(Value::Integer(filter.limit as i64));
    }

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(params), row_to_task)?;
    rows.collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|t| hydrate(conn, t))
        .collect()
}

fn qualify(columns: &str, alias: &str) -> String {
    columns
        .split(", ")
        .map(|c| format!("{alias}.{c}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Change a task's status. `Done` stamps `completed_at`; leaving `Done`
/// clears it. Emits [`UnifiedEvent::TaskUpdated`] when the status actually
/// changes.
pub fn set_status(
    tx: &WriteTx<'_>,
    id: TaskId,
    status: TaskStatus,
    origin: Origin,
) -> StoreResult<bool> {
    let now = tx.now();
    let revision = tx.revision().get() as i64;
    let n = if status == TaskStatus::Done {
        tx.execute(
            "UPDATE tasks SET status = ?, completed_at = ?, updated_at = ?, revision = ? \
             WHERE id = ? AND status IS NOT ?",
            params![
                status.as_str(),
                now.as_millis(),
                now.as_millis(),
                revision,
                id.get(),
                status.as_str()
            ],
        )?
    } else {
        tx.execute(
            "UPDATE tasks SET status = ?, completed_at = NULL, updated_at = ?, revision = ? \
             WHERE id = ? AND status IS NOT ?",
            params![
                status.as_str(),
                now.as_millis(),
                revision,
                id.get(),
                status.as_str()
            ],
        )?
    };
    let changed = n > 0;
    if changed {
        tx.emit(UnifiedEvent::TaskUpdated { task_id: id }, origin)?;
    }
    Ok(changed)
}

/// Update a task's title/description/due date. Each parameter is
/// `Option<..>` in the "update this field?" sense, and `description`/
/// `due_at` are themselves `Option<Option<..>>` so `Some(None)` clears the
/// field. A `Some` title is trimmed and must not be empty. Returns `false`
/// (no-op, no event) if nothing was asked to change.
pub fn update(
    tx: &WriteTx<'_>,
    id: TaskId,
    title: Option<&str>,
    description: Option<Option<&str>>,
    due_at: Option<Option<Timestamp>>,
    origin: Origin,
) -> StoreResult<bool> {
    let mut sets = Vec::new();
    let mut params: Vec<Value> = Vec::new();

    if let Some(t) = title {
        let t = t.trim();
        if t.is_empty() {
            return Err(StoreError::Invariant("task title must not be empty".into()));
        }
        sets.push("title = ?");
        params.push(Value::Text(t.to_string()));
    }
    if let Some(d) = description {
        sets.push("description = ?");
        params.push(match d {
            Some(s) => Value::Text(s.to_string()),
            None => Value::Null,
        });
    }
    if let Some(d) = due_at {
        sets.push("due_at = ?");
        params.push(match d {
            Some(t) => Value::Integer(t.as_millis()),
            None => Value::Null,
        });
    }
    if sets.is_empty() {
        return Ok(false);
    }
    sets.push("updated_at = ?");
    params.push(Value::Integer(tx.now().as_millis()));
    sets.push("revision = ?");
    params.push(Value::Integer(tx.revision().get() as i64));

    let sql = format!("UPDATE tasks SET {} WHERE id = ?", sets.join(", "));
    params.push(Value::Integer(id.get()));
    let n = tx.execute(&sql, rusqlite::params_from_iter(params))?;
    let changed = n > 0;
    if changed {
        tx.emit(UnifiedEvent::TaskUpdated { task_id: id }, origin)?;
    }
    Ok(changed)
}

/// Full-text search over task title/description, best match first.
pub fn search(
    conn: &Connection,
    query: &str,
    statuses: Option<&[TaskStatus]>,
    limit: u32,
) -> StoreResult<Vec<(Task, f64)>> {
    let Some(expr) = fts::match_expr(query, FtsMode::Any, false) else {
        return Ok(Vec::new());
    };
    let mut sql = format!(
        "SELECT {}, bm25(tasks_fts) FROM tasks_fts JOIN tasks t ON t.id = tasks_fts.rowid \
         WHERE tasks_fts MATCH ?",
        qualify(SELECT_COLUMNS, "t")
    );
    let mut conditions = Vec::new();
    let mut params: Vec<Value> = vec![Value::Text(expr)];
    if let Some(statuses) = statuses {
        push_in_str(
            &mut conditions,
            &mut params,
            "t.status",
            statuses.iter().map(|s| s.as_str()),
        );
    }
    for c in conditions {
        sql.push_str(" AND ");
        sql.push_str(&c);
    }
    sql.push_str(" ORDER BY bm25(tasks_fts) ASC LIMIT ?");
    let limit = if limit == 0 { 50 } else { limit };
    params.push(Value::Integer(limit as i64));

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(params), |row| {
        let task = row_to_task(row)?;
        let bm25: f64 = row.get(12)?;
        Ok((task, bm25))
    })?;
    rows.collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|(t, score)| Ok((hydrate(conn, t)?, score)))
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::db::Database;

    fn draft(title: &str) -> TaskDraft {
        TaskDraft {
            title: title.to_string(),
            description: None,
            due_at: None,
            related_users: Vec::new(),
            conversation_id: None,
            source: None,
        }
    }

    #[test]
    fn create_confirm_and_complete() {
        let db = Database::open_in_memory().unwrap();
        let id = db
            .write(|tx| {
                create(
                    tx,
                    &draft("write report"),
                    TaskStatus::Candidate,
                    Origin::AgentDerived,
                )
            })
            .unwrap()
            .value;
        let t = db.read(|r| get(r, id)).unwrap().unwrap();
        assert_eq!(t.status, TaskStatus::Candidate);

        let changed = db
            .write(|tx| set_status(tx, id, TaskStatus::Open, Origin::UserProvided))
            .unwrap()
            .value;
        assert!(changed);

        db.write(|tx| set_status(tx, id, TaskStatus::Done, Origin::UserProvided))
            .unwrap();
        let t = db.read(|r| get(r, id)).unwrap().unwrap();
        assert_eq!(t.status, TaskStatus::Done);
        assert!(t.completed_at.is_some());

        db.write(|tx| set_status(tx, id, TaskStatus::Open, Origin::UserProvided))
            .unwrap();
        let t = db.read(|r| get(r, id)).unwrap().unwrap();
        assert!(t.completed_at.is_none());
    }

    #[test]
    fn creation_rejects_non_initial_statuses() {
        let db = Database::open_in_memory().unwrap();
        let err = db
            .write(|tx| create(tx, &draft("x"), TaskStatus::Done, Origin::AgentDerived))
            .unwrap_err();
        assert!(matches!(err, StoreError::Invariant(_)));
    }

    #[test]
    fn search_finds_by_title() {
        let db = Database::open_in_memory().unwrap();
        db.write(|tx| {
            create(
                tx,
                &draft("buy groceries"),
                TaskStatus::Open,
                Origin::UserProvided,
            )
        })
        .unwrap();
        db.write(|tx| {
            create(
                tx,
                &draft("call dentist"),
                TaskStatus::Open,
                Origin::UserProvided,
            )
        })
        .unwrap();
        let hits = db.read(|r| search(r, "groceries", None, 10)).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0.title, "buy groceries");
    }
}
