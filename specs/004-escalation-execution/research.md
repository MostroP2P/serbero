# Phase 0 Research — Escalation Execution Surface

Every slot in the Technical Context was filled without
`NEEDS CLARIFICATION` markers because Phase 4 reuses the Phase 1/2/3
runtime choices directly. This research file records the decisions
that landed, the rationale for each, and the alternatives considered
and rejected. No outstanding research tasks remain; Phase 1 design
can proceed.

## Decision 1 — Delivery transport

**Decision**: reuse the existing Phase 1/2 notifier (gift-wrapped DMs
via `nostr-sdk`). No new transport.

**Rationale**: the notifier already handles per-recipient
`notifications.status = sent | failed` bookkeeping, clock-guard
discipline, and relay retry semantics consistent with the rest of
the daemon. Phase 4's "for your records" DM has the same delivery
expectations as the Phase 3 summary DM.

**Alternatives considered**:

- A new transport (HTTP webhook, email, Slack) — rejected.
  Violates the "Nostr-Native Coordination" constitutional
  principle and requires new credential management. Out of scope
  for v1; out-of-band mechanisms belong at the operator layer,
  not in the Phase 4 dispatcher.
- A distinct gift-wrap path bypassing the Phase 1/2 notifier —
  rejected. Duplicates bookkeeping that already exists and
  introduces a second source of truth about delivery outcome.

## Decision 2 — Consumption scan strategy

**Decision**: one indexed `LEFT JOIN` per cycle —
`mediation_events LEFT JOIN escalation_dispatches
 ON escalation_dispatches.handoff_event_id = mediation_events.id`
filtered to `kind = 'handoff_prepared'` AND
`escalation_dispatches.dispatch_id IS NULL`.

**Rationale**: SQLite handles the join trivially at the volumes
Phase 3 produces (low units per hour). The join is the simplest
expression of "which handoffs have not yet been dispatched" and it
guarantees correct behavior after a daemon restart without a
per-event cursor.

**Alternatives considered**:

- Maintain a "last seen handoff event id" cursor — rejected.
  Adds state that must itself be persisted atomically with the
  dispatch, and is error-prone on crash-between-send-and-audit.
  The join is already cheap enough.
- SQLite WAL-mode triggers that enqueue dispatches on
  `handoff_prepared` insert — rejected. Couples the Phase 3 write
  path to Phase 4 state, violating FR-217 ("no cross-phase
  coupling") and making Phase 4's enabled-flag harder to honor.

## Decision 3 — Event-kind inventory (audit trail)

**Decision**: four new `MediationEventKind` variants —
`EscalationDispatched`, `EscalationSuperseded`,
`EscalationDispatchUnroutable`,
`EscalationDispatchParseFailed`. The orphan-dispute edge case
folds into `EscalationDispatchParseFailed` via a `reason` field
(`deserialize_failed` vs. `orphan_dispute_reference`), consistent
with the 2026-04-22 clarification session.

**Rationale**: one kind per operator-actionable state.
Consolidating orphan and deserialize failures keeps the inventory
tight — the orphan case is explicitly "theoretically impossible"
per the spec, and both failure modes share the same resolution
path (manual operator intervention). Adding a new kind for an
unreachable case would inflate the enum without value.

**Alternatives considered**:

- A separate `EscalationDispatchOrphan` kind — rejected (see
  2026-04-22 clarification session in spec.md).
- Reusing existing Phase 3 kinds (e.g., piggy-backing on
  `handoff_prepared` with a status field) — rejected.
  `handoff_prepared` is Phase 3's write; Phase 4 events are
  Phase 4's write. Keeping producer / consumer kinds distinct
  preserves the audit narrative ("Phase 3 prepared, Phase 4
  dispatched or superseded").

## Decision 4 — Dispatch-tracking table vs. column on existing rows

**Decision**: a new table `escalation_dispatches` keyed by
`dispatch_id` (UUID v4), with an FK-ish `handoff_event_id`
referencing `mediation_events.id`.

**Rationale**: Phase 4's notion of a dispatch attempt is a first-
class entity (it can have `status` = dispatched or send_failed;
it has its own timestamps; it relates many-to-one to a handoff).
Putting these semantics as columns on `mediation_events` would
bloat a table designed for append-only kind-payload audit rows.
The separation also makes the dedup lookup FR-203 / SC-205 a
single-table query rather than a kind-filtered self-join.

**Alternatives considered**:

- Store dispatch status in a new column on
  `mediation_events` — rejected for the reasons above; also makes
  `mediation_events` harder to evolve.
- No dispatch-tracking table at all; treat the
  `escalation_dispatched` audit event as the dispatch record —
  rejected. Loses the ability to carry `send_failed` cleanly
  (would need a distinct audit kind), and entangles "did we
  attempt" with "here is the operator-readable narrative of what
  happened".

## Decision 5 — Broadcast vs. assignment-aware routing

**Decision**: honor `disputes.assigned_solver` ONLY when that
pubkey is configured locally AND has `Write` permission.
Otherwise broadcast to every configured `Write` solver. The
read-permission assignment case is explicitly ignored (the spec's
US1 scenario 4 pins this).

**Rationale**: the assignment field was set by Phase 1/2 during
solver targeting, which ran without permission-level awareness.
Trusting a Read-permission assignment for a write-required
handoff would be a correctness bug, not a routing optimization.
Broadcasting to the write set is the safe default.

**Alternatives considered**:

- Target ANY assigned solver regardless of permission — rejected
  (breaks the Phase 4 → write-permission contract).
- Never target an assigned solver; always broadcast — rejected
  (loses the "one solver already owns this dispute" signal;
  unnecessary inbox noise for the other configured write
  solvers).
- Split into two DMs (targeted + broadcast) — rejected. The
  simpler rule is easier to audit; the assigned solver does not
  need a duplicate DM.

## Decision 6 — DM body format: versioned prefix

**Decision**: every DM body starts with the literal line
`escalation_handoff/v1` followed by the structured content.

**Rationale**: matches the FR-124 final-report DM pattern
(`mediation_resolution_report/v1`). Log parsers and solver-side
tooling can detect the format at a glance, and future format
changes can bump the version without breaking older parsers.

**Alternatives considered**:

- JSON-only body — rejected. Loses the "quick human scan" use
  case; operators reading the DM in a Nostr client shouldn't
  need to mentally parse JSON.
- Human-summary only — rejected (fails FR-204 requirement for a
  machine-readable payload).
- Separate gift-wraps for human and machine sections — rejected.
  Doubles delivery cost and loses atomicity; one DM with two
  clearly-separated sections matches the solver's mental model.

## Decision 7 — Idempotency and at-least-once semantics

**Decision**: dedup by `handoff_event_id`. Phase 4 favors
at-least-once over at-most-once — a dispatcher crash after DM
send but before the `escalation_dispatches` write may cause the
DM to be sent a second time on restart.

**Rationale**: losing a handoff is worse than duplicating one.
The solver's client handles deduplication on their side (Nostr
event ids are idempotent by construction). The `notifications`
table captures every attempt so the operator can see the
duplicates explicitly.

**Alternatives considered**:

- At-most-once semantics via a pre-send lock row — rejected.
  Adds write ordering complexity, doesn't actually remove the
  failure mode (the crash can still happen between lock release
  and audit write).
- Two-phase commit across the send + audit — rejected.
  Overkill for a notification path where duplicates are
  tolerable and losses are not.
