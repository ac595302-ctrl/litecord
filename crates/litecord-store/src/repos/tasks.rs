//! Tasks: derived candidates that graduate into confirmed work items (V2
//! §22). A task is only ever *created* as `Candidate` or `Open`; every other
//! status is reached through [`set_status`].

use rusqlite::{params, types::Value, Connection, OptionalExtension, Row};

use litecord_core::events::UnifiedEvent;
use litecord_types::provenance::{Origin, SourceRef};
use litecord_types::tasks::{Task, TaskComment, TaskDraft, TaskPriority, TaskStatus};
use litecord_types::{ConversationId, TaskId, Timestamp, UserId};

use crate::db::WriteTx;
use crate::error::{StoreError, StoreResult};
use crate::repos::fts::{self, FtsMode};
use crate::sql::{col_err, push_in_str, rev, ts};

const SELECT_COLUMNS: &str = "id, title, description, status, priority, origin, source_entity, \
     source_note, conversation_id, parent_id, due_at, created_at, updated_at, completed_at, revision";

/// `CASE` expression ranking priority urgent-first, for `ORDER BY`.
const PRIORITY_RANK: &str =
    "CASE priority WHEN 'urgent' THEN 0 WHEN 'high' THEN 1 WHEN 'normal' THEN 2 WHEN 'low' THEN 3 ELSE 4 END";

/// Create a task. `status` must be [`TaskStatus::Candidate`] or
/// [`TaskStatus::Open`]; any other starting status is rejected. `title` is
/// trimmed and must not end up empty. If `draft.parent_id` is set, the parent
/// must exist and must not itself be a subtask (subtasks are only ever one
/// level deep) — either violation is a [`StoreError::Invariant`]. Emits
/// [`UnifiedEvent::TaskCreated`].
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
    if let Some(parent_id) = draft.parent_id {
        let parent = get(tx, parent_id)?
            .ok_or_else(|| StoreError::Invariant(format!("parent task {parent_id:?} not found")))?;
        if parent.parent_id.is_some() {
            return Err(StoreError::Invariant(
                "subtasks cannot themselves have subtasks (one level of nesting only)".into(),
            ));
        }
    }
    let (source_entity, source_note) = match &draft.source {
        Some(s) => (Some(s.entity.to_string()), s.note.clone()),
        None => (None, None),
    };
    let now = tx.now();
    let revision = tx.revision().get() as i64;
    tx.execute(
        "INSERT INTO tasks \
            (title, description, status, priority, origin, source_entity, source_note, \
             conversation_id, parent_id, due_at, created_at, updated_at, completed_at, revision) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, NULL, ?)",
        params![
            title,
            draft.description,
            status.as_str(),
            draft.priority.as_str(),
            origin.as_str(),
            source_entity,
            source_note,
            draft.conversation_id.map(|c| c.to_sql()),
            draft.parent_id.map(|p| p.get()),
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
    let priority: String = row.get(4)?;
    let priority = TaskPriority::parse(&priority).map_err(|e| col_err(4, e))?;
    let origin_s: String = row.get(5)?;
    let origin = crate::sql::origin(5, &origin_s)?;
    let source_entity: Option<String> = row.get(6)?;
    let source_note: Option<String> = row.get(7)?;
    let source = match source_entity {
        Some(e) => Some(SourceRef {
            entity: e.parse().map_err(|e| col_err(6, e))?,
            note: source_note,
        }),
        None => None,
    };
    let conversation_id: Option<i64> = row.get(8)?;
    let parent_id: Option<i64> = row.get(9)?;
    let due_at: Option<i64> = row.get(10)?;
    let created_at: i64 = row.get(11)?;
    let updated_at: i64 = row.get(12)?;
    let completed_at: Option<i64> = row.get(13)?;
    let revision: i64 = row.get(14)?;
    Ok(Task {
        id,
        title,
        description,
        status,
        priority,
        origin,
        source,
        related_users: Vec::new(),
        conversation_id: conversation_id.map(ConversationId::from_sql),
        parent_id: parent_id.map(TaskId),
        due_at: ts(due_at),
        created_at: Timestamp::from_millis(created_at),
        updated_at: Timestamp::from_millis(updated_at),
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
    /// `Some(None)` keeps only top-level tasks (no parent); `Some(Some(id))`
    /// keeps only subtasks of `id`; `None` applies no filter on parentage.
    pub parent_id: Option<Option<TaskId>>,
    /// Keep only tasks at or above this priority (e.g. `High` also matches
    /// `Urgent`).
    pub min_priority: Option<TaskPriority>,
    pub limit: u32,
}

/// List tasks matching `filter`: highest priority first, then tasks without
/// a due date last, then by due date ascending, then newest first.
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
    match filter.parent_id {
        Some(None) => conditions.push("t.parent_id IS NULL".to_string()),
        Some(Some(parent)) => {
            conditions.push("t.parent_id = ?".to_string());
            params.push(Value::Integer(parent.get()));
        }
        None => {}
    }
    if let Some(min_priority) = filter.min_priority {
        conditions.push(format!(
            "({}) <= ?",
            PRIORITY_RANK.replace("priority", "t.priority")
        ));
        params.push(Value::Integer(priority_rank(min_priority)));
    }
    if !conditions.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&conditions.join(" AND "));
    }
    sql.push_str(&format!(
        " ORDER BY {} ASC, t.due_at IS NULL, t.due_at ASC, t.created_at DESC",
        PRIORITY_RANK.replace("priority", "t.priority")
    ));
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

/// The numeric rank [`PRIORITY_RANK`] assigns a priority (lower = more
/// urgent), for building `min_priority` comparisons in Rust.
fn priority_rank(p: TaskPriority) -> i64 {
    match p {
        TaskPriority::Urgent => 0,
        TaskPriority::High => 1,
        TaskPriority::Normal => 2,
        TaskPriority::Low => 3,
    }
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

/// Change a task's priority. Emits [`UnifiedEvent::TaskUpdated`] when the
/// priority actually changes. Returns `false` (no event) if the task already
/// has this priority or does not exist.
pub fn set_priority(
    tx: &WriteTx<'_>,
    id: TaskId,
    priority: TaskPriority,
    origin: Origin,
) -> StoreResult<bool> {
    let now = tx.now();
    let revision = tx.revision().get() as i64;
    let n = tx.execute(
        "UPDATE tasks SET priority = ?, updated_at = ?, revision = ? \
         WHERE id = ? AND priority IS NOT ?",
        params![
            priority.as_str(),
            now.as_millis(),
            revision,
            id.get(),
            priority.as_str()
        ],
    )?;
    let changed = n > 0;
    if changed {
        tx.emit(UnifiedEvent::TaskUpdated { task_id: id }, origin)?;
    }
    Ok(changed)
}

/// Direct subtasks of `parent`, in the same order as [`list`] (priority
/// first, then due date, then newest first).
pub fn subtasks(conn: &Connection, parent: TaskId) -> StoreResult<Vec<Task>> {
    list(
        conn,
        &TaskFilter {
            parent_id: Some(Some(parent)),
            ..Default::default()
        },
    )
}

/// How many of `id`'s subtasks are still open (`Open` or `Candidate`).
/// Completing a parent task does **not** auto-complete its subtasks, so
/// callers use this to warn about (or simply display) outstanding work.
pub fn open_subtask_count(conn: &Connection, id: TaskId) -> StoreResult<i64> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM tasks WHERE parent_id = ? AND status IN ('open', 'candidate')",
        params![id.get()],
        |r| r.get(0),
    )?;
    Ok(n)
}

/// Add a comment to a task. `body` is trimmed and must not end up empty.
/// Emits [`UnifiedEvent::TaskUpdated`] for the parent task.
pub fn add_comment(
    tx: &WriteTx<'_>,
    task_id: TaskId,
    body: &str,
    origin: Origin,
) -> StoreResult<i64> {
    let body = body.trim();
    if body.is_empty() {
        return Err(StoreError::Invariant(
            "comment body must not be empty".into(),
        ));
    }
    if get(tx, task_id)?.is_none() {
        return Err(StoreError::Invariant(format!("task {task_id:?} not found")));
    }
    let now = tx.now();
    let revision = tx.revision().get() as i64;
    tx.execute(
        "INSERT INTO task_comments (task_id, body, origin, created_at, revision) \
         VALUES (?, ?, ?, ?, ?)",
        params![
            task_id.get(),
            body,
            origin.as_str(),
            now.as_millis(),
            revision
        ],
    )?;
    let id = tx.last_insert_rowid();
    tx.emit(UnifiedEvent::TaskUpdated { task_id }, origin)?;
    Ok(id)
}

/// A task's comments, oldest first.
pub fn comments(conn: &Connection, task_id: TaskId) -> StoreResult<Vec<TaskComment>> {
    let mut stmt = conn.prepare(
        "SELECT id, task_id, body, origin, created_at FROM task_comments \
         WHERE task_id = ? ORDER BY created_at ASC, id ASC",
    )?;
    let rows = stmt.query_map(params![task_id.get()], |row| {
        let id: i64 = row.get(0)?;
        let task_id: i64 = row.get(1)?;
        let body: String = row.get(2)?;
        let origin_s: String = row.get(3)?;
        let origin = crate::sql::origin(3, &origin_s)?;
        let created_at: i64 = row.get(4)?;
        Ok(TaskComment {
            id,
            task_id: TaskId(task_id),
            body,
            origin,
            created_at: Timestamp::from_millis(created_at),
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
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
        let bm25: f64 = row.get(15)?;
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
            priority: TaskPriority::Normal,
            due_at: None,
            related_users: Vec::new(),
            conversation_id: None,
            parent_id: None,
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

    #[test]
    fn list_orders_urgent_first() {
        let db = Database::open_in_memory().unwrap();
        let mk = |title: &str, priority: TaskPriority| {
            let mut d = draft(title);
            d.priority = priority;
            db.write(|tx| create(tx, &d, TaskStatus::Open, Origin::UserProvided))
                .unwrap()
                .value
        };
        mk("low prio", TaskPriority::Low);
        mk("urgent prio", TaskPriority::Urgent);
        mk("normal prio", TaskPriority::Normal);
        mk("high prio", TaskPriority::High);

        let tasks = db.read(|r| list(r, &TaskFilter::default())).unwrap();
        let titles: Vec<&str> = tasks.iter().map(|t| t.title.as_str()).collect();
        assert_eq!(
            titles,
            vec!["urgent prio", "high prio", "normal prio", "low prio"]
        );
    }

    #[test]
    fn min_priority_filters_out_lower_priority_tasks() {
        let db = Database::open_in_memory().unwrap();
        let mk = |title: &str, priority: TaskPriority| {
            let mut d = draft(title);
            d.priority = priority;
            db.write(|tx| create(tx, &d, TaskStatus::Open, Origin::UserProvided))
                .unwrap();
        };
        mk("low prio", TaskPriority::Low);
        mk("urgent prio", TaskPriority::Urgent);
        mk("high prio", TaskPriority::High);

        let tasks = db
            .read(|r| {
                list(
                    r,
                    &TaskFilter {
                        min_priority: Some(TaskPriority::High),
                        ..Default::default()
                    },
                )
            })
            .unwrap();
        assert_eq!(tasks.len(), 2);
        assert!(tasks.iter().all(|t| t.priority != TaskPriority::Low));
    }

    #[test]
    fn set_priority_updates_and_emits() {
        let db = Database::open_in_memory().unwrap();
        let id = db
            .write(|tx| create(tx, &draft("x"), TaskStatus::Open, Origin::UserProvided))
            .unwrap()
            .value;
        let changed = db
            .write(|tx| set_priority(tx, id, TaskPriority::Urgent, Origin::UserProvided))
            .unwrap()
            .value;
        assert!(changed);
        let t = db.read(|r| get(r, id)).unwrap().unwrap();
        assert_eq!(t.priority, TaskPriority::Urgent);

        // Setting the same priority again is a no-op.
        let changed_again = db
            .write(|tx| set_priority(tx, id, TaskPriority::Urgent, Origin::UserProvided))
            .unwrap()
            .value;
        assert!(!changed_again);
    }

    #[test]
    fn subtasks_are_limited_to_one_level() {
        let db = Database::open_in_memory().unwrap();
        let parent = db
            .write(|tx| create(tx, &draft("parent"), TaskStatus::Open, Origin::UserProvided))
            .unwrap()
            .value;
        let mut sub_draft = draft("sub");
        sub_draft.parent_id = Some(parent);
        let sub = db
            .write(|tx| create(tx, &sub_draft, TaskStatus::Open, Origin::UserProvided))
            .unwrap()
            .value;

        let mut grandchild_draft = draft("grandchild");
        grandchild_draft.parent_id = Some(sub);
        let err = db
            .write(|tx| {
                create(
                    tx,
                    &grandchild_draft,
                    TaskStatus::Open,
                    Origin::UserProvided,
                )
            })
            .unwrap_err();
        assert!(matches!(err, StoreError::Invariant(_)));

        let mut missing_parent_draft = draft("orphan");
        missing_parent_draft.parent_id = Some(TaskId(99999));
        let err = db
            .write(|tx| {
                create(
                    tx,
                    &missing_parent_draft,
                    TaskStatus::Open,
                    Origin::UserProvided,
                )
            })
            .unwrap_err();
        assert!(matches!(err, StoreError::Invariant(_)));

        let subs = db.read(|r| subtasks(r, parent)).unwrap();
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].id, sub);

        let open_count = db.read(|r| open_subtask_count(r, parent)).unwrap();
        assert_eq!(open_count, 1);
        db.write(|tx| set_status(tx, sub, TaskStatus::Done, Origin::UserProvided))
            .unwrap();
        let open_count = db.read(|r| open_subtask_count(r, parent)).unwrap();
        assert_eq!(open_count, 0);
    }

    #[test]
    fn completing_parent_does_not_complete_subtasks() {
        let db = Database::open_in_memory().unwrap();
        let parent = db
            .write(|tx| create(tx, &draft("parent"), TaskStatus::Open, Origin::UserProvided))
            .unwrap()
            .value;
        let mut sub_draft = draft("sub");
        sub_draft.parent_id = Some(parent);
        let sub = db
            .write(|tx| create(tx, &sub_draft, TaskStatus::Open, Origin::UserProvided))
            .unwrap()
            .value;

        db.write(|tx| set_status(tx, parent, TaskStatus::Done, Origin::UserProvided))
            .unwrap();
        let sub = db.read(|r| get(r, sub)).unwrap().unwrap();
        assert_eq!(sub.status, TaskStatus::Open);
    }

    #[test]
    fn filter_top_level_only() {
        let db = Database::open_in_memory().unwrap();
        let parent = db
            .write(|tx| create(tx, &draft("parent"), TaskStatus::Open, Origin::UserProvided))
            .unwrap()
            .value;
        let mut sub_draft = draft("sub");
        sub_draft.parent_id = Some(parent);
        db.write(|tx| create(tx, &sub_draft, TaskStatus::Open, Origin::UserProvided))
            .unwrap();

        let top_level = db
            .read(|r| {
                list(
                    r,
                    &TaskFilter {
                        parent_id: Some(None),
                        ..Default::default()
                    },
                )
            })
            .unwrap();
        assert_eq!(top_level.len(), 1);
        assert_eq!(top_level[0].id, parent);
    }

    #[test]
    fn comments_round_trip_oldest_first_and_cascade_delete() {
        let db = Database::open_in_memory().unwrap();
        let id = db
            .write(|tx| create(tx, &draft("x"), TaskStatus::Open, Origin::UserProvided))
            .unwrap()
            .value;
        db.write(|tx| add_comment(tx, id, "first", Origin::UserProvided))
            .unwrap();
        db.write(|tx| add_comment(tx, id, "second", Origin::AgentDerived))
            .unwrap();

        let all = db.read(|r| comments(r, id)).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].body, "first");
        assert_eq!(all[1].body, "second");
        assert_eq!(all[1].origin, Origin::AgentDerived);

        let err = db
            .write(|tx| add_comment(tx, id, "   ", Origin::UserProvided))
            .unwrap_err();
        assert!(matches!(err, StoreError::Invariant(_)));

        db.write(|tx| -> StoreResult<()> {
            tx.execute("DELETE FROM tasks WHERE id = ?", params![id.get()])?;
            Ok(())
        })
        .unwrap();
        let after_delete = db.read(|r| comments(r, id)).unwrap();
        assert!(after_delete.is_empty());
    }
}
