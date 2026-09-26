-- User-defined Omni automations (docs/ROADMAP.md §1). The trigger is JSON
-- interpreted by litecord-app (daily / every N hours / DM from / keyword).

CREATE TABLE omni_automations (
    id                    INTEGER PRIMARY KEY AUTOINCREMENT,
    name                  TEXT    NOT NULL,
    prompt                TEXT    NOT NULL,
    trigger               TEXT    NOT NULL,
    output                TEXT    NOT NULL DEFAULT 'inbox', -- inbox | tasks | drafts
    enabled               INTEGER NOT NULL DEFAULT 1,
    session_id            INTEGER REFERENCES omni_sessions(id) ON DELETE SET NULL,
    last_run_at           INTEGER,
    last_checked_revision INTEGER NOT NULL DEFAULT 0,
    runs                  INTEGER NOT NULL DEFAULT 0,
    last_error            TEXT,
    created_at            INTEGER NOT NULL
);
