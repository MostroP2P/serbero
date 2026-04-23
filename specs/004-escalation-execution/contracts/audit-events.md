# Contract — Phase 4 Audit Events

Four new `mediation_events.kind` values are introduced by Phase 4.
This file pins the wire shape of each row's `payload_json` so
operator dashboards, log parsers, and future phases can consume
them safely.

## Common envelope (shared with Phase 3)

Every row still lives in `mediation_events` with the existing
columns: `id`, `session_id` (nullable), `kind`, `payload_json`,
`rationale_id` (nullable; always NULL for Phase 4 rows),
`prompt_bundle_id` (nullable), `policy_hash` (nullable),
`occurred_at`. The Phase 4 rows carry `rationale_id = NULL` always
because the dispatcher does not itself run reasoning; the
upstream `handoff_prepared` row already references the rationale.

`prompt_bundle_id` and `policy_hash` are copied from the upstream
`handoff_prepared` row so the dispatch audit stays pinned to the
bundle that produced the handoff. If the upstream row carried
NULL for either, the Phase 4 row also carries NULL.

`session_id` copies the shape of the upstream `handoff_prepared`
row: NULL for FR-122 dispute-scoped handoffs, otherwise set.

## `escalation_dispatched`

Fires after the send loop completes, inside the same transaction
as the `escalation_dispatches` table row (FR-211).

### Payload

```json
{
  "dispatch_id": "<uuid-v4>",
  "dispute_id": "<dispute_id>",
  "handoff_event_id": <integer>,
  "target_solver": "<hex-pubkey or comma-separated list>",
  "status": "dispatched" | "send_failed",
  "fallback_broadcast": true | false
}
```

| Key                   | Required | Notes                                                                                                |
|-----------------------|----------|------------------------------------------------------------------------------------------------------|
| `dispatch_id`         | yes      | Same UUID as `escalation_dispatches.dispatch_id`.                                                    |
| `dispute_id`          | yes      |                                                                                                      |
| `handoff_event_id`    | yes      | Points back to the `handoff_prepared` row that triggered this dispatch.                              |
| `target_solver`       | yes      | Matches the column of the same name in `escalation_dispatches`; see data-model.md encoding rules.    |
| `status`              | yes      | Mirrors `escalation_dispatches.status`. Never written as any other value.                            |
| `fallback_broadcast`  | yes      | `true` when FR-202 rule 3 fired (no write-perm solvers, fallback-to-all-solvers on).                 |

## `escalation_superseded`

Fires when FR-208 detects the dispute has already resolved before
the dispatcher's send step. No `escalation_dispatches` row is
written (supersession is a non-event from the dispatch table's
perspective — FR-212).

### Payload

```json
{
  "dispute_id": "<dispute_id>",
  "handoff_event_id": <integer>,
  "reason": "dispute_already_resolved"
}
```

The `reason` field is an enum with exactly one valid value today
(`"dispute_already_resolved"`). The field is kept for forward
compatibility — future lifecycle states that should also
supersede an undispatched handoff can add new enum values without
changing the kind.

## `escalation_dispatch_unroutable`

Fires when FR-202 rule 4 matches: zero write-permission solvers
are configured AND `[escalation].fallback_to_all_solvers = false`.
The handoff event is NOT marked consumed (no dispatch-tracking row
written), so a later config change that adds a write-permission
solver picks the handoff up on the next cycle.

### Payload

```json
{
  "dispute_id": "<dispute_id>",
  "handoff_event_id": <integer>,
  "configured_solver_count": <integer>,
  "fallback_to_all_solvers": false
}
```

| Key                         | Required | Notes                                                          |
|-----------------------------|----------|----------------------------------------------------------------|
| `configured_solver_count`   | yes      | Total configured solvers (any permission). Zero is a valid value; the spec's US3 covers the "only read-perm solvers" scenario explicitly. |
| `fallback_to_all_solvers`   | yes      | Always `false` here by construction — encodes "did FR-202 rule 3 fire?" (the read-side analogue of `via_fallback` on the dispatched path), NOT the raw `[escalation].fallback_to_all_solvers` config flag. Reaching this audit kind means rule 3 did not fire, which holds for both shapes the router collapses onto Unroutable: (a) rule 3 gated off via the config flag, and (b) rule 3 enabled via the config flag but zero solvers configured (nothing to fall back to). Operator queries for "every unroutable event" can filter `WHERE fallback_to_all_solvers = false` confidently. The raw config flag for interactive debugging is carried by the paired `phase4_unroutable` ERROR log line's `config_fallback_to_all_solvers` field. |

## `escalation_dispatch_parse_failed`

Fires when the upstream `handoff_prepared` row is structurally
broken. The event is marked consumed (the queue moves forward);
manual operator action is required to re-dispatch. Two sub-shapes
exist, disambiguated by the `reason` field:

### Payload

```json
{
  "dispute_id": "<dispute_id>",
  "handoff_event_id": <integer>,
  "reason": "deserialize_failed" | "orphan_dispute_reference",
  "detail": "<operator-readable message>"
}
```

| `reason` value               | Meaning                                                                                |
|------------------------------|----------------------------------------------------------------------------------------|
| `deserialize_failed`         | `payload_json` does not parse into a `HandoffPackage`. `detail` carries the parser error. |
| `orphan_dispute_reference`   | Payload parses cleanly but the `dispute_id` has no row in `disputes`. `detail` carries `"dispute_id not found"`. |

For `orphan_dispute_reference` the `dispute_id` field at the top
level of the payload MAY be the string that failed lookup (a best-
effort field — it's what the dispatcher attempted to resolve).

## Operator query patterns

Common canned queries these events unlock:

```sql
-- Every dispatch that reached zero recipients (SC-208)
SELECT * FROM escalation_dispatches WHERE status = 'send_failed';

-- Every handoff that was stranded because nobody has write perm (FR-213)
SELECT me.id, me.occurred_at,
       json_extract(me.payload_json, '$.dispute_id') AS dispute_id
  FROM mediation_events me
 WHERE me.kind = 'escalation_dispatch_unroutable'
 ORDER BY me.occurred_at DESC;

-- Reconcile dispatch table against audit events (SC-203)
SELECT d.dispatch_id
  FROM escalation_dispatches d
  LEFT JOIN mediation_events e
    ON json_extract(e.payload_json, '$.dispatch_id') = d.dispatch_id
   AND e.kind = 'escalation_dispatched'
 WHERE e.id IS NULL
UNION ALL
SELECT json_extract(e.payload_json, '$.dispatch_id')
  FROM mediation_events e
  LEFT JOIN escalation_dispatches d
    ON json_extract(e.payload_json, '$.dispatch_id') = d.dispatch_id
 WHERE e.kind = 'escalation_dispatched'
   AND d.dispatch_id IS NULL;
-- Any returned rows indicate mismatches; steady-state expected: zero.

-- Supersessions (stale handoffs that would have been noise)
SELECT me.id, me.occurred_at,
       json_extract(me.payload_json, '$.dispute_id') AS dispute_id
  FROM mediation_events me
 WHERE me.kind = 'escalation_superseded'
 ORDER BY me.occurred_at DESC;

-- Parse failures that need manual re-dispatch
SELECT me.id, me.occurred_at,
       json_extract(me.payload_json, '$.reason') AS reason,
       json_extract(me.payload_json, '$.detail') AS detail
  FROM mediation_events me
 WHERE me.kind = 'escalation_dispatch_parse_failed'
 ORDER BY me.occurred_at DESC;
```

## Invariants

- Every `escalation_dispatched` audit row has exactly one
  matching `escalation_dispatches` row (and vice versa). Enforced
  by writing both in the same transaction.
- `escalation_superseded`, `escalation_dispatch_unroutable`, and
  `escalation_dispatch_parse_failed` are NEVER paired with an
  `escalation_dispatches` row.
- A single `handoff_event_id` produces at most one
  `escalation_dispatched` event (FR-203 / SC-205 dedup). It MAY
  be paired with a later `escalation_superseded` or
  `escalation_dispatch_*` event only if the prior `dispatched`
  row did not exist — the dispatcher's consumer query filters
  dispatched rows out of the pending set, so a second dispatch
  attempt cannot occur.
