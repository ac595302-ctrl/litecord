-- Stage C: paged, resumable history backfill.
--
-- One row per conversation the user selected for history sync. The cursor
-- (`oldest_message_id`) is advanced by the reducer in the same transaction
-- that stores each `MessagesPage`, so a page and its checkpoint commit (or
-- roll back) together. `complete` is set once a page comes back empty.
CREATE TABLE history_sync (
    conversation_id   INTEGER PRIMARY KEY,
    enabled           INTEGER NOT NULL DEFAULT 1,
    oldest_message_id INTEGER,
    complete          INTEGER NOT NULL DEFAULT 0,
    pages             INTEGER NOT NULL DEFAULT 0,
    messages          INTEGER NOT NULL DEFAULT 0,
    last_error        TEXT,
    paused_reason     TEXT,
    requested_at      INTEGER NOT NULL,
    updated_at        INTEGER NOT NULL
);
