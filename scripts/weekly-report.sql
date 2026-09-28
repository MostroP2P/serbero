-- Serbero weekly report (docs/evaluation.md §5): the last 7 days.
--
--   sqlite3 -header -column serbero.db < scripts/weekly-report.sql
--
-- Read-only. Every section is one query, so each can be run on its own.

-- Sessions opened, resolved by the parties themselves, and handed off.
WITH handoffs AS (
    -- A handoff counts in the week it happened (its `handoff` event, or
    -- `mediation_failed` for `opening_failed`), not when the session last
    -- changed. An opening that failed before a session existed is an
    -- `opening_failed` handoff too.
    SELECT s.handoff_reason AS reason,
           (SELECT min(e.created_at) FROM events e
             WHERE e.session_id = s.session_id
               AND e.kind IN ('handoff', 'mediation_failed')) AS handed_off_at
      FROM sessions s
     WHERE s.handoff_reason IS NOT NULL
    UNION ALL
    SELECT 'opening_failed', created_at
      FROM events
     WHERE kind = 'mediation_failed' AND session_id IS NULL
)
SELECT
    (SELECT count(*) FROM sessions
      WHERE opened_at >= strftime('%s', 'now', '-7 days')) AS sessions_opened,
    (SELECT count(DISTINCT e.dispute_id) FROM events e
       JOIN sessions s ON s.dispute_id = e.dispute_id
      WHERE e.kind = 'resolved'
        AND json_extract(e.payload_json, '$.resolved_by') = 'parties'
        AND s.handoff_reason IS NULL
        -- A solver took it over first: not a self-resolution.
        AND s.state != 'superseded'
        -- Mostro's resolution time; the audit row may be written later,
        -- after downtime.
        AND coalesce(json_extract(e.payload_json, '$.resolved_at'), e.created_at)
            >= CAST(strftime('%s', 'now', '-7 days') AS INTEGER)) AS resolved_by_parties,
    (SELECT count(*) FROM handoffs
      WHERE handed_off_at >= CAST(strftime('%s', 'now', '-7 days') AS INTEGER)) AS handed_off;

-- Handoffs by reason, with each reason's share. A rising share of
-- `uncertain` or `round_limit` means a question needs golden cases.
WITH handoffs AS (
    -- A handoff counts in the week it happened (its `handoff` event, or
    -- `mediation_failed` for `opening_failed`), not when the session last
    -- changed. An opening that failed before a session existed is an
    -- `opening_failed` handoff too.
    SELECT s.handoff_reason AS reason,
           (SELECT min(e.created_at) FROM events e
             WHERE e.session_id = s.session_id
               AND e.kind IN ('handoff', 'mediation_failed')) AS handed_off_at
      FROM sessions s
     WHERE s.handoff_reason IS NOT NULL
    UNION ALL
    SELECT 'opening_failed', created_at
      FROM events
     WHERE kind = 'mediation_failed' AND session_id IS NULL
)
SELECT reason,
       count(*) AS handoffs,
       round(100.0 * count(*) / sum(count(*)) OVER (), 1) AS share_pct
  FROM handoffs
 WHERE handed_off_at >= CAST(strftime('%s', 'now', '-7 days') AS INTEGER)
 GROUP BY reason
 ORDER BY handoffs DESC, reason;

-- Solver feedback (`wrong <question>` replies), by question.
SELECT json_extract(payload_json, '$.question') AS question,
       count(*) AS feedback
  FROM events
 WHERE kind = 'solver_feedback'
   AND created_at >= strftime('%s', 'now', '-7 days')
 GROUP BY question
 ORDER BY feedback DESC, question;

-- Judge usage: requests, median latency, and input tokens.
WITH recent AS (
    SELECT latency_ms, input_tokens FROM evaluations
     WHERE created_at >= strftime('%s', 'now', '-7 days')
), ordered AS (
    SELECT latency_ms,
           row_number() OVER (ORDER BY latency_ms) AS n,
           count(*) OVER () AS total
      FROM recent
     WHERE latency_ms IS NOT NULL
)
SELECT (SELECT count(*) FROM recent) AS judge_requests,
       -- The middle row, or the mean of the two middle rows.
       (SELECT avg(latency_ms) FROM ordered
         WHERE n IN ((total + 1) / 2, (total + 2) / 2)) AS median_latency_ms,
       (SELECT coalesce(sum(input_tokens), 0) FROM recent) AS input_tokens;
