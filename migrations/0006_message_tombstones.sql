-- Preserve message deletions that arrive before the corresponding message
-- or before history hydration. Message ids are globally unique, so the id is
-- sufficient to prevent any later source from recreating the message.
CREATE TABLE message_tombstones (
    message_id      INTEGER PRIMARY KEY,
    conversation_id INTEGER NOT NULL
);

-- Existing soft-deleted rows already represent observed deletions. Keep a
-- tombstone as well so retention pruning cannot make them reappear later.
INSERT OR IGNORE INTO message_tombstones (message_id, conversation_id)
SELECT id, conversation_id FROM messages WHERE deleted = 1;
