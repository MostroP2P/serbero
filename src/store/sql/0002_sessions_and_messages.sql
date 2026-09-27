-- Mediation sessions: Serbero assisting one dispute (docs/spec.md §7).
CREATE TABLE sessions (
    session_id          TEXT PRIMARY KEY,
    dispute_id          TEXT NOT NULL REFERENCES disputes (dispute_id),
    state               TEXT NOT NULL CHECK (state IN
                            ('opening', 'active', 'guiding', 'handed_off', 'closed', 'superseded')),
    buyer_trade_pubkey  TEXT NOT NULL,
    seller_trade_pubkey TEXT NOT NULL,
    fiat_amount         TEXT,
    fiat_code           TEXT,
    payment_method      TEXT,
    order_published_at  INTEGER,
    buyer_lang          TEXT,
    seller_lang         TEXT,
    buyer_chat_cursor   INTEGER,
    seller_chat_cursor  INTEGER,
    rounds              INTEGER NOT NULL DEFAULT 0,
    handoff_reason      TEXT,
    opened_at           INTEGER NOT NULL,
    updated_at          INTEGER NOT NULL
);

-- At most one live session per dispute.
CREATE UNIQUE INDEX one_live_session ON sessions (dispute_id)
    WHERE state NOT IN ('closed', 'superseded');

-- Every message exchanged with a party, both directions.
CREATE TABLE messages (
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
    -- Durable inner-id dedup: rejects relay replays and re-wrapped messages.
    UNIQUE (session_id, inner_event_id)
);

CREATE INDEX idx_messages_session ON messages (session_id, created_at, id);
