# Quickstart — Phase 4 Escalation Execution Surface

Layers on top of the Phase 1/2 + Phase 3 quickstarts. Complete those
first; Phase 4 assumes a working daemon with Phase 3 enabled and a
Mostro instance that already produces disputes you can escalate.

## Prerequisites (Phase 4 additions)

- Phases 1/2 + Phase 3 installed and verified. Your `serbero.db`
  is at schema v3.
- At least one solver configured with write permission. If your
  deployment intentionally has no write-permission solvers, set
  `[escalation].fallback_to_all_solvers = true` before enabling
  Phase 4 (see `contracts/config.md`).
- The solver's Nostr client can receive gift-wrapped DMs.

## Configure Phase 4

Add the new section to `config.toml`:

```toml
[escalation]
enabled = true
dispatch_interval_seconds = 30
fallback_to_all_solvers = false
```

Verify at least one of the existing `[[solvers]]` entries carries
`permission = "Write"`:

```toml
[[solvers]]
pubkey     = "<hex pubkey of the write solver>"
permission = "Write"
```

## Run

Rebuild and restart the daemon:

```bash
cargo build --release
./target/release/serbero
```

First-boot log lines (among the Phase 1/2/3 output):

```text
schema migration: applied v5 (escalation_dispatches)
Phase 4 escalation dispatcher enabled   dispatch_interval_seconds=30 fallback_to_all_solvers=false write_solver_count=1
Phase 4 dispatcher loop started
```

If Phase 4 is disabled you instead see:

```text
Phase 4 escalation dispatcher disabled (config.escalation.enabled = false)
```

If Phase 4 is enabled but no write-permission solvers are
configured and fallback is off:

```text
Phase 4 escalation dispatcher enabled   dispatch_interval_seconds=30 fallback_to_all_solvers=false write_solver_count=0
WARN  Phase 4 will record escalation_dispatch_unroutable for every handoff until a write-permission solver is configured (or fallback_to_all_solvers is set true)
```

## Verify Phase 4 end-to-end

### 1. Cooperative escalation → structured DM (US1)

1. Drive a Phase 3 session into escalation. Quickest path: cross
   the party-response timeout, exceed `max_rounds` without
   convergence, or run with an unhealthy reasoning provider. The
   Phase 3 engine tick writes `escalation_recommended` and
   `handoff_prepared` audit events.
2. Within `dispatch_interval_seconds` (default 30 s) plus a short
   send latency — budget 60 s total to match SC-201 — the
   write-permission solver's Nostr client surfaces a gift-wrapped
   DM. The body starts with `escalation_handoff/v1` and carries
   both the human summary and the inline JSON payload.
3. Confirm the audit trail:

   ```bash
   sqlite3 serbero.db \
     "SELECT kind, substr(payload_json,1,80), occurred_at
        FROM mediation_events
       WHERE kind IN ('handoff_prepared', 'escalation_dispatched')
       ORDER BY id DESC LIMIT 4;"
   ```

   Expected: one `handoff_prepared` row followed by one
   `escalation_dispatched` row pointing back at the handoff event
   id. The ordering invariant (handoff_prepared < escalation_dispatched by
   `mediation_events.id`) is what makes the dispatcher's
   one-indexed-scan consumer safe.

4. Confirm the dispatch-tracking table:

   ```bash
   sqlite3 serbero.db \
     "SELECT dispatch_id, dispute_id, target_solver, status, fallback_broadcast
        FROM escalation_dispatches ORDER BY dispatched_at DESC LIMIT 5;"
   ```

   Expected: one row with `status = 'dispatched'`,
   `fallback_broadcast = 0`, and `target_solver` matching the
   pubkey of your write-permission solver (assigned-solver path)
   or a comma-separated list (broadcast path).

5. The solver reads the summary, runs `TakeDispute` on the target
   Mostro instance, and proceeds with the resolution using the
   context from the inline JSON payload. Phase 4's responsibility
   ends here — it does not track the solver's subsequent actions.

### 2. External resolution supersedes an undispatched handoff (US2)

1. Drive another Phase 3 escalation, but before the next
   dispatcher cycle (within 30 s by default), go to Mostro and
   resolve the dispute directly (e.g. `admin-settle`).
2. Serbero's Phase 1/2 observer flips
   `disputes.lifecycle_state → 'resolved'`.
3. On the dispatcher's next cycle, Phase 4 detects the resolution
   and skips the DM. Verify:

   ```bash
   sqlite3 serbero.db \
     "SELECT kind, json_extract(payload_json, '$.reason'), occurred_at
        FROM mediation_events
       WHERE kind = 'escalation_superseded'
       ORDER BY occurred_at DESC LIMIT 3;"
   ```

   Expected: one row with `reason = 'dispute_already_resolved'`.
   No `escalation_dispatches` row exists for this handoff; no DM
   arrives at the solver.

### 3. No write-permission solvers (US3)

1. Rewrite `config.toml` so every `[[solvers]]` entry has
   `permission = "Read"`. Keep
   `fallback_to_all_solvers = false`. Restart the daemon.
2. Drive a Phase 3 escalation.
3. Observe:
   - An ERROR-level log line:
     `Phase 4 dispatch unroutable: no Write-permission solvers configured (fallback_to_all_solvers=false) dispute_id=...`.
   - An `escalation_dispatch_unroutable` audit row.
   - No dispatch-tracking row and no DM at any solver's inbox.
4. Flip `fallback_to_all_solvers = true` and restart. On the
   next dispatcher cycle the handoff picks up from where it was:

   ```bash
   sqlite3 serbero.db \
     "SELECT dispatch_id, target_solver, fallback_broadcast, status
        FROM escalation_dispatches ORDER BY dispatched_at DESC LIMIT 1;"
   ```

   Expected: `fallback_broadcast = 1`, `target_solver` is a
   comma-separated list of every configured solver,
   `status = 'dispatched'`.

### 4. All recipients fail → `status = 'send_failed'` (SC-208)

This is hard to reproduce intentionally because it requires every
configured solver's relay to be unreachable. If you have a staging
environment with a single solver configured against a temporarily-
offline relay, shut down that relay after the daemon starts,
drive a Phase 3 escalation, and observe:

```bash
sqlite3 serbero.db \
  "SELECT dispatch_id, status FROM escalation_dispatches
    ORDER BY dispatched_at DESC LIMIT 1;"

sqlite3 serbero.db \
  "SELECT COUNT(*) FROM notifications
    WHERE dispute_id = ? AND status = 'failed';"
```

Expected: the dispatch row carries `status = 'send_failed'`, and
the `notifications` table has one `status = 'failed'` row per
attempted recipient. The operator query for "which dispatches
reached nobody" resolves from `escalation_dispatches` alone
without a JOIN (SC-208).

## Inspect

```bash
# Every Phase 4 audit event for a dispute (ordered by time)
sqlite3 serbero.db \
  "SELECT id, kind, substr(payload_json, 1, 120), occurred_at
     FROM mediation_events
    WHERE kind IN ('escalation_dispatched', 'escalation_superseded',
                   'escalation_dispatch_unroutable',
                   'escalation_dispatch_parse_failed')
      AND json_extract(payload_json, '$.dispute_id') = '<dispute_id>'
    ORDER BY id ASC;"

# Dispatch reconciliation (should return zero rows in steady state)
sqlite3 serbero.db -header \
  "SELECT d.dispatch_id
     FROM escalation_dispatches d
     LEFT JOIN mediation_events e
       ON json_extract(e.payload_json, '$.dispatch_id') = d.dispatch_id
      AND e.kind = 'escalation_dispatched'
    WHERE e.id IS NULL;"
```

## Audit: Phase 4 never executed a dispute-closing action (SC-207 / Constitution I)

```bash
# Phase 4 does not sign admin-settle / admin-cancel; no Phase-4-authored
# events of those types should exist.
sqlite3 serbero.db \
  "SELECT COUNT(*) FROM notifications
    WHERE notif_type IN ('admin_settle','admin_cancel');"
# Expected: 0 (inherited from Phase 3's audit — Phase 4 adds nothing here).
```

Combined with the constitutional invariant that Serbero holds no
credentials for those actions, Phase 4 satisfies Fund Isolation
First by construction. The dispatcher's only side effects are
gift-wrap DMs to solvers and appends to `escalation_dispatches` +
`mediation_events`.
