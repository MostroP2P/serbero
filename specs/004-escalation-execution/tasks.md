---

description: "Task list for Phase 4 — Escalation Execution Surface"
---

# Tasks: Phase 4 — Escalation Execution Surface

**Input**: Design documents from `/specs/004-escalation-execution/`
**Prerequisites**: plan.md, spec.md, research.md, data-model.md, contracts/, quickstart.md

**Tests**: Included. The spec explicitly requests unit, integration, and edge-case tests. The repo's established discipline is integration-test-alongside-implementation (not strict TDD-first); each user-story phase lists the test task adjacent to the implementation it pins.

**Organization**: Tasks are grouped by user story (spec.md §User Scenarios). Each user-story phase is independently testable and delivers a coherent slice of Phase 4 value.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies on incomplete tasks)
- **[Story]**: Maps to spec.md's user stories (US1, US2, US3). Setup / Foundational / Polish phases carry no story label.
- Every task includes an absolute-style repo path (relative to repo root).

## Path Conventions

Single-project Rust daemon:

- Source under `src/`
- Integration tests under `tests/`
- Specs under `specs/004-escalation-execution/`

---

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: Schema migration and new-module scaffolding that every later phase depends on.

- [X] T001 Add migration v5 to `src/db/migrations.rs`: create table `escalation_dispatches` with columns per `specs/004-escalation-execution/data-model.md` (dispatch_id TEXT PRIMARY KEY, dispute_id TEXT NOT NULL FK, session_id TEXT NULL FK, handoff_event_id INTEGER NOT NULL FK, target_solver TEXT NOT NULL, dispatched_at INTEGER NOT NULL, created_at INTEGER NOT NULL, status TEXT NOT NULL DEFAULT 'dispatched' with CHECK constraint, fallback_broadcast INTEGER NOT NULL DEFAULT 0). Create both indexes (idx_escalation_dispatches_dispute, idx_escalation_dispatches_handoff). Bump `schema_version.version` to 5 and `CURRENT_SCHEMA_VERSION` in the test module to 5 to match. Wire a new `apply_v5` function alongside the existing `apply_v1..apply_v4` chain (`main`'s `apply_v4` owns the Phase 11 mid-session columns — do NOT edit or renumber it). Add assertion in the existing migration-table round-trip test that v5 creates the new table plus indexes.

- [X] T002 [P] Create `src/db/escalation_dispatches.rs` with the `EscalationDispatch` struct (fields matching the table) and the `DispatchStatus` enum (`Dispatched` / `SendFailed`). Implement `Display` and `FromStr` on `DispatchStatus` following the `MediationSessionState` / `ClassificationLabel` idiom (in-scope `FromStr` + `Error` names, snake_case tokens). Add inline unit tests for round-trip display/parse plus the unknown-token error path.

- [X] T003 [P] Add `EscalationConfig` to `src/models/config.rs` with fields `enabled: bool` (default false), `dispatch_interval_seconds: u64` (default 30, positive-integer validation that errors loudly on zero), `fallback_to_all_solvers: bool` (default false). Wire into the top-level `Config` struct as `pub escalation: EscalationConfig`. Make the section optional in TOML so older `config.toml` files keep parsing; when absent the defaults apply. Add inline tests: all-defaults load, explicit-enable load, zero-interval load returns ConfigError.

**Checkpoint**: schema v5 is applied, `escalation_dispatches` CRUD types exist, `Config` carries an `escalation` section. No behavior change yet.

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: The four audit-event kinds, their typed constructors, the dispatch-tracking CRUD helpers, and the empty-loop dispatcher task wired into the daemon. MUST complete before any user-story work begins.

**⚠️ CRITICAL**: user-story phases (P1/P2/P3) all depend on the audit writers and the module scaffolding below.

- [X] T004 Extend `MediationEventKind` in `src/db/mediation_events.rs` with four variants — `EscalationDispatched`, `EscalationSuperseded`, `EscalationDispatchUnroutable`, `EscalationDispatchParseFailed`. Update `as_str()`, the `Display` impl, and the existing `FromStr`/round-trip tests so every new variant round-trips against the snake_case wire token. Keep the existing variant ordering (append, do not reorder).

- [X] T005 Add four typed constructors to `src/db/mediation_events.rs`, one per new kind: `record_escalation_dispatched`, `record_escalation_superseded`, `record_escalation_dispatch_unroutable`, `record_escalation_dispatch_parse_failed`. Each takes `session_id: Option<&str>`, `dispute_id: &str`, the kind-specific payload fields, `prompt_bundle_id: Option<&str>`, `policy_hash: Option<&str>`, `occurred_at: i64`. Payload JSON shapes MUST match `contracts/audit-events.md` verbatim. Each constructor reuses the existing `record_event` helper so a payload-formatting bug never reaches SQL.

- [X] T006 Implement CRUD helpers in `src/db/escalation_dispatches.rs`: `insert_dispatch(tx, &EscalationDispatch) -> Result<()>`, `find_dispatch_by_handoff_event_id(conn, handoff_event_id) -> Result<Option<EscalationDispatch>>` (the dedup probe for FR-203/SC-205), `list_pending_handoffs(conn, limit) -> Result<Vec<PendingHandoff>>` implementing the single `LEFT JOIN` scan from research.md Decision 2 (join `mediation_events` on `handoff_prepared` against `escalation_dispatches.handoff_event_id IS NULL`). Add inline tests covering insert/query round-trip + the dedup filter.

- [X] T007 Add `pub mod escalation;` to `src/lib.rs`. Create `src/escalation/mod.rs` with the public entry `pub async fn run_dispatcher(conn, client, serbero_keys, solvers, cfg: EscalationConfig) -> !` stubbed as a `tokio::time::interval(cfg.dispatch_interval_seconds)` loop that currently only logs "Phase 4 dispatcher tick" at debug level. Add `mod consumer; mod router; mod dispatcher; mod tracker;` (empty module files). Document the loop's safety-net vs. event-driven discipline in the module docstring (follow the `mediation::run_engine` shape).

- [X] T008 Wire the dispatcher into `src/daemon.rs`: after the Phase 3 engine spawn block, if `config.escalation.enabled` is `true`, spawn `escalation::run_dispatcher(conn.clone(), client.clone(), serbero_keys.clone(), solvers.clone(), config.escalation.clone())` on a `tokio::spawn` guarded by the existing shutdown signal. Log one info! on spawn (`phase4_dispatcher_enabled dispatch_interval_seconds=<n> write_solver_count=<m>`) and one info! on skip (`phase4_dispatcher_disabled`).

- [X] T009 [P] Append the `[escalation]` section to `config.sample.toml` with the three keys and explanatory comments drawn from `contracts/config.md`.

- [X] T010 [P] Foundational smoke test: build the daemon with `[escalation].enabled = true` and a mock relay, assert that the dispatcher task spawns, loops on cadence, and does not touch any Phase 1/2/3 table when there are no pending handoffs. This pins FR-217/FR-218 / SC-207 before any implementation adds behavior. Live under `tests/phase4_foundational.rs`.

**Checkpoint**: dispatcher task spawns, loops, and is inert. Every new audit kind has a typed constructor. Every user-story phase below can now proceed in parallel.

---

## Phase 3: User Story 1 — Write-Permission Solver Receives Structured Escalation (Priority: P1) 🎯 MVP

**Goal**: a Phase 3 `handoff_prepared` event produces one structured `escalation_handoff/v1` DM to a write-permission solver, plus one `escalation_dispatches` row, plus one `escalation_dispatched` audit event. Covers every acceptance scenario of spec.md §US1 (session-scoped, dispute-scoped, targeted assigned-solver, read-permission-assigned fallback).

**Independent Test**: `tests/phase4_dispatch.rs`. Seed one `handoff_prepared` row; configure one write-permission solver; run `run_dispatcher` for a single cycle; assert DM arrives at the solver, `escalation_dispatches.status = 'dispatched'`, `escalation_dispatched` audit row present, and the DM body starts with `escalation_handoff/v1`.

### Implementation for User Story 1

- [ ] T011 [P] [US1] Implement `src/escalation/consumer.rs`: `pub async fn scan_pending(conn, limit) -> Result<Vec<PendingHandoff>>` that calls `db::escalation_dispatches::list_pending_handoffs`. Add inline unit tests that seed mixed handoff + non-handoff events and assert only the pending ones come back.

- [ ] T012 [P] [US1] Implement `src/escalation/router.rs`: `pub fn resolve_recipients(solvers: &[SolverConfig], assigned_solver: Option<&str>, fallback_to_all: bool) -> Recipients` where `Recipients` is an enum with variants `Targeted(String)`, `Broadcast { pubkeys: Vec<String>, via_fallback: bool }`, and `Unroutable`. Implement the FR-202 rules in order (targeted write-perm assignment > broadcast write-perm > fallback to all > unroutable). Add inline unit tests for each branch: targeted-write, write-assigned-but-not-configured, read-assigned, no-assignment + 2 write solvers, zero write solvers + fallback-on, zero write solvers + fallback-off.

- [ ] T013 [US1] Implement `src/escalation/dispatcher.rs`: `pub fn build_dm_body(pkg: &HandoffPackage) -> String` producing the exact `escalation_handoff/v1` shape from `contracts/dm-payload.md` (version prefix line, Dispute/Session/Trigger headers, two-sentence human summary mentioning "Serbero's mediation assistance system", inline single-line JSON payload line). Handle the session-id header for both the `Some` and `None` cases. Add inline unit tests that assert the first line is exactly `escalation_handoff/v1`, the body contains the dispute id and trigger, the JSON line round-trips back to a valid `HandoffPackage`, and — FR-120 — the body does NOT contain any rationale text (only rationale_refs ids).

- [ ] T014 [US1] Extend `src/escalation/dispatcher.rs` with `pub async fn send_to_recipients(client, serbero_keys, recipients: &[String], body: &str) -> DispatchOutcome` that iterates recipients, calls the existing `nostr::send_gift_wrap_notification` once per recipient, records each per-recipient `notifications` row with status `sent`/`failed`, and returns `AllSucceeded`/`AllFailed`/`PartialSuccess`. Reuses the existing notifier machinery — no new transport.

- [ ] T015 [US1] Implement `src/escalation/tracker.rs`: `pub async fn record_successful_dispatch(conn, handoff: &PendingHandoff, pkg: &HandoffPackage, outcome: DispatchOutcome, fallback_broadcast: bool) -> Result<DispatchStatus>` that, in a single transaction: (a) derives `status` per FR-211 (AllSucceeded/PartialSuccess → Dispatched; AllFailed → SendFailed); (b) inserts the `escalation_dispatches` row; (c) appends an `escalation_dispatched` event via `db::mediation_events::record_escalation_dispatched` with the payload shape from `contracts/audit-events.md`. Uses `uuid::Uuid::new_v4()` for `dispatch_id`.

- [ ] T016 [US1] Wire the full dispatch cycle inside `src/escalation/mod.rs::run_dispatcher`: per interval tick, call `consumer::scan_pending`; for each pending handoff, deserialize the payload to `HandoffPackage`, look up the dispute's `assigned_solver`, call `router::resolve_recipients`, branch on the `Recipients` variant — `Targeted`/`Broadcast` → call `dispatcher::send_to_recipients` → call `tracker::record_successful_dispatch`. Leave the `Unroutable` and supersession branches as TODO stubs (US3 and US2 fill them in).

- [ ] T017 [US1] Integration test `tests/phase4_dispatch.rs`. Sub-tests:
  - `targeted_write_solver_receives_dm` — one session-scoped handoff + `assigned_solver` matches a configured write solver → one DM to that solver, `target_solver` column = that pubkey, `fallback_broadcast = 0`, `status = 'dispatched'`.
  - `broadcast_to_all_write_solvers_when_assigned_unknown` — handoff + `assigned_solver = NULL` + two configured write solvers → both receive the DM, `target_solver` is the comma-joined list.
  - `read_permission_assignment_falls_back_to_broadcast` — `assigned_solver` points at a Read-permission solver → DM broadcasts to Write solvers only, not to the Read one.
  - `dispute_scoped_handoff_emits_none_session_marker` — FR-122 handoff (`session_id = NULL`) → DM body contains literal `Session: <none — dispute-scoped handoff>`.
  - `dispatch_audit_row_paired_with_tracking_row` — one `escalation_dispatches` row + one `escalation_dispatched` audit row, same `dispatch_id`, no mismatch (SC-203 invariant).

- [ ] T018 [US1] Add `duplicate_handoff_deduplicated` sub-test to `tests/phase4_dispatch.rs`: seed the same `handoff_prepared` event position twice (simulate relay replay via two consumer-tick runs over the same row); assert only one `escalation_dispatches` row and one `escalation_dispatched` audit row exist for that `handoff_event_id`. Covers SC-205 / FR-203.

**Checkpoint**: US1 is shippable as the MVP. Running `cargo test --test phase4_dispatch` passes all five happy-path sub-tests plus the dedup sub-test.

---

## Phase 4: User Story 2 — External Resolution Supersedes an Undispatched Escalation (Priority: P2)

**Goal**: when a dispute's `lifecycle_state` flips to `resolved` before the dispatcher examines its handoff, no DM is sent, no dispatch row is written, and an `escalation_superseded` audit event records the skip.

**Independent Test**: `tests/phase4_supersession.rs`. Seed a `handoff_prepared` row for a dispute already at `lifecycle_state = 'resolved'`; run one dispatcher cycle; assert zero DMs arrive, zero `escalation_dispatches` rows, exactly one `escalation_superseded` audit row with `reason = "dispute_already_resolved"`.

### Implementation for User Story 2

- [ ] T019 [US2] Add a supersession gate inside `src/escalation/mod.rs::run_dispatcher`, between the `HandoffPackage` deserialization and the `resolve_recipients` call: read `disputes.lifecycle_state` for the handoff's dispute_id; if it equals `LifecycleState::Resolved`, call `tracker::record_supersession` (new helper, T020) and skip to the next pending handoff. Don't consume the handoff event in the dispatch table — `escalation_superseded` is a non-event from the dispatch-table's perspective (FR-212), and the handoff stays unconsumed so a future clarifying policy change can re-process it.

- [ ] T020 [US2] Add `pub async fn record_supersession(conn, handoff: &PendingHandoff) -> Result<()>` to `src/escalation/tracker.rs`. Calls `db::mediation_events::record_escalation_superseded` with the payload shape from `contracts/audit-events.md` (`dispute_id`, `handoff_event_id`, `reason: "dispute_already_resolved"`). Session-id scoping copies the upstream `handoff_prepared` row. No `escalation_dispatches` insert.

- [ ] T021 [US2] Integration test `tests/phase4_supersession.rs`. Sub-tests:
  - `resolved_dispute_is_skipped_no_dm` — seed dispute with `lifecycle_state = 'resolved'` + handoff_prepared → run one cycle → zero DMs, zero dispatch rows, exactly one `escalation_superseded` audit row.
  - `dispute_resolving_between_scan_and_send_still_dispatches` — race case: dispute is open at scan time, flips to resolved before send completes → the dispatch still happens (spec's US2 acceptance scenario 2 — Phase 4 does NOT attempt to recall). Drive by mutating `lifecycle_state` after scan but before `send_to_recipients` (use a hook point or directly in the test with a controlled interval).
  - `supersession_does_not_mark_handoff_consumed` — after a supersession, the handoff row stays available for a future scan (consumer still sees it as pending because no `escalation_dispatches` row was written). Assert by running a second cycle on the same DB and observing the supersession row count goes to 2 (because the outer dispute is still resolved, it supersedes again). Documents the "idempotent supersession" shape.

**Checkpoint**: US2 ships. A replay-friendly audit trail distinguishes supersessions from dispatches cleanly.

---

## Phase 5: User Story 3 — No Write-Permission Solvers Configured (Priority: P3)

**Goal**: when zero write-permission solvers are configured, Phase 4 either (a) records an ERROR and an `escalation_dispatch_unroutable` audit row if fallback is off, or (b) broadcasts to every configured solver and records the dispatch with `fallback_broadcast = 1` if fallback is on. No silent drops.

**Independent Test**: `tests/phase4_no_write_solvers.rs`. Two sub-tests covering both fallback branches.

### Implementation for User Story 3

- [ ] T022 [US3] Handle `Recipients::Unroutable` inside `src/escalation/mod.rs::run_dispatcher`: call `tracker::record_unroutable` (new helper, T023), emit one `error!` log line naming the dispute id plus the config hint `fallback_to_all_solvers`. Do NOT write an `escalation_dispatches` row; the handoff stays pending so a future config change can pick it up (FR-213).

- [ ] T023 [US3] Add `pub async fn record_unroutable(conn, handoff: &PendingHandoff, configured_solver_count: usize) -> Result<()>` to `src/escalation/tracker.rs`. Calls `db::mediation_events::record_escalation_dispatch_unroutable` with the payload shape from `contracts/audit-events.md`.

- [ ] T024 [US3] Extend the dispatch wiring in `src/escalation/mod.rs` (T016) so the `Recipients::Broadcast { via_fallback: true }` arm sets `fallback_broadcast = true` when calling `tracker::record_successful_dispatch`. Audit-event payload's `fallback_broadcast` field flows through T015.

- [ ] T025 [US3] Integration test `tests/phase4_no_write_solvers.rs`. Sub-tests:
  - `zero_write_solvers_fallback_off_records_unroutable` — config has only Read solvers, `fallback_to_all_solvers = false`, one handoff → zero DMs, zero dispatch rows, exactly one `escalation_dispatch_unroutable` audit row, the test's tracing capture contains an ERROR-level line naming the dispute id.
  - `zero_write_solvers_fallback_on_broadcasts_to_everyone` — same config but `fallback_to_all_solvers = true` → DMs go to every configured Read solver, one dispatch row with `fallback_broadcast = 1` and `target_solver` = comma-joined list of every configured pubkey, the audit payload's `fallback_broadcast` key is `true`.
  - `unroutable_handoff_picked_up_after_config_change` — run one cycle with fallback off and zero Write solvers (produces the unroutable row); swap the config to add a Write solver; run another cycle on the same DB; the handoff now dispatches normally (`status = 'dispatched'`). Pins FR-213's "stays unconsumed" guarantee.

**Checkpoint**: US3 ships. No silent drops; operators can tell the two failure modes apart from audit alone.

---

## Phase 6: Polish & Cross-Cutting Concerns

**Purpose**: edge-case coverage (send_failed status, parse_failed branches), quality gates, and documentation.

- [ ] T026 Extend `src/escalation/dispatcher.rs::send_to_recipients` with correct `DispatchOutcome` classification: every recipient must record a `notifications` row before returning. If every recipient failed, return `DispatchOutcome::AllFailed` so `tracker::record_successful_dispatch` writes `status = 'send_failed'` per FR-211. Make sure the existing SC-208 shape holds.

- [ ] T027 Integration test `tests/phase4_send_failure.rs`. Simulate all-recipient failure by pointing the configured solver at an unreachable pubkey (or by using a mock relay that rejects writes to a given pubkey). Assert:
  - Exactly one `escalation_dispatches` row with `status = 'send_failed'`.
  - One `notifications` row per attempted recipient with `status = 'failed'`.
  - Exactly one `escalation_dispatched` audit row (the audit kind does not change with send outcome — the `status` payload field does).
  - `SELECT * FROM escalation_dispatches WHERE status = 'send_failed'` returns the row without a JOIN against `notifications` (SC-208 invariant).

- [ ] T028 Implement parse-failed handling in `src/escalation/mod.rs::run_dispatcher`: wrap the `HandoffPackage` deserialize call in `match serde_json::from_str::<HandoffPackage>(&handoff.payload_json)` — on `Err`, call `tracker::record_parse_failed` (new helper, T029) with `reason = "deserialize_failed"` and the parser error detail, then continue. Add a separate dispute-row lookup to detect orphan references: if the deserialize succeeds but `db::disputes::get_dispute` returns `Ok(None)`, call the same helper with `reason = "orphan_dispute_reference"`. Both branches mark the handoff consumed so the queue moves forward.

- [ ] T029 Add `pub async fn record_parse_failed(conn, handoff: &PendingHandoff, reason: &str, detail: &str) -> Result<()>` to `src/escalation/tracker.rs`. Calls `db::mediation_events::record_escalation_dispatch_parse_failed` with the payload from `contracts/audit-events.md`. The parse_failed path needs a "mark consumed" effect to prevent queue poisoning; implement by inserting a sentinel `escalation_dispatches` row? No — the data-model says these kinds do NOT write a dispatch row. Instead, keep the dedup query honest: extend `list_pending_handoffs` (T006) to exclude handoffs that already have any `escalation_dispatch_parse_failed` OR `escalation_dispatched` audit row for the same `handoff_event_id`.

- [ ] T030 Integration test `tests/phase4_dedup_and_parse_failure.rs`. Sub-tests:
  - `malformed_payload_records_parse_failed_and_moves_on` — seed a `handoff_prepared` row with invalid JSON payload → one cycle → one `escalation_dispatch_parse_failed` audit row (reason = `deserialize_failed`, detail contains the parser error), zero dispatch rows, zero DMs. Second cycle over the same DB does NOT re-emit the audit row.
  - `orphan_dispute_reference_records_parse_failed` — handoff whose `dispute_id` has no row in `disputes` → one `escalation_dispatch_parse_failed` audit row (reason = `orphan_dispute_reference`), zero DMs, zero dispatch rows.
  - `poisoning_is_prevented` — start with a malformed payload (parse_failed fires), add a valid payload on the next tick → only the valid one dispatches; the malformed one stays consumed-via-audit and is not retried.

- [ ] T031 [P] Run `cargo clippy --all-targets --all-features -- -D warnings` and `cargo fmt --all -- --check`. Expect new lints around the reshaped `Config` struct and the newly-introduced dispatcher loop; fix findings without `#[allow]` attributes.

- [ ] T032 [P] Run the full test suite (`cargo test --all`). Ensure all pre-existing Phase 1/2/3 tests still pass and the new Phase 4 tests pass. SC-207 regression proof.

- [ ] T033 [P] Update `README.md` if it carries a phase-status section; otherwise leave. CLAUDE.md was already updated by the plan step (verify via git diff that the Phase 4 row is present).

- [ ] T034 Validate `specs/004-escalation-execution/quickstart.md` manually against a local `serbero` build: walk through the US1/US2/US3 steps, confirm the SQL inspection queries return the expected shapes. If any command produces an unexpected result, fix the quickstart or the implementation before closing the task.

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)**: no dependencies; the three tasks can start immediately. T002 and T003 are parallel-safe with each other; T001 must land first because later schema tests depend on v5.
- **Foundational (Phase 2)**: depends on all Phase 1 tasks. T004 and T005 share a file (`src/db/mediation_events.rs`) and must run sequentially. T006 depends on T001 (the table must exist). T007–T010 depend on T004/T005/T006. T009 and T010 are parallel-safe.
- **User Story 1 (Phase 3 / P1)**: depends on all of Phase 2. Within US1, T011 and T012 are parallel-safe (different files). T013 and T014 share `dispatcher.rs`. T015 depends on T014's `DispatchOutcome` enum. T016 integrates T011–T015. T017 and T018 land after T016.
- **User Story 2 (Phase 4 / P2)**: depends on Phase 2 and on US1's dispatch wiring (T016) — the supersession gate slots in before the dispatch call.
- **User Story 3 (Phase 5 / P3)**: depends on Phase 2 and on US1's dispatch wiring (T016) — the unroutable branch slots into the same match statement.
- **Polish (Phase 6)**: T026–T030 depend on US1–US3 implementation. T031–T034 depend on T026–T030.

### User Story Dependencies

- **US1 (P1)** has no dependency on other stories — it is the MVP. Shipping US1 alone produces a working Phase 4 for the happy-path case.
- **US2 (P2)** depends on US1's dispatch loop being in place (the supersession gate lives inside that loop). It does NOT depend on US3.
- **US3 (P3)** depends on US1's dispatch loop (the unroutable branch is one arm of the recipient match). It does NOT depend on US2.

US2 and US3 can land in either order after US1; they touch different code paths inside `src/escalation/mod.rs`.

### Within Each User Story

- Unit tests live inline in the source files (Phase 3 pattern) and exercise pure functions (router, body builder) as soon as they exist.
- Integration tests (one `.rs` file per phase) run last within the story and validate end-to-end shape against the mock relay.
- Audit events are recorded in the same transaction as state changes so partial-commit races are impossible.

### Parallel Opportunities

- Phase 1: T002 ∥ T003 (different files, no shared type).
- Phase 2: T009 ∥ T010 (docs + smoke test).
- US1: T011 ∥ T012 (different files), T017 and T018 can be authored in parallel once T016 lands.
- US2 and US3 can be staffed by different developers simultaneously after US1 ships.
- Polish: T031, T032, T033 are fully parallel.

---

## Parallel Example: User Story 1

```bash
# After Phase 2 completes, kick off US1 in parallel slots:
Task: "T011 [P] [US1] Implement src/escalation/consumer.rs scan_pending + inline tests"
Task: "T012 [P] [US1] Implement src/escalation/router.rs resolve_recipients + inline tests"

# After T013, T014, T015, T016 serially land, parallelize:
Task: "T017 [US1] Integration test tests/phase4_dispatch.rs (targeted, broadcast, session-scoped, dispute-scoped, pairing)"
Task: "T018 [US1] Integration test tests/phase4_dispatch.rs duplicate_handoff_deduplicated sub-test"
```

---

## Implementation Strategy

### MVP First (User Story 1 Only)

1. Complete Phase 1 (Setup): migration v5, escalation_dispatches CRUD types, EscalationConfig.
2. Complete Phase 2 (Foundational): audit kinds + typed constructors, dispatch CRUD helpers, empty-loop dispatcher spawned from daemon.rs.
3. Complete Phase 3 (US1): full dispatch path — consumer, router, dispatcher, tracker, integration test.
4. **STOP and VALIDATE**: `cargo test --test phase4_dispatch` passes all six sub-tests; manually run the quickstart US1 walkthrough against a local MockRelay smoke-test. Phase 4 now ships the MVP.

### Incremental Delivery

1. MVP (US1) — ships the happy-path dispatcher. Operators with a write-permission solver see every Phase 3 escalation arrive as a structured DM.
2. + US2 — ships supersession. Stale handoffs stop producing noise for resolved disputes.
3. + US3 — ships unroutable + fallback. Deployments without a write-permission solver either get loud errors or an explicit broadcast.
4. + Polish — ships send_failed accuracy, parse_failed edge cases, clippy clean, quickstart validated.

Each slice is independently testable and independently shippable; the constitution's Graceful Degradation (Principle VII) is preserved at every step because `[escalation].enabled = false` disables the entire feature.

### Parallel Team Strategy

With multiple developers:

1. Developer A + Developer B team up on Phases 1 + 2 (schema + scaffolding). About half a day of effort.
2. After Phase 2: Developer A takes US1 (the biggest slice). Developer B takes US2 + US3 in parallel (they touch different arms of the same match).
3. Polish is split three ways (send_failed, parse_failed, clippy/fmt/tests).

---

## Notes

- [P] tasks = different files, no pending dependencies.
- [Story] label maps each task to spec.md's user stories for traceability.
- Every task states an exact file path so execution is unambiguous.
- The spec's FR-217 ("no modifications to Phase 1/2/3 tables") is the dominant non-negotiable constraint; every task above touches either new files, new rows, or append-only event writes.
- Commit cadence: one commit per user-story phase (or per sub-task if the phase is large) to keep bisect granular.
- Do not skip the supersession gate (T019) on grounds of "the race is rare" — the whole point of US2 is that the supersession path is observable.
