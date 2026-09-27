-- Durable action intent; no credentials and no automatic retries.
CREATE TABLE outbound_operations (
    action_id INTEGER PRIMARY KEY REFERENCES action_proposals(id),
    nonce TEXT NOT NULL UNIQUE,
    account_id INTEGER NOT NULL,
    conversation_id INTEGER,
    kind TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('pending','submitted','confirmed','uncertain','failed')),
    remote_id INTEGER,
    started_at INTEGER NOT NULL,
    confirmed_at INTEGER,
    error TEXT
);
CREATE INDEX outbound_reconciliation ON outbound_operations(account_id, conversation_id, state);
