-- Litecord initial schema.
--
-- Layout follows the V2 unified-memory layers:
--   * canonical social state (users .. voice_state)  -- observed Discord data only
--   * event log (events)                              -- append-only, ids only
--   * derived memory (memory_*, summaries, embeddings)
--   * operational memory (tasks, reminders, drafts, notes, bookmarks, settings)
--   * action engine (action_*) and agent runs
--   * hydration bookkeeping (sync_state, hydration_jobs)
--
-- Conventions:
--   * Discord snowflakes are stored as INTEGER (bit-cast u64 -> i64).
--   * Times are INTEGER milliseconds since the Unix epoch (UTC).
--   * `origin` is the provenance string from litecord_types::provenance::Origin.
--   * `revision` is the global revision of the transaction that last wrote the row.
--   * Enumerations are stored as their stable snake_case strings.

CREATE TABLE revision_counter (
    id       INTEGER PRIMARY KEY CHECK (id = 1),
    revision INTEGER NOT NULL
);
INSERT INTO revision_counter (id, revision) VALUES (1, 0);

-- Small non-user-facing key/value state (e.g. last session state).
CREATE TABLE app_state (
    key        TEXT PRIMARY KEY,
    value      TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);

------------------------------------------------------------------------------
-- Canonical social state
------------------------------------------------------------------------------

CREATE TABLE accounts (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id      INTEGER NOT NULL,
    identity     TEXT    NOT NULL,          -- user_social_sdk | application_bot
    origin       TEXT    NOT NULL,
    is_current   INTEGER NOT NULL DEFAULT 0,
    created_at   INTEGER NOT NULL,
    last_seen_at INTEGER NOT NULL,
    UNIQUE (user_id, identity)
);

CREATE TABLE users (
    id                  INTEGER PRIMARY KEY,
    username            TEXT    NOT NULL,
    global_name         TEXT,
    avatar_url          TEXT,
    is_bot              INTEGER NOT NULL DEFAULT 0,
    is_provisional      INTEGER NOT NULL DEFAULT 0,
    -- 1 when the row is a placeholder created from a reference (e.g. a message
    -- author) before the profile was hydrated.
    is_stub             INTEGER NOT NULL DEFAULT 0,
    presence_status     TEXT    NOT NULL DEFAULT 'unknown',
    activity_name       TEXT,
    activity_details    TEXT,
    activity_state      TEXT,
    presence_updated_at INTEGER,
    origin              TEXT    NOT NULL,
    observed_at         INTEGER NOT NULL,
    revision            INTEGER NOT NULL
);

CREATE TABLE relationships (
    user_id     INTEGER PRIMARY KEY,
    discord_kind TEXT   NOT NULL,
    game_kind   TEXT    NOT NULL,
    since       INTEGER,
    origin      TEXT    NOT NULL,
    observed_at INTEGER NOT NULL,
    revision    INTEGER NOT NULL
);
CREATE INDEX relationships_kind ON relationships (discord_kind);

CREATE TABLE guilds (
    id          INTEGER PRIMARY KEY,
    name        TEXT    NOT NULL,
    icon_url    TEXT,
    departed    INTEGER NOT NULL DEFAULT 0,
    origin      TEXT    NOT NULL,
    observed_at INTEGER NOT NULL,
    revision    INTEGER NOT NULL
);

CREATE TABLE channels (
    id           INTEGER PRIMARY KEY,
    guild_id     INTEGER NOT NULL REFERENCES guilds (id) ON DELETE CASCADE,
    name         TEXT    NOT NULL,
    kind         TEXT    NOT NULL,
    position     INTEGER NOT NULL DEFAULT 0,
    parent_id    INTEGER,
    access       TEXT    NOT NULL,
    capabilities INTEGER NOT NULL DEFAULT 0,
    removed      INTEGER NOT NULL DEFAULT 0,
    origin       TEXT    NOT NULL,
    observed_at  INTEGER NOT NULL,
    revision     INTEGER NOT NULL
);
CREATE INDEX channels_guild ON channels (guild_id, position);

CREATE TABLE conversations (
    id               INTEGER PRIMARY KEY,
    kind             TEXT    NOT NULL,
    recipient_id     INTEGER,
    guild_id         INTEGER,
    lobby_id         INTEGER,
    title            TEXT,
    last_message_id  INTEGER,
    last_activity_at INTEGER,
    -- NULL means "use the configured default".
    agent_visibility TEXT CHECK (agent_visibility IN ('allowed', 'metadata_only', 'hidden')),
    origin           TEXT    NOT NULL,
    observed_at      INTEGER NOT NULL,
    revision         INTEGER NOT NULL
);
CREATE INDEX conversations_activity ON conversations (last_activity_at DESC);
CREATE INDEX conversations_recipient ON conversations (recipient_id);

CREATE TABLE messages (
    id              INTEGER PRIMARY KEY,
    conversation_id INTEGER NOT NULL REFERENCES conversations (id) ON DELETE CASCADE,
    author_id       INTEGER NOT NULL,
    content         TEXT    NOT NULL,
    sent_at         INTEGER NOT NULL,
    edited_at       INTEGER,
    reply_to        INTEGER,
    -- Small JSON array of MessageExtra (attachments/embeds metadata). Not queried.
    extras_json     TEXT,
    deleted         INTEGER NOT NULL DEFAULT 0,
    origin          TEXT    NOT NULL,
    observed_at     INTEGER NOT NULL,
    revision        INTEGER NOT NULL
);
CREATE INDEX messages_conversation_time ON messages (conversation_id, sent_at DESC);
CREATE INDEX messages_author_time ON messages (author_id, sent_at DESC);

CREATE TABLE lobbies (
    id                INTEGER PRIMARY KEY,
    linked_channel_id INTEGER,
    origin            TEXT    NOT NULL,
    observed_at       INTEGER NOT NULL,
    revision          INTEGER NOT NULL
);

CREATE TABLE lobby_members (
    lobby_id INTEGER NOT NULL REFERENCES lobbies (id) ON DELETE CASCADE,
    user_id  INTEGER NOT NULL,
    PRIMARY KEY (lobby_id, user_id)
);

-- Single-row local voice state (ephemeral but restored for the UI).
CREATE TABLE voice_state (
    id                INTEGER PRIMARY KEY CHECK (id = 1),
    connected         INTEGER NOT NULL,
    lobby_id          INTEGER,
    muted             INTEGER NOT NULL,
    deafened          INTEGER NOT NULL,
    input_device      TEXT,
    output_device     TEXT,
    output_volume     REAL    NOT NULL,
    noise_suppression INTEGER NOT NULL,
    push_to_talk      INTEGER NOT NULL,
    participants_json TEXT,
    observed_at       INTEGER NOT NULL,
    revision          INTEGER NOT NULL
);

------------------------------------------------------------------------------
-- Event log (append-only; compact UnifiedEvent JSON, ids only)
------------------------------------------------------------------------------

CREATE TABLE events (
    seq      INTEGER PRIMARY KEY AUTOINCREMENT,
    revision INTEGER NOT NULL,
    kind     TEXT    NOT NULL,
    entity   TEXT,
    source   TEXT    NOT NULL,
    payload  TEXT    NOT NULL,
    at       INTEGER NOT NULL
);
CREATE INDEX events_revision ON events (revision);
CREATE INDEX events_entity ON events (entity, revision);

------------------------------------------------------------------------------
-- Derived memory
------------------------------------------------------------------------------

CREATE TABLE local_entities (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    kind       TEXT    NOT NULL,
    name       TEXT    NOT NULL,
    origin     TEXT    NOT NULL,
    created_at INTEGER NOT NULL,
    UNIQUE (kind, name)
);

CREATE TABLE memory_items (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    kind              TEXT    NOT NULL,
    content           TEXT    NOT NULL,
    payload_json      TEXT,
    origin            TEXT    NOT NULL,
    status            TEXT    NOT NULL,
    confidence        REAL    NOT NULL CHECK (confidence >= 0.0 AND confidence <= 1.0),
    created_at        INTEGER NOT NULL,
    observed_at       INTEGER,
    expires_at        INTEGER,
    revision          INTEGER NOT NULL,
    superseded_by     INTEGER REFERENCES memory_items (id),
    fingerprint       TEXT,
    importance        REAL    NOT NULL DEFAULT 0.5,
    pinned            INTEGER NOT NULL DEFAULT 0,
    last_retrieved_at INTEGER,
    retrieval_count   INTEGER NOT NULL DEFAULT 0,
    -- A superseded item must point at its replacement.
    CHECK (status != 'superseded' OR superseded_by IS NOT NULL)
);
CREATE INDEX memory_status_kind ON memory_items (status, kind, created_at DESC);
CREATE INDEX memory_fingerprint ON memory_items (fingerprint) WHERE fingerprint IS NOT NULL;
CREATE INDEX memory_expires ON memory_items (expires_at) WHERE expires_at IS NOT NULL;

CREATE TABLE memory_sources (
    memory_id INTEGER NOT NULL REFERENCES memory_items (id) ON DELETE CASCADE,
    entity    TEXT    NOT NULL,
    note      TEXT,
    PRIMARY KEY (memory_id, entity)
);

CREATE TABLE memory_entities (
    memory_id INTEGER NOT NULL REFERENCES memory_items (id) ON DELETE CASCADE,
    entity    TEXT    NOT NULL,
    PRIMARY KEY (memory_id, entity)
);
CREATE INDEX memory_entities_entity ON memory_entities (entity);

CREATE TABLE memory_edges (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    from_entity TEXT    NOT NULL,
    relation    TEXT    NOT NULL,
    to_entity   TEXT    NOT NULL,
    origin      TEXT    NOT NULL,
    confidence  REAL    NOT NULL CHECK (confidence >= 0.0 AND confidence <= 1.0),
    updated_at  INTEGER NOT NULL,
    revision    INTEGER NOT NULL,
    UNIQUE (from_entity, relation, to_entity, origin)
);
CREATE INDEX memory_edges_from ON memory_edges (from_entity, relation);
CREATE INDEX memory_edges_to ON memory_edges (to_entity, relation);

CREATE TABLE summaries (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    conversation_id INTEGER REFERENCES conversations (id) ON DELETE CASCADE,
    level           TEXT    NOT NULL,    -- segment | recent | weekly | long_term
    content         TEXT    NOT NULL,
    from_message_id INTEGER,
    to_message_id   INTEGER,
    period_start    INTEGER,
    period_end      INTEGER,
    origin          TEXT    NOT NULL,
    created_at      INTEGER NOT NULL,
    revision        INTEGER NOT NULL,
    superseded_by   INTEGER REFERENCES summaries (id)
);
CREATE INDEX summaries_conversation ON summaries (conversation_id, level, period_end DESC);

-- Optional semantic vectors (little-endian f32). Not required for operation.
CREATE TABLE embeddings (
    owner      TEXT    NOT NULL,         -- EntityId string, e.g. memory:5
    model      TEXT    NOT NULL,
    dims       INTEGER NOT NULL,
    vector     BLOB    NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (owner, model)
);

------------------------------------------------------------------------------
-- Operational memory
------------------------------------------------------------------------------

CREATE TABLE tasks (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    title           TEXT    NOT NULL,
    description     TEXT,
    status          TEXT    NOT NULL,
    origin          TEXT    NOT NULL,
    source_entity   TEXT,
    source_note     TEXT,
    conversation_id INTEGER,
    due_at          INTEGER,
    created_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL,
    completed_at    INTEGER,
    revision        INTEGER NOT NULL
);
CREATE INDEX tasks_status_due ON tasks (status, due_at);

CREATE TABLE task_users (
    task_id INTEGER NOT NULL REFERENCES tasks (id) ON DELETE CASCADE,
    user_id INTEGER NOT NULL,
    PRIMARY KEY (task_id, user_id)
);
CREATE INDEX task_users_user ON task_users (user_id);

CREATE TABLE reminders (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    title           TEXT    NOT NULL,
    note            TEXT,
    trigger_kind    TEXT    NOT NULL CHECK (trigger_kind IN ('at', 'conditional')),
    due_at          INTEGER NOT NULL,
    -- ReminderCondition JSON for conditional reminders.
    condition_json  TEXT,
    status          TEXT    NOT NULL,
    origin          TEXT    NOT NULL,
    conversation_id INTEGER,
    task_id         INTEGER REFERENCES tasks (id) ON DELETE SET NULL,
    source_entity   TEXT,
    source_note     TEXT,
    created_at      INTEGER NOT NULL,
    fired_at        INTEGER,
    revision        INTEGER NOT NULL
);
CREATE INDEX reminders_status_due ON reminders (status, due_at);

CREATE TABLE drafts (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    conversation_id INTEGER NOT NULL,
    content         TEXT    NOT NULL,
    origin          TEXT    NOT NULL,
    status          TEXT    NOT NULL,
    created_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL,
    revision        INTEGER NOT NULL
);
CREATE INDEX drafts_conversation ON drafts (conversation_id, status);

CREATE TABLE bookmarks (
    message_id      INTEGER PRIMARY KEY,
    conversation_id INTEGER NOT NULL,
    note            TEXT,
    created_at      INTEGER NOT NULL
);

CREATE TABLE user_notes (
    user_id    INTEGER PRIMARY KEY,
    alias      TEXT,
    note       TEXT,
    favorite   INTEGER NOT NULL DEFAULT 0,
    updated_at INTEGER NOT NULL
);

-- User-changeable settings; value is JSON.
CREATE TABLE settings (
    key        TEXT PRIMARY KEY,
    value      TEXT    NOT NULL,
    updated_at INTEGER NOT NULL
);

------------------------------------------------------------------------------
-- Action engine and agents
------------------------------------------------------------------------------

CREATE TABLE action_proposals (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    kind              TEXT    NOT NULL,
    class             TEXT    NOT NULL,
    identity          TEXT    NOT NULL,
    actor_json        TEXT    NOT NULL,
    payload_json      TEXT    NOT NULL,
    payload_hash      TEXT    NOT NULL,
    status            TEXT    NOT NULL,
    based_on_revision INTEGER NOT NULL,
    rationale         TEXT,
    created_at        INTEGER NOT NULL,
    updated_at        INTEGER NOT NULL
);
CREATE INDEX action_proposals_status ON action_proposals (status, created_at);

CREATE TABLE action_approvals (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    action_id    INTEGER NOT NULL REFERENCES action_proposals (id) ON DELETE CASCADE,
    payload_hash TEXT    NOT NULL,
    nonce        TEXT    NOT NULL UNIQUE,
    approved_at  INTEGER NOT NULL,
    expires_at   INTEGER NOT NULL,
    edited       INTEGER NOT NULL DEFAULT 0,
    consumed_at  INTEGER
);
CREATE INDEX action_approvals_action ON action_approvals (action_id);

CREATE TABLE action_history (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    action_id  INTEGER NOT NULL REFERENCES action_proposals (id) ON DELETE CASCADE,
    at         INTEGER NOT NULL,
    actor_json TEXT    NOT NULL,
    event_kind TEXT    NOT NULL,
    event_json TEXT    NOT NULL,
    revision   INTEGER NOT NULL
);
CREATE INDEX action_history_action ON action_history (action_id, at);
CREATE INDEX action_history_at ON action_history (at);

CREATE TABLE agent_runs (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    harness          TEXT    NOT NULL,
    request_summary  TEXT    NOT NULL,
    status           TEXT    NOT NULL,   -- running | succeeded | failed
    as_of_revision   INTEGER NOT NULL,
    context_items    INTEGER NOT NULL DEFAULT 0,
    tokens_estimated INTEGER NOT NULL DEFAULT 0,
    error            TEXT,
    started_at       INTEGER NOT NULL,
    finished_at      INTEGER
);
CREATE INDEX agent_runs_started ON agent_runs (started_at DESC);

------------------------------------------------------------------------------
-- Hydration bookkeeping
------------------------------------------------------------------------------

CREATE TABLE sync_state (
    key            TEXT PRIMARY KEY,    -- HydrationKey::storage_key()
    observed_at    INTEGER,
    stale_after_ms INTEGER NOT NULL,
    revision       INTEGER NOT NULL,
    dirty          INTEGER NOT NULL DEFAULT 0,
    failure_count  INTEGER NOT NULL DEFAULT 0,
    last_error     TEXT
);

-- Pending hydration work persisted for restart recovery.
CREATE TABLE hydration_jobs (
    key             TEXT PRIMARY KEY,
    priority        INTEGER NOT NULL,
    reason          TEXT    NOT NULL,
    requested_at    INTEGER NOT NULL,
    attempts        INTEGER NOT NULL DEFAULT 0,
    next_attempt_at INTEGER NOT NULL
);

------------------------------------------------------------------------------
-- Full-text search (FTS5, external content, kept in sync by triggers)
------------------------------------------------------------------------------

CREATE VIRTUAL TABLE messages_fts USING fts5 (
    content, content = 'messages', content_rowid = 'id',
    tokenize = 'unicode61 remove_diacritics 2'
);
CREATE TRIGGER messages_fts_ai AFTER INSERT ON messages WHEN new.deleted = 0 BEGIN
    INSERT INTO messages_fts (rowid, content) VALUES (new.id, new.content);
END;
CREATE TRIGGER messages_fts_ad AFTER DELETE ON messages WHEN old.deleted = 0 BEGIN
    INSERT INTO messages_fts (messages_fts, rowid, content) VALUES ('delete', old.id, old.content);
END;
CREATE TRIGGER messages_fts_au AFTER UPDATE OF content, deleted ON messages BEGIN
    INSERT INTO messages_fts (messages_fts, rowid, content)
        SELECT 'delete', old.id, old.content WHERE old.deleted = 0;
    INSERT INTO messages_fts (rowid, content)
        SELECT new.id, new.content WHERE new.deleted = 0;
END;

CREATE VIRTUAL TABLE users_fts USING fts5 (
    username, global_name, content = 'users', content_rowid = 'id',
    tokenize = 'unicode61 remove_diacritics 2'
);
CREATE TRIGGER users_fts_ai AFTER INSERT ON users BEGIN
    INSERT INTO users_fts (rowid, username, global_name) VALUES (new.id, new.username, new.global_name);
END;
CREATE TRIGGER users_fts_ad AFTER DELETE ON users BEGIN
    INSERT INTO users_fts (users_fts, rowid, username, global_name)
        VALUES ('delete', old.id, old.username, old.global_name);
END;
CREATE TRIGGER users_fts_au AFTER UPDATE OF username, global_name ON users BEGIN
    INSERT INTO users_fts (users_fts, rowid, username, global_name)
        VALUES ('delete', old.id, old.username, old.global_name);
    INSERT INTO users_fts (rowid, username, global_name) VALUES (new.id, new.username, new.global_name);
END;

CREATE VIRTUAL TABLE memory_fts USING fts5 (
    content, content = 'memory_items', content_rowid = 'id',
    tokenize = 'unicode61 remove_diacritics 2'
);
CREATE TRIGGER memory_fts_ai AFTER INSERT ON memory_items BEGIN
    INSERT INTO memory_fts (rowid, content) VALUES (new.id, new.content);
END;
CREATE TRIGGER memory_fts_ad AFTER DELETE ON memory_items BEGIN
    INSERT INTO memory_fts (memory_fts, rowid, content) VALUES ('delete', old.id, old.content);
END;
CREATE TRIGGER memory_fts_au AFTER UPDATE OF content ON memory_items BEGIN
    INSERT INTO memory_fts (memory_fts, rowid, content) VALUES ('delete', old.id, old.content);
    INSERT INTO memory_fts (rowid, content) VALUES (new.id, new.content);
END;

CREATE VIRTUAL TABLE tasks_fts USING fts5 (
    title, description, content = 'tasks', content_rowid = 'id',
    tokenize = 'unicode61 remove_diacritics 2'
);
CREATE TRIGGER tasks_fts_ai AFTER INSERT ON tasks BEGIN
    INSERT INTO tasks_fts (rowid, title, description) VALUES (new.id, new.title, new.description);
END;
CREATE TRIGGER tasks_fts_ad AFTER DELETE ON tasks BEGIN
    INSERT INTO tasks_fts (tasks_fts, rowid, title, description)
        VALUES ('delete', old.id, old.title, old.description);
END;
CREATE TRIGGER tasks_fts_au AFTER UPDATE OF title, description ON tasks BEGIN
    INSERT INTO tasks_fts (tasks_fts, rowid, title, description)
        VALUES ('delete', old.id, old.title, old.description);
    INSERT INTO tasks_fts (rowid, title, description) VALUES (new.id, new.title, new.description);
END;

CREATE VIRTUAL TABLE notes_fts USING fts5 (
    alias, note, content = 'user_notes', content_rowid = 'user_id',
    tokenize = 'unicode61 remove_diacritics 2'
);
CREATE TRIGGER notes_fts_ai AFTER INSERT ON user_notes BEGIN
    INSERT INTO notes_fts (rowid, alias, note) VALUES (new.user_id, new.alias, new.note);
END;
CREATE TRIGGER notes_fts_ad AFTER DELETE ON user_notes BEGIN
    INSERT INTO notes_fts (notes_fts, rowid, alias, note) VALUES ('delete', old.user_id, old.alias, old.note);
END;
CREATE TRIGGER notes_fts_au AFTER UPDATE OF alias, note ON user_notes BEGIN
    INSERT INTO notes_fts (notes_fts, rowid, alias, note) VALUES ('delete', old.user_id, old.alias, old.note);
    INSERT INTO notes_fts (rowid, alias, note) VALUES (new.user_id, new.alias, new.note);
END;

CREATE VIRTUAL TABLE summaries_fts USING fts5 (
    content, content = 'summaries', content_rowid = 'id',
    tokenize = 'unicode61 remove_diacritics 2'
);
CREATE TRIGGER summaries_fts_ai AFTER INSERT ON summaries BEGIN
    INSERT INTO summaries_fts (rowid, content) VALUES (new.id, new.content);
END;
CREATE TRIGGER summaries_fts_ad AFTER DELETE ON summaries BEGIN
    INSERT INTO summaries_fts (summaries_fts, rowid, content) VALUES ('delete', old.id, old.content);
END;
CREATE TRIGGER summaries_fts_au AFTER UPDATE OF content ON summaries BEGIN
    INSERT INTO summaries_fts (summaries_fts, rowid, content) VALUES ('delete', old.id, old.content);
    INSERT INTO summaries_fts (rowid, content) VALUES (new.id, new.content);
END;
