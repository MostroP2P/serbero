-- Serbero weekly report (docs/evaluation.md §5): the last 7 days.
--
--   sqlite3 -header -column serbero.db < scripts/weekly-report.sql
--
-- Read-only. Every section is one query, so each can be run on its own.

-- Sessions opened, resolved by the parties themselves, and handed off.
SELECT
    (SELECT count(*) FROM sessions
      WHERE opened_at >= strftime('%s', 'now', '-7 days')) AS sessions_opened,
    (SELECT count(DISTINCT e.dispute_id) FROM events e
       JOIN sessions s ON s.dispute_id = e.dispute_id
      WHERE e.kind = 'resolved'
        AND json_extract(e.payload_json, '$.resolved_by') = 'parties'
        AND s.handoff_reason IS NULL
        AND e.created_at >= strftime('%s', 'now', '-7 days')) AS resolved_by_parties,
    (SELECT count(*) FROM sessions
      WHERE handoff_reason IS NOT NULL
        AND updated_at >= strftime('%s', 'now', '-7 days')) AS handed_off;

-- Handoffs by reason, with each reason's share. A rising share of
-- `uncertain` or `round_limit` means a question needs golden cases.
SELECT handoff_reason AS reason,
       count(*) AS handoffs,
       round(100.0 * count(*) / sum(count(*)) OVER (), 1) AS share_pct
  FROM sessions
 WHERE handoff_reason IS NOT NULL
   AND updated_at >= strftime('%s', 'now', '-7 days')
 GROUP BY handoff_reason
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
       (SELECT latency_ms FROM ordered WHERE n = (total + 1) / 2) AS median_latency_ms,
       (SELECT coalesce(sum(input_tokens), 0) FROM recent) AS input_tokens;
