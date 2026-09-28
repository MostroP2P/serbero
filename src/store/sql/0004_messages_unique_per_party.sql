-- The inner event of a chat message names no recipient, so the same text
-- sent to both parties in one second has one inner id. Deduplicate per
-- direction and party: an inbound replay is still caught, because a party's
-- inner event is always signed by that party.
CREATE TABLE messages_new (
    id             INTEGER PRIMARY KEY,
    session_id     TEXT NOT NULL REFERENCES sessions (session_id),
    direction      TEXT NOT NULL CHECK (direction IN ('in', 'out')),
    party          TEXT NOT NULL CHECK (party IN ('buyer', 'seller')),
    template_id    TEXT,
    lang           TEXT,
    content        TEXT NOT NULL,
    attachments    INTEGER NOT NULL DEFAULT 0,
    inner_event_id TEXT NOT NULL,
    created_at     INTEGER NOT NULL,
    UNIQUE (session_id, direction, party, inner_event_id)
);

INSERT INTO messages_new
    SELECT id, session_id, direction, party, template_id, lang, content,
           attachments, inner_event_id, created_at
    FROM messages;

DROP TABLE messages;
ALTER TABLE messages_new RENAME TO messages;

CREATE INDEX idx_messages_session ON messages (session_id, created_at, id);
