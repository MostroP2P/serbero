-- Disputes seen on the Mostro node, one row per dispute id.
CREATE TABLE disputes (
    dispute_id       TEXT PRIMARY KEY,
    initiator        TEXT NOT NULL CHECK (initiator IN ('buyer', 'seller', 'unknown')),
    status           TEXT NOT NULL,             -- Mostro dispute status (kebab-case)
    status_at        INTEGER NOT NULL,          -- created_at of the applied dispute event revision
    lifecycle        TEXT NOT NULL CHECK (lifecycle IN ('new', 'notified', 'taken', 'resolved')),
    assigned_solver  TEXT,
    first_seen_at    INTEGER NOT NULL,
    last_notified_at INTEGER,
    updated_at       INTEGER NOT NULL
);

CREATE INDEX idx_disputes_lifecycle ON disputes (lifecycle, last_notified_at);

-- Append-only audit log.
CREATE TABLE events (
    id           INTEGER PRIMARY KEY,
    dispute_id   TEXT NOT NULL,
    session_id   TEXT,
    kind         TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    created_at   INTEGER NOT NULL
);

CREATE INDEX idx_events_dispute ON events (dispute_id, id);
