# Feature Specification: Phase 4 — Escalation Execution Surface

**Feature Branch**: `004-escalation-execution`
**Created**: 2026-04-22
**Status**: Draft
**Input**: User description: Phase 4 of the Serbero dispute coordination pipeline — consume the escalation handoff packages produced by Phase 3 and dispatch structured context to human solvers with write permission.

## Clarifications

### Session 2026-04-22

- Q: Should `escalation_dispatches.status` capture the send outcome, or is the split-brain between dispatch-intent and per-recipient delivery the intended design? → A: Option A — extend the status enum with `send_failed`. The dispatcher writes `dispatched` when at least one targeted recipient succeeded and `send_failed` when every recipient failed. Partial-success cases stay `dispatched`; per-recipient forensic detail remains in the existing notifications table.

## User Scenarios & Testing *(mandatory)*

### User Story 1 — Write-Permission Solver Receives Structured Escalation (Priority: P1)

A human solver configured with write permission opens their Nostr client and
finds a gift-wrapped DM from Serbero. The DM contains a short natural-language
summary of why Serbero escalated a dispute ("Escalation required for dispute
X. Trigger: conflicting_claims. Please run `TakeDispute` for dispute X to
review the full context.") plus a machine-readable handoff payload carrying
every datum Phase 3 collected (dispute id, session id if any, escalation
trigger, evidence references, rationale references, prompt bundle id, policy
hash). The solver reads the summary, runs `TakeDispute` on their Mostro
instance, and works the case with full context in hand.

**Why this priority**: Every other Phase 4 capability exists to make this
one case work reliably. Without it, Phase 3's handoff packages stay buried
in an audit table and human operators receive only the free-form Phase
1/2-style "needs human judgment" message, losing the structured context
Serbero spent the whole mediation session assembling. This is the MVP.

**Independent Test**: Seed one `handoff_prepared` audit event for a
dispute; configure exactly one solver with write permission; run the
Phase 4 dispatcher; verify that a single gift-wrap DM arrives at that
solver's pubkey and that the DM body contains both the human-readable
summary (including dispute id and trigger) and the machine-readable
handoff payload.

**Acceptance Scenarios**:

1. **Given** a Phase 3 session closed with `escalation_recommended` + a
   `handoff_prepared` audit event and one configured solver with write
   permission, **when** the dispatcher runs its next cycle, **then** the
   write-permission solver receives exactly one gift-wrap DM containing
   the human summary, the action instruction, and the machine-readable
   handoff payload, and an `escalation_dispatched` audit event is
   recorded with the target solver pubkey.

2. **Given** a Phase 3 opening-call escalation that wrote a
   dispute-scoped `handoff_prepared` event with `session_id = NULL`
   (FR-122 shape) and two configured solvers both with write
   permission, **when** the dispatcher runs, **then** the dispute's
   `assigned_solver` is NULL so the dispatcher broadcasts to both
   write-permission solvers; the DM body carries a null / "<none>"
   session marker so the recipient understands no mediation session
   existed.

3. **Given** a dispute whose `assigned_solver` pubkey matches a
   configured write-permission solver, **when** the dispatcher fires,
   **then** the DM targets only the assigned solver (not a broadcast)
   and the audit row records that specific pubkey as
   `target_solver`.

4. **Given** a dispute whose `assigned_solver` pubkey matches a
   configured READ-permission solver, **when** the dispatcher fires,
   **then** the read-permission assignment is ignored and the DM is
   broadcast to every configured write-permission solver instead.

---

### User Story 2 — External Resolution Supersedes an Undispatched Escalation (Priority: P2)

A Phase 3 session escalated at 10:00, writing a `handoff_prepared` event.
Before the Phase 4 dispatcher's next cycle, a human solver resolves the
dispute directly on the Mostro side (e.g., `admin-settle`). Phase 1/2 picks
up the terminal `DisputeStatus` and flips the dispute's `lifecycle_state`
to `resolved`. When Phase 4's cycle finally runs, it must detect the
externally-resolved dispute and skip the dispatch — sending a DM at this
point would just confuse the solver, who already resolved the case.

**Why this priority**: Without this the solver receives a stale
"please take this dispute" DM after they've already closed it, eroding
trust in Serbero's signal-to-noise ratio. The check is a cheap read
against existing Phase 1/2 state, so the cost of doing it right is low.

**Independent Test**: Seed a `handoff_prepared` event for a dispute
whose `lifecycle_state` is already `resolved`; run the dispatcher;
verify that no DM is sent, no `escalation_dispatched` row is written,
and an `escalation_superseded` audit event records the skip.

**Acceptance Scenarios**:

1. **Given** a `handoff_prepared` event for a dispute whose
   `lifecycle_state = 'resolved'` at the moment the dispatcher
   examines it, **when** the dispatcher cycle runs, **then** no DM is
   sent, no `escalation_dispatched` row is written, and an
   `escalation_superseded` audit event (dispute-scoped when no
   session, session-scoped otherwise) records the skip with the
   reason `dispute_already_resolved`.

2. **Given** a `handoff_prepared` event whose dispute is still open
   when the dispatcher examines it and then resolves between the
   check and the DM send (rare race), **when** the dispatcher
   completes the send, **then** the dispatch still counts (the DM
   was already delivered) — Phase 4 does NOT attempt to recall or
   suppress an in-flight DM.

---

### User Story 3 — No Write-Permission Solvers Configured (Priority: P3)

An operator deploys Serbero with a configuration that lists only
read-permission solvers (for example, an observability / audit setup).
A Phase 3 session escalates and writes a `handoff_prepared` event.
The Phase 4 dispatcher must not silently drop the escalation on the
floor: the operator has to be told, via logs and audit, that the
handoff has nowhere to go.

**Why this priority**: The scenario is real (some deployments may
intentionally run read-only Serbero as a first-step observer before
promoting to write-capable solvers) and the failure mode — silent
drop — is costly because it looks like everything is working.

**Independent Test**: Seed a `handoff_prepared` event; configure zero
write-permission solvers (and set `fallback_to_all_solvers = false`);
run the dispatcher; verify that a loud error-level log line is
emitted, an `escalation_dispatch_unroutable` audit event is
recorded, and no DM is sent; flip `fallback_to_all_solvers = true`
and re-run; verify the DM does go out to every configured (read-only)
solver with the audit event's `target_solver` listing all of them.

**Acceptance Scenarios**:

1. **Given** a pending `handoff_prepared` event and a config with
   zero write-permission solvers and `fallback_to_all_solvers =
   false`, **when** the dispatcher runs, **then** an ERROR-level log
   line fires (explicitly naming the dispute id and the
   configuration hint `fallback_to_all_solvers`), an
   `escalation_dispatch_unroutable` audit event is recorded, and no
   DM is sent; a future config change that adds a write-permission
   solver still picks this event up on the next cycle (i.e.,
   "unroutable" does NOT mark the event as consumed).

2. **Given** the same starting state but with
   `fallback_to_all_solvers = true`, **when** the dispatcher runs,
   **then** the DM is broadcast to every configured solver
   regardless of permission; the audit event records
   `target_solver` as the full list and `fallback_broadcast = true`
   so the operator can tell that the dispatch used the fallback
   rather than a real write-permission target.

---

### Edge Cases

- **Duplicate handoff events**: The same `handoff_prepared` audit row
  is observed twice (relay replay of a session-scoped event that
  later got re-recorded; dispatcher crash between DM send and audit
  write). Phase 4 MUST deduplicate by checking for an existing
  `escalation_dispatches` row keyed on `handoff_event_id`. Only the
  first observation dispatches; subsequent observations are silent
  no-ops (they do not re-emit the DM, do not write a duplicate
  audit row, and do not log at WARN — they log at DEBUG because
  dedup is the expected steady-state).

- **Dispatcher crash after DM send, before audit write**: On restart
  the dispatcher sees the `handoff_prepared` event again but has no
  `escalation_dispatches` row, so it re-sends the DM. The solver
  may get two copies of the same escalation. This is accepted
  behavior — Phase 4 favors at-least-once delivery over at-most-once,
  because losing a handoff is worse than duplicating one. The
  audit trail records both dispatches so an operator can see what
  happened.

- **HandoffPackage JSON is malformed**: A `handoff_prepared`
  payload that fails to deserialize cannot be dispatched. Phase 4
  MUST log an ERROR with the event id and the parse error, record
  an `escalation_dispatch_parse_failed` audit event, and advance
  past the row (so a malformed event does not poison the queue).
  The operator receives the alert; re-dispatch requires manual
  intervention.

- **Handoff_prepared event references a dispute that was never
  persisted to `disputes`**: Theoretically impossible (Phase 3's
  write path always has a parent `disputes` row) but defensive:
  the dispatcher logs an ERROR, records
  `escalation_dispatch_orphan`, and skips — no DM, no retries.

- **Gift-wrap send fails for every targeted solver** (relay down,
  all recipients unreachable): each per-recipient failure is
  recorded in the existing `notifications` table with
  `status = 'failed'`. The `escalation_dispatches` row is written
  once with the attempted target list and `status = 'send_failed'`
  so the all-failed outcome is visible from the dispatch table
  alone (no JOIN needed for the common "nothing reached anyone"
  query). If at least one recipient succeeded but others failed,
  the row records `status = 'dispatched'` — partial success is
  still success at the Phase 4 layer, and per-recipient gaps
  remain available in `notifications` for forensic work. Phase 4
  does NOT retry — the notifications table is the operator's hook
  for targeted re-delivery via whatever out-of-band mechanism
  exists.

- **Serbero restart mid-cycle**: The cycle is idempotent (dedup by
  `handoff_event_id`) so a restart just resumes on the next
  interval and picks up anything not yet in `escalation_dispatches`.

- **Engine interval is longer than user tolerance**: The operator
  sets `dispatch_interval_seconds = 600`; the solver sees the DM
  10 minutes after Phase 3 wrote the handoff. Acceptable — the
  interval is a knob, not a guarantee. The success-criteria
  ceiling (60 s) applies to the default configuration.

## Requirements *(mandatory)*

### Functional Requirements

**Consumption & routing**

- **FR-201**: Phase 4 MUST scan the audit log for
  `handoff_prepared` events (session-scoped AND dispute-scoped) that
  do not yet have a corresponding `escalation_dispatches` row, on
  the configured dispatch interval.

- **FR-202**: Phase 4 MUST determine the target recipient list per
  handoff using this rule, evaluated in order:
  1. If the dispute has a non-NULL `assigned_solver` AND that
     solver is configured locally AND that solver has write
     permission → target that solver specifically.
  2. Otherwise → broadcast to every configured solver with write
     permission.
  3. If step 2 produces an empty list AND `fallback_to_all_solvers
     = true` → broadcast to every configured solver regardless of
     permission.
  4. If step 2 produces an empty list AND `fallback_to_all_solvers
     = false` → do not send; record an
     `escalation_dispatch_unroutable` audit event and log at ERROR.

- **FR-203**: Phase 4 MUST deduplicate by `handoff_event_id`. A
  `handoff_prepared` row whose `mediation_events.id` already
  appears in `escalation_dispatches.handoff_event_id` MUST NOT be
  dispatched a second time.

**DM delivery**

- **FR-204**: The dispatched DM MUST carry three discrete sections:
  1. A short human-readable summary line containing the dispute id
     and the escalation trigger.
  2. A human-readable action instruction telling the solver to run
     `TakeDispute` for this dispute on their Mostro instance.
  3. A machine-readable representation of the
     `HandoffPackage` — every field Phase 3 wrote (dispute id,
     optional session id, trigger, evidence_refs, prompt_bundle_id,
     policy_hash, rationale_refs, assembled_at).

- **FR-205**: The DM body MUST carry a version prefix on the first
  line (e.g., `escalation_handoff/v1`) so future format changes
  can be detected by log parsers.

- **FR-206**: The DM body MUST NOT contain raw rationale text.
  Only rationale reference ids (content-hash SHA-256) from the
  handoff package may appear. (FR-120 carry-forward.)

- **FR-207**: The DM body MUST identify Serbero as an assistance
  system, not a Mostro admin or a human solver. (Constitutional
  invariant "Assistance, not authority" carry-forward.)

**Lifecycle integration**

- **FR-208**: Before sending the DM, Phase 4 MUST check the
  dispute's current `lifecycle_state`. If it is already
  `resolved`, Phase 4 MUST NOT send the DM and MUST record an
  `escalation_superseded` audit event with reason
  `dispute_already_resolved`.

- **FR-209**: Phase 4 MUST NOT issue `TakeDispute`, MUST NOT sign
  `admin-settle` or `admin-cancel`, MUST NOT close disputes, and
  MUST NOT move funds. (Fund-isolation carry-forward.)

- **FR-210**: Phase 4 MUST NOT track whether the solver
  acknowledged, read, or acted on the DM. It MUST NOT retry
  delivery to a different solver, re-escalate on timeout, or
  expect an ACK from the recipient. Delivery is the terminal step.

**Audit**

- **FR-211**: Every dispatch attempt that reached the send step
  (i.e. was not superseded and was not unroutable) MUST insert
  exactly one `escalation_dispatches` row (dispatch_id,
  dispute_id, session_id optional, handoff_event_id,
  target_solver, dispatched_at, status) and one
  `escalation_dispatched` `mediation_events` audit row whose
  payload carries dispatch_id, dispute_id, target_solver, and
  the fallback flag if the fallback path was used. The
  `status` column MUST be set according to the per-recipient
  outcomes:
  - `dispatched` when at least one recipient in the target list
    successfully received the gift-wrapped DM.
  - `send_failed` when every recipient in the target list failed
    (network error, malformed pubkey, relay rejection, etc.).
  Partial success counts as `dispatched`; per-recipient gaps
  remain visible in `notifications.status`.

- **FR-212**: Every supersession (FR-208) MUST insert one
  `escalation_superseded` `mediation_events` audit row and MUST NOT
  insert an `escalation_dispatches` row — "superseded" is a
  non-event from the dispatch table's perspective.

- **FR-213**: Every unroutable handoff (FR-202 rule 4) MUST
  insert one `escalation_dispatch_unroutable` audit row AND emit
  one ERROR-level log line. The handoff event stays unconsumed so
  a future config change can pick it up.

- **FR-214**: Every malformed handoff payload (cannot deserialize
  the JSON to `HandoffPackage`) MUST insert one
  `escalation_dispatch_parse_failed` audit row AND emit one
  ERROR-level log line. The event is considered consumed (so the
  queue moves forward) — manual operator action is required to
  re-dispatch.

**Config**

- **FR-215**: A new `[escalation]` config section MUST expose at
  least: `enabled` (bool; false disables Phase 4 entirely),
  `dispatch_interval_seconds` (positive integer, default 30), and
  `fallback_to_all_solvers` (bool, default false).

- **FR-216**: When `[escalation].enabled = false`, Phase 4 MUST NOT
  spawn the dispatcher task. Phase 1/2/3 behavior MUST be
  unaffected.

**Non-interference with earlier phases**

- **FR-217**: Phase 4 MUST NOT modify `disputes`,
  `mediation_sessions`, or any Phase 1/2/3 table other than
  writing to its own `escalation_dispatches` table and appending
  to `mediation_events`. No UPDATE to existing rows, no deletion,
  no FK cascade changes.

- **FR-218**: Phase 4 MUST NOT depend on or alter the Phase 3
  engine tick. The two loops run in parallel and share nothing
  beyond the audit table (read-only from Phase 4's side).

### Key Entities

- **HandoffPackage** (existing, produced by Phase 3): structured
  payload carrying dispute id, optional session id, escalation
  trigger, evidence refs, prompt bundle id, policy hash, rationale
  refs, assembled-at timestamp. Phase 4 deserializes this from
  `mediation_events.payload_json`; Phase 4 does not mutate the
  shape.

- **Escalation Dispatch** (new, table `escalation_dispatches`): one
  row per dispatch attempt that reached the send step. Carries
  dispatch_id (UUID v4), dispute_id, optional session_id,
  handoff_event_id (references the `handoff_prepared` audit row
  that triggered this dispatch), target_solver (pubkey or
  pubkey-list representation; exact encoding is an implementation
  detail), dispatched_at (Unix seconds), status, created_at (Unix
  seconds). Valid `status` values for this feature:
  - `dispatched` — at least one recipient successfully received
    the gift-wrapped DM.
  - `send_failed` — every recipient failed. Per-recipient error
    detail remains in `notifications`.
  (Supersession is recorded as a `mediation_events` audit row, not
  as an `escalation_dispatches` status — see FR-212.)

- **Escalation DM** (new, runtime-only): the gift-wrapped Nostr
  event delivered to the target solver(s). Its body is a
  structured text document with a versioned prefix, a human
  summary, an action instruction, and a machine-readable
  handoff block. Persistence on the delivery side is handled by
  the existing `notifications` table the Phase 1/2 notifier
  already writes.

- **Escalation Audit Event** (extension of existing
  `mediation_events` table): four new `kind` values —
  `escalation_dispatched`, `escalation_superseded`,
  `escalation_dispatch_unroutable`,
  `escalation_dispatch_parse_failed` — carrying dispatch-side
  metadata. Session_id follows the same rules as Phase 3's
  dispute-scoped events: NULL when no session row exists for the
  dispute, set otherwise.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-201**: With the default `dispatch_interval_seconds = 30`,
  95% of handoff packages produced by Phase 3 reach the target
  solver's inbox within 60 seconds of the `handoff_prepared`
  audit event being written. The SLA degrades linearly with
  longer configured intervals; the 60-second figure is specific
  to the default.

- **SC-202**: Every dispatched DM contains both a human-readable
  summary (dispute id + trigger + action instruction) and a
  machine-readable handoff payload. Operator sampling of delivered
  DMs finds zero DMs missing either section.

- **SC-203**: For every dispatch attempt that reached the send step
  there is exactly one dispatch-tracking row AND one
  `escalation_dispatched` audit event, keyed consistently by
  dispatch id. An audit reconciliation between the two stores
  returns zero mismatches over a 24-hour window, regardless of
  whether the dispatch row's status is `dispatched` or
  `send_failed`.

- **SC-204**: When no write-permission solvers are configured and
  fallback is off, 100% of resulting handoffs produce an ERROR
  log line AND an `escalation_dispatch_unroutable` audit event.
  Zero silent drops.

- **SC-205**: Duplicate handoff events (same handoff event id
  observed twice) produce exactly one dispatch. Grouping the
  dispatch-tracking rows by handoff event id returns at most one
  row per key.

- **SC-206**: When a dispute's `lifecycle_state` flips to
  `resolved` before Phase 4's examination, 100% of such
  handoffs produce an `escalation_superseded` audit event and
  zero DMs. An operator sampling the audit trail finds no cases
  of "DM sent after resolved".

- **SC-207**: Phase 1/2 and Phase 3 behavior is unchanged. A
  regression test-suite run against a pre-Phase-4 snapshot and
  the post-Phase-4 snapshot produces identical outputs for every
  Phase 1/2 and Phase 3 test.

- **SC-208**: When every recipient of a dispatch fails, the
  resulting `escalation_dispatches` row carries
  `status = 'send_failed'` AND the `notifications` table carries
  one `status = 'failed'` row per attempted recipient. A single
  "which dispatches reached nobody?" operator query against
  `escalation_dispatches` alone returns the correct set — no
  JOIN against `notifications` required.

## Assumptions

- **Solver permission values are `read` and `write` only**, as
  already encoded in the existing `SolverConfig.permission`
  enum. Any future granular permission model (admin vs write, or
  scoped-write) is out of scope for Phase 4 v1.

- **The existing Phase 1/2 notifier is the single delivery
  mechanism.** Phase 4 does not introduce a new transport, a new
  relay connection, or a different cryptographic channel. The
  existing notifications-audit pathway (per-recipient sent / failed
  row) captures delivery outcomes.

- **`HandoffPackage` is already a stable serialized shape** in the
  audit table. Phase 3's escalation module writes every field, and
  omits the optional session id key when no session was opened (the
  FR-122 dispute-scoped shape). Phase 4 treats "session id key
  absent" and "session id value is null" as equivalent, and does
  not request format changes on Phase 3's side.

- **Operators are expected to monitor ERROR-level log lines.** The
  spec relies on ERROR logging as the primary operator-alert
  channel for unroutable / malformed / orphan cases. Promoting any
  of those to a separate alert transport (email, Slack, paging) is
  out of scope.

- **`fallback_to_all_solvers` defaults to `false`.** The safer
  default is "fail loud and visible" rather than "silently
  broadcast to read-only solvers". Operators who run read-only
  deployments intentionally must opt in.

- **Phase 4 runs in the same process as Phases 1/2/3**, sharing the
  same database handle. A standalone Phase 4 daemon talking to the
  same database is not in scope for v1.

- **The 60-second SC-201 target assumes a healthy relay and
  reachable solvers.** Network-level outages are captured by the
  `notifications.status = failed` entries; they do not count
  against SC-201.

- **Phase 4 is independent of Phase 3's engine-tick cadence.** The
  two loops share no state beyond the read-only audit table. A
  future change to Phase 3's tick interval, retry discipline, or
  reasoning-health gate has no effect on Phase 4.
