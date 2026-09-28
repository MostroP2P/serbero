-- Serbero judge cost (docs/evaluation.md §5): input tokens per day and
-- judge for the last 30 days, and their cost at a price you set.
--
--   sqlite3 -header -column serbero.db \
--     -cmd ".parameter set :usd_per_million_tokens 0.25" \
--     < scripts/cost-report.sql
--
-- Read-only. Without the parameter the cost column is 0.

SELECT date(created_at, 'unixepoch') AS day,
       judge_id,
       count(*) AS requests,
       coalesce(sum(input_tokens), 0) AS input_tokens,
       round(coalesce(sum(input_tokens), 0) / 1e6
             * coalesce(:usd_per_million_tokens, 0), 4) AS cost_usd
  FROM evaluations
 WHERE created_at >= strftime('%s', 'now', '-30 days')
 GROUP BY day, judge_id
 ORDER BY day, judge_id;
