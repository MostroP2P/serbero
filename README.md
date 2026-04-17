<p align="center">
  <img src="serbero.jpg" alt="Serbero" width="400">
</p>

# Serbero

Dispute coordination, notification, and assistance system for the [Mostro](https://mostro.network/) ecosystem.

Serbero helps operators and users handle disputes more quickly, more consistently, and with better visibility — without expanding the system's fund-risk surface.

---

## Table of Contents

- [What It Does](#what-it-does)
- [What It Does Not Do](#what-it-does-not-do)
- [Architecture](#architecture)
- [Implementation Status](#implementation-status)
- [Quickstart](#quickstart)
- [Configuration Reference](#configuration-reference)
- [How Serbero Behaves at Runtime](#how-serbero-behaves-at-runtime)
- [Notification Format](#notification-format)
- [Observability and Audit Trail](#observability-and-audit-trail)
- [Degraded-Mode Behavior](#degraded-mode-behavior)
- [Project Layout](#project-layout)
- [Running the Test Suite](#running-the-test-suite)
- [Technical Constraints](#technical-constraints)
- [Project Principles](#project-principles)
- [Roadmap](#roadmap)
- [License](#license)

---

## What It Does

Serbero sits alongside Mostro as a coordination layer that:

- **Detects disputes** by subscribing to Mostro's `kind 38386` dispute events on Nostr relays.
- **Notifies solvers** promptly via encrypted NIP-17 / NIP-59 gift-wrapped direct messages.
- **Deduplicates** across relay replays, reconnections, and process restarts using SQLite-backed persistence.
- **Tracks lifecycle state** (`new → notified → taken → waiting → escalated → resolved`) and records every transition.
- **Re-notifies unattended disputes** on a configurable timer and **suppresses further notifications** once a solver takes a dispute.
- **Records an audit trail** of every detection, notification attempt, state transition, and assignment event.

## What It Does Not Do

Serbero never moves funds. It cannot sign `admin-settle` or `admin-cancel`, and it is never granted credentials that would allow it to do so. Dispute-closing authority belongs to Mostro and its human operators.

Mostro operates normally with or without Serbero. If Serbero is offline, operators continue resolving disputes manually as they always have.

---

## Architecture

```text
┌──────────────┐      kind 38386 events     ┌──────────────────────┐
│    Mostro    │ ──────────────────────────▶│       Serbero        │
│              │                             │                      │
│  - Escrow    │                             │  - Detection         │
│  - Settle    │      NIP-59 gift wraps      │  - Dedup (SQLite)    │
│  - Cancel    │ ◀─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ │  - Notification      │
│  - Perms     │       (to solvers)          │  - Lifecycle state   │
└──────────────┘                             │  - Re-notification   │
                                             │  - Audit log         │
                                             └──────────┬───────────┘
                                                        │
                                                ┌───────▼────────┐
                                                │    SQLite      │
                                                │  - disputes    │
                                                │  - notifs      │
                                                │  - transitions │
                                                └────────────────┘
```

- **Mostro** owns escrow state, permissions, and dispute-closing authority.
- **Serbero** owns notification, coordination, assignment visibility, and audit logging.
- **Reasoning backend** (Phase 5) — documented as a planning contract in [`specs/002-phased-dispute-coordination/contracts/reasoning-backend.md`](specs/002-phased-dispute-coordination/contracts/reasoning-backend.md). No Rust code has been scaffolded for it yet. It will only be implemented when Phase 5 is planned.

---

## Implementation Status

Serbero evolves in five phases. The current codebase implements **Phases 1 and 2**:

| Phase | Scope                                                        | Status         |
|-------|--------------------------------------------------------------|----------------|
| 1     | Always-on dispute listener and solver notification           | **Implemented** |
| 2     | Intake tracking, assignment visibility, re-notification      | **Implemented** |
| 3     | Guided mediation for low-risk disputes                       | Planned        |
| 4     | Escalation support for write-permission operators            | Planned        |
| 5     | Optional reasoning backend (direct API / OpenClaw)           | Planned        |

The full specification lives in [`specs/002-phased-dispute-coordination/`](specs/002-phased-dispute-coordination/):

- [`spec.md`](specs/002-phased-dispute-coordination/spec.md) — user stories, requirements, acceptance criteria
- [`plan.md`](specs/002-phased-dispute-coordination/plan.md) — implementation plan, flow diagrams, degraded-mode table
- [`research.md`](specs/002-phased-dispute-coordination/research.md) — pinned technical decisions (nostr-sdk, mostro-core, rusqlite)
- [`data-model.md`](specs/002-phased-dispute-coordination/data-model.md) — SQLite schema, state machine, Phase 3+ forward-looking sketches
- [`quickstart.md`](specs/002-phased-dispute-coordination/quickstart.md) — verification steps for Phases 1 and 2
- [`tasks.md`](specs/002-phased-dispute-coordination/tasks.md) — the 50-task implementation breakdown

---

## Quickstart

### Prerequisites

- **Rust toolchain**, stable, edition 2021. Install via [`rustup`](https://rustup.rs/).
- Access to at least one Nostr relay that carries Mostro's dispute events.
- A **hex-encoded** Nostr key pair for Serbero. If you hold your keys in Bech32 form (`nsec...`, `npub...`), convert them to hex before placing them in the config.
- **Hex-encoded** Nostr public keys for the Mostro instance you monitor and for every solver you want to notify.

### Build

```bash
cargo build --release
```

The binary is produced at `./target/release/serbero`.

### Configure

Create `config.toml` in the working directory (see [Configuration Reference](#configuration-reference) for the full surface):

```toml
[serbero]
# Serbero's hex-encoded private key. Override via SERBERO_PRIVATE_KEY env var
# when running in production — do NOT commit this file with a real key.
private_key = "<hex-encoded private key>"
db_path     = "serbero.db"
log_level   = "info"

[mostro]
# Hex-encoded public key of the Mostro instance to monitor.
pubkey = "<hex-encoded public key>"

[[relays]]
url = "wss://relay.example.com"

[[solvers]]
pubkey     = "<hex-encoded solver public key>"
permission = "read"   # "read" or "write" — see notes below

[[solvers]]
pubkey     = "<hex-encoded solver public key>"
permission = "write"

[timeouts]
renotification_seconds                = 300   # re-notify disputes unattended this long
renotification_check_interval_seconds = 60    # how often to scan for unattended disputes
```

**About the `permission` field:** Phase 1 and Phase 2 notify **every** configured solver regardless of this value. Permission is parsed, stored, and surfaced to later phases — Phase 4 (escalation routing) will target write-permission solvers specifically. Setting it today is future-proofing, not gating.

### Run

Serbero reads `config.toml` from the current working directory. Secrets and a few operational parameters can be overridden via environment variables:

```bash
# Minimal invocation — expects ./config.toml
./target/release/serbero

# Override the private key via env (recommended for production)
SERBERO_PRIVATE_KEY="<hex-encoded private key>" ./target/release/serbero

# Point at a different config file (any path)
SERBERO_CONFIG=/etc/serbero/config.toml ./target/release/serbero

# Verbose tracing (module-level filters also supported)
SERBERO_LOG=debug ./target/release/serbero
SERBERO_LOG="serbero=debug,nostr_sdk=info" ./target/release/serbero
```

Shut down with `Ctrl-C` — Serbero handles `SIGINT`/`SIGTERM` cooperatively, aborts the re-notification timer, and exits cleanly.

### Verify Phase 1

1. Start Serbero with a valid config pointing at a test relay.
2. Publish a `kind 38386` event with tags `s=initiated`, `z=dispute`, `y=<mostro_pubkey>`, `d=<dispute_id>`, and `initiator=buyer` (or `seller`).
3. Every configured solver should receive an encrypted gift-wrap DM within seconds containing the dispute ID, initiator role, and event timestamp.
4. Publish the same event again — **no duplicate** notification should be sent.
5. Restart Serbero pointed at the same `db_path` — previously-seen disputes should **not** be re-notified.

### Verify Phase 2

1. After the initial notification, wait for `renotification_seconds` to elapse. Solvers should receive a single re-notification with `notif_type='re-notification'` and a status-aware payload.
2. Publish an `s=in-progress` event for the same dispute (this simulates a solver taking it via Mostro).
3. Serbero transitions the dispute to `taken`, records the `assigned_solver` from the event's `p` tag if present, and sends an **assignment** notification to all solvers.
4. No further re-notifications are sent for that dispute.

---

## Configuration Reference

### `config.toml` structure

| Section          | Key                                        | Type     | Required | Notes                                                                                       |
|------------------|--------------------------------------------|----------|----------|---------------------------------------------------------------------------------------------|
| `[serbero]`      | `private_key`                              | string   | ✓        | Hex-encoded secret key. Override: `SERBERO_PRIVATE_KEY`.                                    |
| `[serbero]`      | `db_path`                                  | string   |          | Defaults to `serbero.db`. Override: `SERBERO_DB_PATH`.                                      |
| `[serbero]`      | `log_level`                                | string   |          | `trace` / `debug` / `info` / `warn` / `error`. Defaults to `info`. Override: `SERBERO_LOG`. |
| `[mostro]`       | `pubkey`                                   | string   | ✓        | Hex-encoded public key of the Mostro instance to monitor.                                   |
| `[[relays]]`     | `url`                                      | string   | ≥ 1      | One or more `wss://…` relay URLs. Serbero connects to all of them.                          |
| `[[solvers]]`    | `pubkey`                                   | string   |          | Hex-encoded solver public key.                                                              |
| `[[solvers]]`    | `permission`                               | string   |          | `"read"` or `"write"`. Not used for filtering in Phases 1–2; reserved for Phase 4 routing.  |
| `[timeouts]`     | `renotification_seconds`                   | integer  |          | Defaults to `300`. Disputes in `notified` state older than this are re-notified.            |
| `[timeouts]`     | `renotification_check_interval_seconds`    | integer  |          | Defaults to `60`. How often the re-notification timer scans the DB.                         |

### Environment variable overrides

| Variable                | Overrides                | Behavior                                                                 |
|-------------------------|--------------------------|--------------------------------------------------------------------------|
| `SERBERO_CONFIG`        | path of config file      | Defaults to `./config.toml`.                                             |
| `SERBERO_PRIVATE_KEY`   | `[serbero].private_key`  | Preferred way to inject the key in production / systemd / containers.   |
| `SERBERO_DB_PATH`       | `[serbero].db_path`      | Absolute or relative path.                                               |
| `SERBERO_LOG`           | `[serbero].log_level`    | Accepts either a level (`info`) or a `tracing-subscriber` filter string. |

Empty or whitespace-only env values are **ignored** — an accidentally-unset shell variable will not wipe a valid config entry.

### No CLI flag surface

Phases 1 and 2 intentionally do not commit to a CLI flag surface. The entire configuration lives in `config.toml` plus the environment variables above. If you need to point at a different config file, use `SERBERO_CONFIG`, not a flag.

---

## How Serbero Behaves at Runtime

### Startup

1. Load config from `$SERBERO_CONFIG` (or `./config.toml`) and apply env overrides.
2. Initialize `tracing-subscriber` using `SERBERO_LOG` or `log_level` from the config.
3. Open the SQLite database at `db_path`; run migrations (`schema_version` is tracked so this is idempotent and survives restarts).
4. Build the Nostr client from the private key and connect to every configured relay. nostr-sdk handles automatic reconnection with backoff.
5. Subscribe to `kind 38386` events for the configured Mostro pubkey with `s ∈ {initiated, in-progress}`, `z=dispute`, `y=<mostro_pubkey>`.
6. Spawn the re-notification timer task.
7. Enter the main notification-handling loop, dispatching each incoming event by its `s` tag.

### New dispute (`s=initiated`)

1. Extract `dispute_id` (from `d` tag), `initiator` (buyer or seller), `mostro_pubkey` (from `y`), and the event's `id` / `created_at`.
2. Attempt to `INSERT` into `disputes` (keyed by `dispute_id` with `ON CONFLICT DO NOTHING`).
   - **Duplicate** → log at debug, skip notification (idempotent replay / restart).
   - **Insert fails** → log an error and **do not notify**. This is a deliberate Phase 1 policy: the dispute may not be notified unless the same event is observed again after persistence recovers. See `plan.md` §Deduplication Strategy and `spec.md` clarification 3.
   - **Inserted** → proceed.
3. For each configured solver: parse pubkey → send NIP-17/NIP-59 gift-wrapped DM via `send_private_msg` → record the attempt (`sent` or `failed`, with the error message) in the `notifications` table.
4. If at least one notification was sent, transition the dispute `new → notified`, record the transition in `dispute_state_transitions`, and update `last_notified_at`.

### Dispute taken (`s=in-progress`)

1. Look up the dispute by `dispute_id`.
2. If the dispute is already in `taken` / `waiting` / `escalated` / `resolved`, treat as idempotent no-op.
3. Otherwise transition `→ taken`, record the solver pubkey from the event's `p` tag (if present) in `assigned_solver`, and record the state transition.
4. Send an **assignment notification** (`notif_type='assignment'`) to every configured solver.

### Re-notification timer

Every `renotification_check_interval_seconds`, the background task:

1. Computes `cutoff = now - renotification_seconds`.
2. Queries disputes with `lifecycle_state = 'notified' AND last_notified_at < cutoff`.
3. For each match: sends a re-notification (`notif_type='re-notification'`) including the current `lifecycle_state` and elapsed time, then bumps `last_notified_at` to prevent the same tick from double-firing.

Disputes that are already `taken`, `waiting`, `escalated`, or `resolved` never trigger re-notifications — the SQL filter enforces this.

---

## Notification Format

All notifications are NIP-17/NIP-59 gift-wrapped direct messages. The rumor content is plain UTF-8 text. Phase 1 and 2 use three notification types:

### Initial notification

```text
New Mostro dispute requires attention.
dispute_id: <dispute-id>
initiator: <buyer|seller>
event_timestamp: <unix-seconds>
status: initiated
```

### Re-notification

```text
Mostro dispute is still unattended.
dispute_id: <dispute-id>
lifecycle_state: notified
time_elapsed_seconds: <n>
```

### Assignment notification

```text
Mostro dispute has been taken.
dispute_id: <dispute-id>
assigned_solver: <pubkey|unknown>
lifecycle_state: taken
```

Notifications **never include** the initiator's public key — only their trade role (buyer / seller). This matches the privacy clarification in `spec.md` Session 2026-04-16.

---

## Observability and Audit Trail

Serbero emits structured `tracing` spans and events at every decision point:

- `detected` / `duplicate_skip` / `persistence_failed`
- `notification_sent` / `notification_failed` (with `solver` and error)
- `state_transition` (with `from`, `to`, `trigger`)
- `assignment_detected` (with `assigned_solver`)
- `assignment_notification_sent` / `assignment_notification_failed`
- `renotification_tick` (with `count`)

Use `SERBERO_LOG` to tune the filter:

```bash
SERBERO_LOG="serbero=debug,nostr_sdk=warn" ./target/release/serbero
```

### SQLite tables

Every audit-relevant fact is also in the database, so you can reconstruct the history of a dispute without grepping logs:

- `disputes` — one row per detected dispute, including `lifecycle_state`, `assigned_solver`, `last_notified_at`, `last_state_change`.
- `notifications` — one row per notification attempt (initial, re-notification, assignment), with `status` (`sent` / `failed`) and `error_message`.
- `dispute_state_transitions` — every state change with `from_state`, `to_state`, `transitioned_at`, `trigger` (event id or internal tag).
- `schema_version` — tracks applied migrations; migrations are idempotent and wrapped in per-version transactions.

Inspect with the usual `sqlite3` CLI:

```bash
sqlite3 serbero.db "SELECT dispute_id, lifecycle_state, assigned_solver, last_state_change \
                    FROM disputes ORDER BY detected_at DESC LIMIT 20;"

sqlite3 serbero.db "SELECT dispute_id, notif_type, status, sent_at, error_message \
                    FROM notifications ORDER BY sent_at DESC LIMIT 50;"

sqlite3 serbero.db "SELECT dispute_id, from_state, to_state, trigger, transitioned_at \
                    FROM dispute_state_transitions ORDER BY id DESC LIMIT 50;"
```

---

## Degraded-Mode Behavior

| Failure                         | Behavior                                                                                                                                                                                                        |
|---------------------------------|-----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| Single relay drops              | nostr-sdk auto-reconnects with backoff. Other relays continue serving events.                                                                                                                                   |
| All relays drop                 | Reconnection keeps retrying. Notifications halt until a relay comes back. The daemon keeps running.                                                                                                            |
| SQLite read failure             | Notifications halt. The daemon logs the error, keeps retrying DB access, and resumes notifications when persistence recovers. Deduplication integrity is prioritized over delivery.                            |
| SQLite write failure on INSERT  | No Phase 1 queue exists. The dispute may not be notified at all unless the same event is observed again after persistence recovers (e.g., a relay retransmission or operator replay).                          |
| Notification send failure       | Logged as a `failed` row in `notifications` with the error message. Phase 1 does not retry individual sends. Phase 2's re-notification timer covers disputes that stay unattended.                            |
| Invalid solver pubkey in config | Logged as a `failed` notification row; other solvers still receive the notification. The daemon keeps running.                                                                                                 |
| No solvers configured           | Logged as a WARN at startup. Serbero still detects and persists disputes, but the notification loop is skipped — the audit trail is preserved.                                                                 |
| Serbero fully offline           | Mostro operates normally. Solvers resolve disputes manually. When Serbero comes back and reconnects, it resumes detecting **new** events. Historic events delivered while offline are the relay's to replay.   |

---

## Project Layout

```text
.
├── Cargo.toml, Cargo.lock
├── clippy.toml, rustfmt.toml
├── config.toml                          (you create this; gitignored)
├── src/
│   ├── main.rs                          # binary entry point
│   ├── lib.rs                           # re-exports modules for tests
│   ├── error.rs                         # Error + Result types
│   ├── config.rs                        # TOML + env loader
│   ├── daemon.rs                        # main loop + re-notification timer
│   ├── dispatcher.rs                    # event routing by `s` tag
│   ├── nostr/
│   │   ├── client.rs                    # Client + relay wiring
│   │   ├── subscriptions.rs             # kind 38386 filter builders
│   │   └── notifier.rs                  # gift-wrap send helper
│   ├── handlers/
│   │   ├── dispute_detected.rs          # s=initiated handler
│   │   └── dispute_updated.rs           # s=in-progress handler
│   ├── db/
│   │   ├── mod.rs                       # connection + pragmas
│   │   ├── migrations.rs                # schema_version + per-version txns
│   │   ├── disputes.rs                  # insert, get, lifecycle state helpers
│   │   ├── notifications.rs             # record_notification{,_logged}
│   │   └── state_transitions.rs         # unattended dispute query
│   └── models/
│       ├── config.rs                    # typed config structs
│       ├── dispute.rs                   # Dispute + LifecycleState state machine
│       └── notification.rs              # NotificationStatus / NotificationType
├── tests/
│   ├── common/mod.rs                    # MockRelay harness + SolverListener
│   ├── phase1_detection.rs
│   ├── phase1_dedup.rs
│   ├── phase1_failure.rs
│   ├── phase2_lifecycle.rs
│   ├── phase2_assignment.rs
│   └── phase2_renotification.rs
└── specs/002-phased-dispute-coordination/
    ├── spec.md                          # feature spec + clarifications
    ├── plan.md                          # implementation plan + flow diagrams
    ├── research.md                      # pinned SDK / crate decisions
    ├── data-model.md                    # SQLite schema + state machine
    ├── quickstart.md                    # verification steps
    ├── tasks.md                         # 50-task breakdown (all complete)
    ├── checklists/requirements.md
    └── contracts/reasoning-backend.md   # Phase 5 planning contract (no code yet)
```

---

## Running the Test Suite

The crate ships with **30 tests**: 22 unit tests (inline `#[cfg(test)]` modules across the lib) and 8 integration tests that spin up an in-process `nostr-relay-builder::MockRelay` and exercise the daemon end-to-end.

```bash
# Unit tests only (fast)
cargo test --lib

# Full suite (unit + integration)
cargo test --all-targets

# Lint + format checks
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check

# Release build
cargo build --release
```

Integration tests cover the scenarios from `quickstart.md`:

| Test file                        | What it verifies                                                                 |
|----------------------------------|----------------------------------------------------------------------------------|
| `phase1_detection.rs`            | New dispute detected → every solver receives correct gift-wrapped DM             |
| `phase1_dedup.rs`                | Duplicate events and daemon restarts produce exactly one notification            |
| `phase1_failure.rs`              | Invalid solver pubkey → `failed` row recorded, other solvers still notified; no-solvers path persists without notifying |
| `phase2_lifecycle.rs`            | `new → notified → taken` transition chain recorded in correct order              |
| `phase2_assignment.rs`           | `s=in-progress` → lifecycle_state `taken`, assigned_solver set, assignment notification delivered, no further re-notifications |
| `phase2_renotification.rs`       | Unattended disputes re-notified past timeout; taken disputes are not             |

---

## Technical Constraints

- **Rust**, stable, edition 2021.
- **[nostr-sdk](https://docs.rs/nostr-sdk/0.44.1) v0.44.1** for all Nostr communication (subscriptions, event handling, NIP-17 / NIP-59 gift-wrap messaging). The `nip59`, `nip44`, and `nip04` features are enabled.
- **[mostro-core](https://docs.rs/mostro-core/0.8.4) v0.8.4** for protocol types (`NOSTR_DISPUTE_EVENT_KIND`, dispute `Status` enum, `Action` variants).
- **[rusqlite](https://docs.rs/rusqlite) 0.31** with the `bundled` feature — no external SQLite install required. No ORM, no storage abstraction layer.
- **[tokio](https://docs.rs/tokio) 1** runtime (required by nostr-sdk), `tracing` for structured logs, `toml` + `serde` for configuration.
- Prefers **Nostr-native** communication (encrypted gift wraps) over external bridges or dashboards.

---

## Project Principles

Serbero is governed by a [constitution](.specify/memory/constitution.md) that defines non-negotiable rules. The key principles:

1. **Fund Isolation First** — never touch funds or sign dispute-closing actions.
2. **Protocol-Enforced Security** — safety boundaries enforced by Mostro, not by prompts or model behavior.
3. **Human Final Authority** — complex, adversarial, or ambiguous disputes always go to a human operator.
4. **Operator Notification as Core** — detecting and notifying operators is a primary responsibility.
5. **Assistance Without Authority** — assist and guide, never impose outcomes.
6. **Auditability by Design** — every action, classification, and state transition is logged.
7. **Graceful Degradation** — Mostro works fine without Serbero.
8. **Privacy by Default** — minimum necessary information to each participant.
9. **Nostr-Native Coordination** — encrypted messaging first, external integrations second.
10. **Portable Reasoning Backends** — no lock-in to any single AI provider or runtime.
11. **Incremental Scope** — evolve in stages through explicit specifications.
12. **Honest System Behavior** — surface uncertainty, never fabricate evidence.
13. **Mostro Compatibility** — complement Mostro, never duplicate or weaken its authority.

---

## Roadmap

Upcoming phases will be planned through their own specification amendments. The outline:

- **Phase 3 — Guided Mediation** (low-risk coordination failures): contact dispute parties via gift wraps, ask clarifying questions, surface uncertainty, and either suggest a cooperative resolution to the assigned solver or escalate.
- **Phase 4 — Escalation Support**: structured escalation summaries (dispute timeline, party claims, mediation actions, confidence assessment) routed to write-permission solvers, with re-escalation on no-acknowledge.
- **Phase 5 — Reasoning Backend**: implement the `ReasoningBackend` trait currently documented in [`contracts/reasoning-backend.md`](specs/002-phased-dispute-coordination/contracts/reasoning-backend.md). Direct-API default, optional OpenClaw, strict policy-layer validation of all advisory outputs.

No code for Phases 3–5 is present today — the forward-looking sketches in `data-model.md` and the trait contract are explicitly marked as provisional.

---

## License

Serbero is licensed under the [MIT License](LICENSE).
