# Implementation Plan: Phased Dispute Coordination

**Branch**: `002-phased-dispute-coordination` | **Date**: 2026-04-16 | **Spec**: [spec.md](spec.md)
**Input**: Feature specification from `specs/002-phased-dispute-coordination/spec.md`

## Summary

Serbero is a Nostr-native dispute coordination daemon for the Mostro
ecosystem. It monitors Mostro dispute events on Nostr relays, notifies
registered solvers via encrypted gift-wrap messages, tracks dispute
lifecycle state, and surfaces coordination visibility through
notifications. The first implementation covers Phase 1 (always-on
listener + solver notification) and Phase 2 (intake tracking +
assignment visibility + re-notification).

## Technical Context

**Language/Version**: Rust (stable, edition 2021)
**Primary Dependencies**: nostr-sdk 0.44.1, mostro-core 0.8.4, rusqlite, tokio, serde, toml, tracing
**Storage**: SQLite via rusqlite (direct, no abstraction layer)
**Testing**: cargo test, with integration tests against a local relay
**Target Platform**: Linux server (single-instance daemon)
**Project Type**: Long-lived daemon / background service
**Performance Goals**: Detect and notify within 30 seconds of event publication
**Constraints**: Single instance, no multi-process coordination, SQLite only
**Scale/Scope**: Low dispute volume (tens to hundreds per day)

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

| Principle | Status | Verification |
|-----------|--------|--------------|
| I. Fund Isolation | PASS | Serbero has no code paths for `admin-settle`, `admin-cancel`, or fund movement. No signing keys for those actions. |
| II. Protocol-Enforced Security | PASS | Serbero operates with read-only observation of relay events. Mostro enforces all permission boundaries. |
| III. Human Final Authority | PASS | Phases 1-2 only notify and track. No autonomous resolution. Later phases escalate to human operators. |
| IV. Operator Notification | PASS | Core responsibility. Phase 1 is entirely dedicated to this. |
| V. Assistance Without Authority | PASS | No user-facing communication in Phases 1-2. Later phases identify as assistance system. |
| VI. Auditability | PASS | All notifications and state transitions recorded in SQLite. |
| VII. Graceful Degradation | PASS | Mostro operates independently. Relay disconnect = reconnect with backoff. SQLite failure = halt notifications. |
| VIII. Privacy | PASS | Notifications contain initiator role (buyer/seller), not pubkey. Minimum necessary info. |
| IX. Nostr-Native | PASS | All communication via gift-wrap encrypted messages on Nostr. |
| X. Portable Backends | PASS | Reasoning backend boundary described as a planning/contracts artifact only; no trait or module is scaffolded into the crate until Phase 5 actually implements it. |
| XI. Incremental Scope | PASS | Phased implementation. Phase 1 → Phase 2 → future phases via explicit specs. |
| XII. Honest Behavior | PASS | No classification or mediation in Phases 1-2. Later phases surface uncertainty. |
| XIII. Mostro Compatibility | PASS | Serbero reads events, never writes dispute-closing actions. Clear boundary. |

**Gate result**: ALL PASS. No violations to justify.

## Project Structure

### Documentation (this feature)

```text
specs/002-phased-dispute-coordination/
├── plan.md
├── research.md
├── data-model.md
├── quickstart.md
├── contracts/
│   └── reasoning-backend.md
├── checklists/
│   └── requirements.md
└── spec.md
```

### Source Code (repository root)

```text
src/
├── main.rs                  # Entry point: config load, init, run
├── config.rs                # Configuration parsing (TOML + env)
├── daemon.rs                # Main daemon loop orchestration
├── nostr/
│   ├── mod.rs
│   ├── client.rs            # Nostr client setup, relay management
│   ├── subscriptions.rs     # Filter construction, subscription mgmt
│   └── notifier.rs          # Gift-wrap notification sending
├── dispatcher.rs            # Event routing: new dispute → handler
├── handlers/
│   ├── mod.rs
│   ├── dispute_detected.rs  # Phase 1: new dispute processing
│   └── dispute_updated.rs   # Phase 2: status change processing
├── db/
│   ├── mod.rs
│   ├── migrations.rs        # Schema creation and migration
│   ├── disputes.rs          # Dispute CRUD operations
│   ├── notifications.rs     # Notification record operations
│   └── state_transitions.rs # Phase 2: state transition records
├── models/
│   ├── mod.rs
│   ├── dispute.rs           # Internal dispute representation
│   ├── notification.rs      # Notification record types
│   └── config.rs            # Typed configuration structs
└── error.rs                 # Error types

tests/
├── integration/
│   ├── phase1_detection.rs      # Dispute detection + notification
│   ├── phase1_dedup.rs          # Deduplication across restarts
│   ├── phase1_failure.rs        # Relay disconnect, notif failure
│   ├── phase2_lifecycle.rs      # State transitions
│   ├── phase2_assignment.rs     # Solver takes dispute
│   └── phase2_renotification.rs # Timeout re-notification
└── unit/
    ├── config_test.rs
    ├── dispatcher_test.rs
    ├── db_disputes_test.rs
    └── db_notifications_test.rs
```

**Structure Decision**: Single Rust binary crate. All modules in `src/`.
No workspace, no library crate. Tests split between `tests/` (integration)
and inline `#[cfg(test)]` modules (unit). This matches the single-daemon
architecture with no reusable library surface in Phase 1-2.

## Module Architecture

### Flow: Dispute Detection → Notification (Phase 1)

```
Nostr Relay(s)
     │
     │  kind 38386, s=initiated
     ▼
┌─────────────────┐
│  nostr/client    │  Maintains relay connections, auto-reconnect
│  nostr/subs      │  Filter: kind=38386, #z=dispute, #s=initiated, #y=<mostro>
└────────┬────────┘
         │ RelayPoolNotification::Event
         ▼
┌─────────────────┐
│   dispatcher     │  Routes event by kind + status tag
└────────┬────────┘
         │
         ▼
┌─────────────────┐     ┌──────────┐
│  handlers/       │────▶│  db/     │  Check dedup, insert dispute
│  dispute_detected│     │ disputes │  Record notification attempts
└────────┬────────┘     └──────────┘
         │
         │  For each solver in config
         ▼
┌─────────────────┐
│  nostr/notifier  │  send_private_msg(solver_pubkey, message)
└─────────────────┘
```

### Flow: Assignment Detection → Suppression (Phase 2)

```
Nostr Relay(s)
     │
     │  kind 38386, s=in-progress
     ▼
┌─────────────────┐
│  nostr/client    │
│  nostr/subs      │  Additional filter: #s=in-progress
└────────┬────────┘
         │
         ▼
┌─────────────────┐     ┌──────────────────┐
│  handlers/       │────▶│  db/             │  Update lifecycle_state → taken
│  dispute_updated │     │  disputes        │  Record state transition
└────────┬────────┘     │  state_transitions│  Record assigned solver
         │              └──────────────────┘
         │
         ▼
┌─────────────────┐
│  nostr/notifier  │  Send assignment notification to all solvers
└─────────────────┘
```

### Flow: Re-Notification Timer (Phase 2)

```
┌─────────────────┐
│   daemon         │  Periodic tick (configurable interval)
└────────┬────────┘
         │
         ▼
┌─────────────────┐     ┌──────────┐
│  db/disputes     │────▶│  SQLite  │  Query: lifecycle_state = 'notified'
│                  │     │          │  AND last_notified_at < now - timeout
└────────┬────────┘     └──────────┘
         │
         │  For each unattended dispute
         ▼
┌─────────────────┐
│  nostr/notifier  │  Re-notify all solvers with updated status
└─────────────────┘
```

## Deduplication Strategy

### Phase 1

1. On receiving a kind 38386 event with `s=initiated`:
   - Extract `dispute_id` from the `d` tag.
   - Query SQLite: `SELECT 1 FROM disputes WHERE dispute_id = ?`.
   - If found: skip (already processed). Log at debug level.
   - If not found: INSERT into `disputes` table first, then notify solvers.
     Notification is strictly contingent on a successful INSERT — there
     is no in-memory notification queue in Phase 1. If the INSERT fails,
     notification is skipped for that event and is not automatically
     retried.
2. If SQLite is unreadable: halt notification processing. Log error.
   Resume when SQLite recovers. Deduplication integrity > delivery.
3. On restart: same logic. SQLite state survives restarts.

### Phase 2

Same dedup for initial detection. Additionally:
- Status update events (`s=in-progress`) are idempotent: if dispute
  is already in "taken" state, the update is a no-op.

## Degraded-Mode Behavior

| Failure | Behavior |
|---------|----------|
| Single relay drops | nostr-sdk auto-reconnects with backoff. Other relays continue. |
| All relays drop | Auto-reconnect continues. Notifications halt. Log degraded mode. |
| SQLite read failure | Halt all notification processing. Log error. Retry SQLite access periodically. Resume when recovered. |
| SQLite write failure | Log error. If the dispute INSERT fails, do not notify (dedup integrity). No Phase 1 queue exists, so the dispute may not be notified at all unless the same event is observed again after persistence recovers (e.g., from a subsequent relay retransmission or operator replay). |
| Notification send failure | Log failure in SQLite. Do not retry in Phase 1. Phase 2 re-notification covers unattended disputes. |
| No solvers configured | Log warning at startup. Record disputes but do not attempt notifications. |

## Configuration Surface

### config.toml

```toml
[serbero]
private_key = "<hex-encoded private key>"  # Override: SERBERO_PRIVATE_KEY env var
db_path = "serbero.db"  # Override: SERBERO_DB_PATH env var
log_level = "info"  # Override: SERBERO_LOG env var

[mostro]
pubkey = "<hex-encoded public key>"  # Mostro instance public key

[[relays]]
url = "wss://relay.example.com"

[[solvers]]
pubkey = "<hex-encoded public key>"
permission = "read"  # "read" or "write" — see note below

[timeouts]
renotification_seconds = 300  # Phase 2: re-notification interval
renotification_check_interval_seconds = 60  # How often to check for unattended disputes
```

**Solver permissions — scope in Phase 1**: The `permission` field may be
set to `"read"` or `"write"` and is parsed and stored from the start,
but **Phase 1 notification routing does not filter by permission**. In
Phase 1, every configured solver is notified of every detected dispute
regardless of their permission level. Permission levels become
operationally relevant in later phases — most notably Phase 4
(escalation), which routes escalation summaries specifically to
write-permission solvers. Phase 2 may begin to use permission for
differentiated messaging but does not restrict who is notified.

### Environment Variable Overrides

| Variable | Overrides | Purpose |
|----------|-----------|---------|
| `SERBERO_PRIVATE_KEY` | `serbero.private_key` | Secret key management |
| `SERBERO_DB_PATH` | `serbero.db_path` | Database file location |
| `SERBERO_LOG` | `serbero.log_level` | Log level (trace/debug/info/warn/error) |

## Reasoning Backend Interface (Phase 5 — Planning Artifact Only)

Described in [contracts/reasoning-backend.md](contracts/reasoning-backend.md).

The `ReasoningBackend` trait is a **planning and contracts artifact
only** for Phases 1 and 2. It is documented here to reserve the
architectural boundary and to give future phases a stable target to
design against. It is **not** scaffolded into the Rust source tree
during Phase 1 or Phase 2 — no `trait ReasoningBackend`, no
`reasoning/` module, and no reasoning-related types are added to the
crate until Phase 5 actually needs them. This avoids dead
architectural scaffolding that Phase 1 and Phase 2 do not exercise.

When Phase 5 is planned, the trait definition in
`contracts/reasoning-backend.md` becomes the starting point for the
actual Rust implementation; any refinements discovered then supersede
the contract as documented today.

**Key separation (applies once the backend is implemented in Phase 5)**:
- Serbero's policy layer (dispatcher, handlers) owns all decisions.
- The reasoning backend provides advisory structured outputs.
- The policy layer validates all reasoning output before acting.
- If the backend is unavailable, Serbero escalates to human operators.

## Testing Strategy

### Unit Tests

- `config_test.rs`: Parse valid/invalid TOML, env var overrides.
- `dispatcher_test.rs`: Event routing for known/unknown event kinds.
- `db_disputes_test.rs`: Insert, dedup check, state transitions.
- `db_notifications_test.rs`: Record notification attempts/failures.

### Integration Tests

- `phase1_detection.rs`: Publish kind 38386 event to test relay,
  verify notification received by mock solver.
- `phase1_dedup.rs`: Publish same event twice, verify single
  notification. Restart daemon, verify no re-notification.
- `phase1_failure.rs`: Disconnect relay, verify reconnection.
  Simulate notification failure, verify SQLite logging.
- `phase2_lifecycle.rs`: Verify state transitions from new →
  notified → taken → resolved.
- `phase2_assignment.rs`: Publish `in-progress` event, verify
  dispute transitions to "taken" and notifications suppressed.
- `phase2_renotification.rs`: Wait for timeout, verify re-notification
  sent for unattended disputes.

### Test Infrastructure

- Integration tests use a local relay (e.g., `nostr-relay` in Docker
  or a lightweight in-process relay for testing).
- SQLite uses in-memory databases (`:memory:`) for unit tests and
  temp files for integration tests.
- Mock solver: a nostr-sdk `Client` that listens for incoming
  gift-wrap messages and records them for assertion.

## Phased Implementation Order

### Phase 1: Always-On Listener + Solver Notification

Implementation sequence:

1. **Project setup**: Cargo.toml, dependencies, module skeleton
2. **Configuration**: TOML parsing, env overrides, typed config structs
3. **SQLite schema**: migrations for `disputes` and `notifications` tables
4. **Nostr client**: Relay connection, subscription filter for kind 38386
5. **Dispatcher**: Event routing from notification loop to handlers
6. **Dispute handler**: Dedup check, SQLite insert, trigger notification
7. **Notifier**: Gift-wrap message construction and sending
8. **Daemon loop**: Main entry point tying it together, graceful shutdown
9. **Audit logging**: tracing integration for all actions
10. **Integration tests**: Detection, dedup, restart, failure scenarios

### Phase 2: Intake Tracking + Assignment Visibility

Implementation sequence (extends Phase 1):

1. **Schema migration**: Add Phase 2 columns and `dispute_state_transitions` table
2. **Lifecycle state machine**: State transition logic with validation
3. **Extended subscription**: Add `s=in-progress` filter for assignment detection
4. **Assignment handler**: Process assignment events, update state, notify solvers
5. **Re-notification timer**: Periodic check for unattended disputes
6. **Assignment notification**: Notify all solvers when a dispute is taken
7. **Integration tests**: Lifecycle, assignment, re-notification scenarios

### Future Phases (Not Implemented Now)

- **Phase 3**: Guided mediation — user communication via gift wraps,
  classification, mediation session tracking.
- **Phase 4**: Escalation support — escalation triggers, structured
  summaries, write-operator routing.
- **Phase 5**: Reasoning backend — trait implementation for direct API
  and optional OpenClaw, policy validation layer.

These phases will be planned through separate specification amendments.

## Complexity Tracking

> No constitution violations to justify. All gates pass.
