-- Task priorities, subtasks (one level deep) and comments.
--
-- `priority` defaults every existing row to 'normal' so a v1 database
-- upgrades without data loss. `parent_id` links a subtask to its parent
-- task; the store layer rejects nesting deeper than one level, so this
-- column never chains more than once in practice, but the FK still cascades
-- deletes for safety.

ALTER TABLE tasks ADD COLUMN priority TEXT NOT NULL DEFAULT 'normal';
ALTER TABLE tasks ADD COLUMN parent_id INTEGER REFERENCES tasks (id) ON DELETE CASCADE;

CREATE INDEX tasks_parent ON tasks (parent_id);
CREATE INDEX tasks_status_priority ON tasks (status, priority);

CREATE TABLE task_comments (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    task_id    INTEGER NOT NULL REFERENCES tasks (id) ON DELETE CASCADE,
    body       TEXT    NOT NULL,
    origin     TEXT    NOT NULL,
    created_at INTEGER NOT NULL,
    revision   INTEGER NOT NULL
);
CREATE INDEX task_comments_task_time ON task_comments (task_id, created_at);
