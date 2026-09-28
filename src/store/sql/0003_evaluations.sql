-- Every judge request and the action taken on it (docs/spec.md §8): turn
-- evaluations and briefs, each tagged with its question-set identifier so
-- any of them can be replayed against a newer version.
CREATE TABLE evaluations (
    id                   INTEGER PRIMARY KEY,
    session_id           TEXT NOT NULL REFERENCES sessions (session_id),
    question_set_version TEXT NOT NULL,
    judge_id             TEXT NOT NULL,
    last_message_id      INTEGER NOT NULL,
    answers_json         TEXT NOT NULL,
    action_json          TEXT NOT NULL,
    input_tokens         INTEGER,
    latency_ms           INTEGER,
    created_at           INTEGER NOT NULL
);

CREATE INDEX idx_evaluations_session ON evaluations (session_id, id);
