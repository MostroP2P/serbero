# Implementation Plan: Phase 4 — Escalation Execution Surface

**Branch**: `004-escalation-execution` | **Date**: 2026-04-22 | **Spec**: [spec.md](./spec.md)
**Input**: Feature specification from `/specs/004-escalation-execution/spec.md`

## Summary

Add a fourth pipeline phase to the Serbero daemon: an "escalation
dispatcher" background task that consumes Phase 3's
`handoff_prepared` audit events, routes them to solvers with
`write` permission, and delivers a structured DM containing both a
human-readable summary and a machine-readable representation of
the existing `HandoffPackage`. A new table
`escalation_dispatches` tracks consumption state (one row per
attempt that reached the send step); four new `mediation_events`
kinds (`escalation_dispatched`, `escalation_superseded`,
`escalation_dispatch_unroutable`,
`escalation_dispatch_parse_failed`) record the audit trail. A new
`[escalation]` config section exposes an enable flag, dispatch
interval, and a fallback-to-all-solvers knob.

The dispatcher is strictly additive: it reads Phase 1/2/3 state,
writes only to its own table plus `mediation_events`, never issues
`TakeDispute`, and does not retry / ack / re-escalate. It runs in
the same process as Phases 1/2/3 but its loop is independent of
the Phase 3 engine tick — the two share no state beyond the
audit tables.

## Technical Context

**Language/Version**: Rust stable, edition 2021 (same toolchain as Phases 1/2/3).
**Primary Dependencies**: `nostr-sdk 0.44.1` (gift-wrap transport), `mostro-core 0.8.4`, `rusqlite` (bundled, now via migration v4), `tokio` (existing runtime), `serde` + `serde_json` (HandoffPackage round-trip + DM body), `uuid` (dispatch_id v4), `tracing`, `thiserror`. No new crate pulls.
**Storage**: SQLite. One new table (`escalation_dispatches`), four new `mediation_events.kind` values. Migration v4 extends the existing migrations chain in `src/db/migrations.rs`.
**Testing**: `cargo test` + the existing integration-test harness (`nostr-relay-builder::MockRelay`, `common::SolverListener`, in-memory rusqlite). No new test dependency.
**Target Platform**: Linux server (the parent daemon is already Linux-targeted; no platform-specific code added).
**Project Type**: Single-binary Rust daemon — the existing `serbero` binary gains one new background task and one new module tree.
**Performance Goals**: SC-201 — 95% of handoffs reach a write-permission solver within 60 s at the default 30 s dispatch interval. Per-cycle DB read is a single indexed scan (`mediation_events LEFT JOIN escalation_dispatches ON handoff_event_id`); no cross-table joins on the hot path beyond that.
**Constraints**: FR-217 — must NOT modify `disputes`, `mediation_sessions`, or any Phase 1/2/3 table other than appending to `mediation_events`. FR-218 — must NOT share state with the Phase 3 engine tick. FR-207 — DM body identifies Serbero as an assistance system (not a Mostro admin). FR-209 carries forward Fund Isolation First: no `TakeDispute` / `admin-settle` / `admin-cancel`.
**Scale/Scope**: Same scale as Phase 3. Handoff throughput is bounded by the rate of Phase 3 escalations (observed experimentally in the low units per hour on typical Mostro deployments); the dispatcher is trivially able to absorb that rate with a 30 s cycle.

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

All 13 Serbero-constitution principles are satisfied by the Phase 4
spec; no violations to track. Mapping below is the evidence
examined at this gate:

| # | Principle | Compliance source |
|---|-----------|-------------------|
| I | Fund Isolation First | FR-209 — Phase 4 MUST NOT issue TakeDispute / admin-settle / admin-cancel / close disputes. No credential expansion. |
| II | Protocol-Enforced Security Boundaries | No AI on the dispatch path. Routing is a deterministic filter over `SolverConfig.permission`. |
| III | Human Final Authority | Phase 4's *purpose* is to hand off to a human write-permission solver. Nothing in the flow substitutes for the human decision. |
| IV | Operator Notification Is a Core Responsibility | Phase 4 IS the structured operator-notification surface for escalations; FR-213 forbids silent drops. |
| V | Assistance Without Authority | FR-207 — DM body identifies Serbero as an assistance system. |
| VI | Auditability by Design | FR-211..FR-214 — every dispatch, supersession, unroutable, and parse-failed case writes an audit row. Dispatch-tracking row is keyed by dispatch_id and handoff_event_id for replay-friendly reconciliation. |
| VII | Graceful Degradation | FR-216 — `[escalation].enabled = false` disables Phase 4 entirely without touching Phase 1/2/3. SC-207 empirical check. |
| VIII | Privacy by Default | FR-206 — DM body MUST NOT contain raw rationale text, only content-hash rationale ids. Broadcast path is explicit and opt-in; targeted path is the default when an assigned write-perm solver exists. |
| IX | Nostr-Native Coordination | Delivery goes through the existing Phase 1/2 notifier's gift-wrap path; no new transport. |
| X | Portable Reasoning Backends | N/A — Phase 4 calls no reasoning provider. |
| XI | Incremental Scope and Clear Boundaries | Spec's "What Phase 4 Does NOT Do" list is explicit: no ack tracking, no retry, no re-escalation, no filter beyond write-permission. |
| XII | Honest System Behavior | FR-208 supersession fires rather than sending a stale DM; FR-213 and FR-214 surface uncertainty rather than silently dropping. |
| XIII | Mostro Compatibility | The dispatcher asks the solver to run `TakeDispute` on their Mostro instance — authority stays in Mostro; Serbero only assists with context. |

**Decision**: GATE PASSES. No Complexity Tracking entries needed.

**Post-design re-evaluation (after Phase 1 artifacts)**: data-model.md
(one new table + four enum variants, zero structural changes to
Phase 1/2/3 tables), contracts/dm-payload.md (FR-206 no-rationale-
text + FR-207 assistance-identity explicitly pinned), contracts/
audit-events.md (separate event kinds per operator-actionable
state), contracts/config.md (safe defaults: enabled=false,
fallback_to_all_solvers=false), and quickstart.md (SC-207 admin-
action zero-count verification) collectively introduce no new
constitutional violations. Gate still PASSES.

## Project Structure

### Documentation (this feature)

```text
specs/004-escalation-execution/
├── plan.md              # This file (/speckit.plan command output)
├── research.md          # Phase 0 output (/speckit.plan command)
├── data-model.md        # Phase 1 output (/speckit.plan command)
├── quickstart.md        # Phase 1 output (/speckit.plan command)
├── contracts/           # Phase 1 output (/speckit.plan command)
│   ├── dm-payload.md    # Solver-facing `escalation_handoff/v1` DM shape
│   ├── audit-events.md  # Four new mediation_events kinds + payloads
│   └── config.md        # `[escalation]` TOML section schema
├── checklists/
│   └── requirements.md  # Spec quality checklist (/speckit.specify output)
└── tasks.md             # Phase 2 output (/speckit.tasks command - NOT created here)
```

### Source Code (repository root)

Phase 4 adds one new top-level module under `src/` and one new table
via migration v4. All existing modules remain unchanged.

```text
src/
├── chat/                 # (existing; Phase 3 chat transport)
├── db/
│   ├── migrations.rs     # MODIFIED: add migration v4 (escalation_dispatches + new kinds)
│   ├── mediation_events.rs  # MODIFIED: extend MediationEventKind enum (+4 variants)
│   ├── disputes.rs       # (unchanged; Phase 4 only reads)
│   ├── mediation.rs      # (unchanged)
│   └── escalation_dispatches.rs  # NEW: CRUD for the new table
├── escalation/           # NEW Phase 4 module tree
│   ├── mod.rs            # Engine loop, public `run_dispatcher` entry
│   ├── consumer.rs       # Scans mediation_events for pending handoffs
│   ├── router.rs         # Filters SolverConfig by write permission (FR-202)
│   ├── dispatcher.rs     # Builds the DM body + calls the notifier
│   └── tracker.rs        # Writes escalation_dispatches + audit rows
├── handlers/             # (unchanged)
├── mediation/            # (unchanged — Phase 3's escalation::HandoffPackage is reused via `pub use`)
├── models/
│   ├── config.rs         # MODIFIED: add `EscalationConfig` struct + `[escalation]` section
│   └── (other files)     # (unchanged)
├── nostr/                # (unchanged)
├── reasoning/            # (unchanged)
├── prompts/              # (unchanged)
├── daemon.rs             # MODIFIED: conditionally spawn escalation::run_dispatcher when cfg.escalation.enabled
├── dispatcher.rs         # (existing event dispatcher; unchanged)
├── config.rs             # (pulls in EscalationConfig via the models layer)
├── error.rs              # MODIFIED: add `Escalation*` variants if needed (expected: none; reuses Db / Io)
├── lib.rs                # MODIFIED: `pub mod escalation;`
└── main.rs               # (unchanged; daemon.rs wires the task)

tests/
├── common/mod.rs         # (unchanged — existing MockRelay + SolverListener helpers are reused)
├── phase4_dispatch.rs                          # NEW: US1 happy path + targeted/broadcast routing
├── phase4_supersession.rs                      # NEW: US2 external-resolution supersession
├── phase4_no_write_solvers.rs                  # NEW: US3 unroutable + fallback behavior
├── phase4_dedup_and_parse_failure.rs           # NEW: edge cases (duplicate handoffs, orphan, malformed payload)
└── phase4_send_failure.rs                      # NEW: all-recipient failure → status = 'send_failed' (SC-208)
```

**Structure Decision**: The existing Phase 1/2/3 layout is a
single-project Rust daemon with feature-scoped module trees under
`src/`. Phase 4 follows that precedent: one new top-level module
`src/escalation/` broken into five small files mirroring the spec's
module structure section (mod/consumer/router/dispatcher/tracker),
plus one new `src/db/escalation_dispatches.rs` for the table's
CRUD helpers. Tests live as top-level integration files under
`tests/` following the same `phase3_*.rs` naming pattern used for
Phase 3.

## Complexity Tracking

> **Fill ONLY if Constitution Check has violations that must be justified**

No constitution violations. Table intentionally left empty.
