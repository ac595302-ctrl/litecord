-- Verified REST coverage is independent of live Gateway sequence/high-water.
-- Live messages never advance this cursor over an uncommitted/dropped event.
CREATE TABLE account_catchup (
    conversation_id INTEGER PRIMARY KEY REFERENCES conversations(id),
    newest_message_id INTEGER,
    checked_at INTEGER NOT NULL DEFAULT 0,
    retry_at INTEGER NOT NULL DEFAULT 0,
    last_error TEXT
);
INSERT INTO account_catchup (conversation_id, newest_message_id)
SELECT conversation_id, MAX(id) FROM messages
WHERE origin = 'discord_user_session' GROUP BY conversation_id;

-- One canonical message, independent observations from each source.
-- No FK: a deletion can be observed before the message is locally present.
CREATE TABLE message_observations (
    message_id INTEGER NOT NULL,
    origin TEXT NOT NULL,
    first_observed_at INTEGER NOT NULL,
    last_observed_at INTEGER NOT NULL,
    deleted INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (message_id, origin)
);
INSERT INTO message_observations
SELECT id, origin, observed_at, observed_at, deleted FROM messages;
