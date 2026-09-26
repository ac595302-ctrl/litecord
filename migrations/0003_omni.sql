-- Omni sessions on an external harness (docs/AGENT_HARNESS.md §7, §11).
-- Transcripts hold completed items only (streaming deltas are never
-- stored) and are capped by the app's retention.

CREATE TABLE omni_sessions (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    harness        TEXT    NOT NULL,              -- codex | opencode | fake
    external_id    TEXT,                          -- thread/session id inside the harness
    kind           TEXT    NOT NULL DEFAULT 'chat', -- chat | heartbeat
    mode           TEXT    NOT NULL DEFAULT 'assistant',
    profile        TEXT,
    title          TEXT    NOT NULL,
    parent_id      INTEGER REFERENCES omni_sessions(id) ON DELETE SET NULL,
    status         TEXT    NOT NULL DEFAULT 'active', -- active | archived
    tokens_in      INTEGER NOT NULL DEFAULT 0,
    tokens_out     INTEGER NOT NULL DEFAULT 0,
    turns          INTEGER NOT NULL DEFAULT 0,
    created_at     INTEGER NOT NULL,
    last_active_at INTEGER NOT NULL
);
CREATE INDEX omni_sessions_recent ON omni_sessions (status, last_active_at DESC);

CREATE TABLE omni_items (
    session_id INTEGER NOT NULL REFERENCES omni_sessions(id) ON DELETE CASCADE,
    seq        INTEGER NOT NULL,
    role       TEXT    NOT NULL,   -- user | omni | system
    kind       TEXT    NOT NULL,   -- message | tool_call | command | ... (litecord_harness::ItemKind)
    text       TEXT    NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (session_id, seq)
);
