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
┌──────────────┐      kind 38386 events     ┌────────────────────────────────┐
│    Mostro    │ ──────────────────────────▶│            Serbero             │
│              │                             │                                │
│  - Escrow    │                             │  Phase 1/2:                    │
│  - Settle    │      NIP-59 gift wraps      │   - Detection + dedup          │
│  - Cancel    │ ◀─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ │   - Solver notification        │
│  - Perms     │       (to solvers)          │   - Lifecycle + assignment     │
│  - Chat      │                             │   - Re-notification timer      │
│              │   NIP-59 to shared keys     │                                │
│              │ ◀─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ │  Phase 3 (mediation engine):   │
│              │      (to dispute parties)   │   - Take-flow + clarifying msg │
└──────┬───────┘                             │   - Inbound ingest + dedup     │
       │                                     │   - Classification + policy    │
       │                                     │   - Summary or escalation      │
       │                                     │   - Handoff package (Phase 4)  │
       │                                     └─────────┬──────────────┬───────┘
       │ NIP-44 chat                                   │              │
       │ (parties ↔ Serbero)                  HTTP /chat/completions  │
       │                                       (OpenAI-compatible)    │
       │                                               │              │
       ▼                                               ▼              ▼
┌──────────────┐                                ┌─────────────┐  ┌──────────┐
│ Buyer/Seller │                                │  Reasoning  │  │  SQLite  │
│ (per-trade   │                                │  endpoint   │  │   v3     │
│ shared keys) │                                └─────────────┘  └──────────┘
└──────────────┘
```

- **Mostro** owns escrow state, permissions, and dispute-closing authority.
- **Serbero** owns notification, coordination, assignment visibility, audit logging, and (Phase 3) guided mediation.
- **Reasoning backend** (Phase 3): an OpenAI-compatible HTTP endpoint that Serbero calls for classification + summary drafting. Pluggable via config (`api_base` + `api_key_env`); covers hosted OpenAI, self-hosted vLLM / llama.cpp / Ollama, LiteLLM, and any router proxy exposing `/chat/completions`. Outputs flow through a strict policy layer that suppresses fund-moving / dispute-closing instructions before any solver ever sees them.

---

## Implementation Status

Serbero evolves in five phases. `main` currently implements
**Phases 1, 2, and 3** end-to-end. Phase 3 ships the full guided-
mediation engine: take-flow, clarifying messages, inbound ingest,
classification, summary delivery, and escalation routing.

| Phase | Scope                                                        | Status on `main`                                                                                  |
|-------|--------------------------------------------------------------|---------------------------------------------------------------------------------------------------|
| 1     | Always-on dispute listener and solver notification           | **Implemented**                                                                                   |
| 2     | Intake tracking, assignment visibility, re-notification      | **Implemented**                                                                                   |
| 3     | Guided mediation for low-risk disputes                       | **Implemented** (88 / 88 tasks): US1–US5 + foundational + polish all closed                       |
| 4     | Escalation support for write-permission operators            | Planned (Phase 3 prepares the handoff package; Phase 4 will execute it)                           |
| 5     | Optional reasoning backend                                   | OpenAI-compatible adapter shipped (covers hosted OpenAI, vLLM, llama.cpp, Ollama, LiteLLM, etc.); other vendor adapters are future work |

### What Phase 3 ships

Setting `[mediation].enabled = true` and `[reasoning].enabled = true`
spawns the mediation engine task. On every tick it:

- **Opens sessions** for new disputes that pass the mediation-
  eligibility gate. The reasoning provider classifies the dispute
  first; only if the verdict is positive does Serbero issue
  `TakeDispute` and commit a session row (FR-122 / SC-110). The
  first clarifying message is dispatched to each party's **shared
  (per-trade) pubkey** — never their primary pubkey. If the
  reasoning verdict is negative (e.g. suspected fraud), no take is
  issued and the dispute is escalated with a dispute-scoped handoff
  to all configured solvers.
- **Ingests inbound replies** (`fetch_inbound` + `ingest_inbound`)
  with author authentication, dedup by `(session_id,
  inner_event_id)`, transcript recomputation, and per-party last-seen
  tracking.
- **Classifies** each turn through the configured reasoning provider
  using the versioned prompt bundle. The policy layer enforces:
  fraud / conflicting-claims flags escalate immediately,
  `confidence < threshold` escalates, fund-moving / dispute-closing
  outputs are suppressed and escalated as `authority_boundary_attempt`.
- **Delivers cooperative summaries** (`deliver_summary`) to the
  assigned solver (or broadcasts to all configured solvers if none is
  assigned), then closes the session.
- **Escalates** (`notify_solvers_escalation`) on any of 12 triggers
  (`conflicting_claims`, `fraud_indicator`, `low_confidence`,
  `party_unresponsive`, `round_limit`, `reasoning_unavailable`,
  `authorization_lost`, `authority_boundary_attempt`,
  `mediation_timeout`, `policy_bundle_missing`, `invalid_model_output`,
  `notification_failed`) and writes a Phase 4 handoff package to
  `mediation_events`.
- **Resumes after restart**: `startup_resume_pass` rebuilds the
  per-session ECDH key cache from the `mediation_sessions` table so
  inbound dedup and outbound key-derivation survive process restarts
  (FR-117).
- **Revalidates solver auth** in a bounded background loop when the
  initial check fails — Phase 1/2 keeps running unaffected
  throughout.

Phase 1/2 behavior remains fully isolated: any Phase 3 bring-up
failure (missing prompt bundle, unreachable reasoning provider,
revoked solver auth) leaves Phase 1/2 detection + notification
untouched.

### Specifications

The Phase 1/2 specification lives in [`specs/002-phased-dispute-coordination/`](specs/002-phased-dispute-coordination/):

- [`spec.md`](specs/002-phased-dispute-coordination/spec.md) — user stories, requirements, acceptance criteria
- [`plan.md`](specs/002-phased-dispute-coordination/plan.md) — implementation plan, flow diagrams, degraded-mode table
- [`research.md`](specs/002-phased-dispute-coordination/research.md) — pinned technical decisions (nostr-sdk, mostro-core, rusqlite)
- [`data-model.md`](specs/002-phased-dispute-coordination/data-model.md) — SQLite schema, state machine, Phase 3+ forward-looking sketches
- [`quickstart.md`](specs/002-phased-dispute-coordination/quickstart.md) — verification steps for Phases 1 and 2
- [`tasks.md`](specs/002-phased-dispute-coordination/tasks.md) — the 50-task implementation breakdown

Phase 3 specification:

- [`spec.md`](specs/003-guided-mediation/spec.md) — mediation user stories, requirements, acceptance criteria, and the normative sections on transport, reasoning, prompts, and memory
- [`plan.md`](specs/003-guided-mediation/plan.md), [`research.md`](specs/003-guided-mediation/research.md), [`data-model.md`](specs/003-guided-mediation/data-model.md), [`contracts/`](specs/003-guided-mediation/contracts/) — design artifacts
- [`tasks.md`](specs/003-guided-mediation/tasks.md) — 88-task breakdown; see the per-task `[X]` markers for what has actually shipped on `main` today

---

## Quickstart

### Prerequisites

- **Rust toolchain**, stable, edition 2021. Install via [`rustup`](https://rustup.rs/).
- Access to at least one Nostr relay that carries Mostro's dispute events.
- A **hex-encoded** Nostr key pair for Serbero. If you hold your keys in Bech32 form (`nsec...`, `npub...`), convert them to hex before placing them in the config. The public key derived from this keypair is the identity Serbero uses on Nostr — you must register it as a solver on the Mostro instance before enabling Phase 3 (see [Enable Phase 3](#enable-phase-3-guided-mediation)).
- **Hex-encoded** Nostr public keys for the Mostro instance you monitor and for every solver you want to notify.

### Build

```bash
cargo build --release
```

The binary is produced at `./target/release/serbero`.

### Configure

Create `config.toml` in the working directory. A reference template is provided at [`config.sample.toml`](config.sample.toml) — copy it and fill in your values (see [Configuration Reference](#configuration-reference) for the full surface):

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

Shut down with `Ctrl-C` (SIGINT). On Unix hosts Serbero also catches SIGTERM (so `systemctl stop`, `kill`, and container shutdowns work). Both paths abort the re-notification timer and exit cleanly.

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

### Enable Phase 3 (guided mediation)

Phase 3 layers on top of Phases 1 and 2. To enable it:

1. **Register Serbero as a solver** on the target Mostro instance with at least `read` permission. Serbero's public key is derived from the `private_key` field in `[serbero]` — you can obtain it with any Nostr key tool (e.g. `nak key public <hex-secret-key>`). In **Mostrix**, go to **Settings → Solvers**, paste the hex pubkey, and select `read` permission. Serbero never holds fund-moving credentials.
2. **Provision a reasoning endpoint** (any OpenAI-compatible HTTPS endpoint: hosted OpenAI, self-hosted vLLM / llama.cpp / Ollama, LiteLLM, or any router proxy exposing `/chat/completions`).
3. **Export the API key** under the env-var name configured in `[reasoning].api_key_env` (default: `SERBERO_REASONING_API_KEY`):

   ```bash
   export SERBERO_REASONING_API_KEY="<your key>"
   ```

4. **Add the Phase 3 sections** to `config.toml` (see [Phase 3 configuration surface](#phase-3-configuration-surface)) and ensure the `prompts/phase3-*.md` files exist and contain real mediation content (the repo ships a working bundle — see [Prompt bundle](#prompt-bundle)).
5. **Restart**:

   ```bash
   ./target/release/serbero
   ```

   At startup you should see (alongside the Phase 1/2 lines):

   ```text
   loaded config                    mostro_pubkey=<hex> db_path=serbero.db relay_count=N solver_count=M ...
   Phase 3 prompt bundle loaded     prompt_bundle_id=phase3-default policy_hash=<hex>
   reasoning provider health check ok
   Phase 3 mediation is fully configured; engine task will be spawned
   ```

If the reasoning health check fails, Phase 3 stays disabled for the run (SC-105) and Phase 1/2 continues unaffected:

```text
Phase 3 reasoning health check failed; mediation disabled for this run
(Phase 1/2 detection and notification continue unaffected)
```

If the initial solver-auth check fails, Phase 3 refuses to open new sessions and a bounded retry loop runs in the background; warnings log per attempt.

### Verify Phase 3

1. **Cooperative path (US3)** — publish a buyer-initiated dispute that the policy layer can classify as `coordination_failure_resolvable` (e.g., a payment-timing case). Expected:
   - A `mediation_sessions` row with `state='awaiting_response'` and the policy hash pinned.
   - The buyer's and seller's **shared (per-trade) pubkeys** receive the first clarifying gift wrap (NOT their primary pubkeys — SC-107).
   - After both parties reply, a `mediation_summaries` row is written and the assigned solver receives a `mediation_summary` notification. The session transitions `summary_pending → summary_delivered → closed`.
2. **Escalation path (US4)** — drive any of the 12 triggers (let `party_response_timeout_seconds` elapse without replies, exceed `max_rounds`, or take the reasoning provider offline). Expected:
   - Session transitions to `escalation_recommended`.
   - A `mediation_events` row records the trigger and a `handoff_prepared` row carries the Phase 4 package.
   - The configured solvers receive a `mediation_escalation_recommended` notification ("Needs human judgment").
3. **Provider swap (US5)** — stop the daemon, change `[reasoning].provider` / `model` / `api_base` / `api_key_env` to point at a different OpenAI-compatible endpoint, export the new key, and restart. New sessions call the new endpoint; no rebuild needed.
4. **Restart resume (FR-117)** — kill the daemon mid-session and restart. The startup-resume pass rebuilds the per-session key cache from the database, so inbound replies are deduped correctly and outbound responses go to the right shared keys.

For the full operator walkthrough see [`specs/003-guided-mediation/quickstart.md`](specs/003-guided-mediation/quickstart.md).

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

### Phase 3 configuration surface

Phase 3 adds four new functional sections. They are all `#[serde(default)]` — if you omit them, the daemon behaves as a Phase 1/2 daemon. With both `[mediation].enabled = true` and `[reasoning].enabled = true`, the daemon runs the Phase 3 bring-up (prompt-bundle load, reasoning health check) and spawns the mediation engine task.

```toml
[mediation]
enabled = true                   # Phase 3 mediation feature flag (see caveat above)
max_rounds = 2
party_response_timeout_seconds = 1800

# Solver-auth bounded revalidation loop (scope-controlled)
solver_auth_retry_initial_seconds      = 60
solver_auth_retry_max_interval_seconds = 3600
solver_auth_retry_max_total_seconds    = 86400
solver_auth_retry_max_attempts         = 24

[reasoning]
enabled                 = true
provider                = "openai"                    # only shipped adapter
model                   = "gpt-5"                     # anything the endpoint supports
api_base                = "https://api.openai.com/v1" # swap for any OpenAI-compatible endpoint
api_key_env             = "SERBERO_REASONING_API_KEY" # vendor-neutral on purpose
request_timeout_seconds = 30
followup_retry_count    = 1                           # adapter owns the HTTP retry budget

[prompts]
system_instructions_path   = "./prompts/phase3-system.md"
classification_policy_path = "./prompts/phase3-classification.md"
escalation_policy_path     = "./prompts/phase3-escalation-policy.md"
mediation_style_path       = "./prompts/phase3-mediation-style.md"
message_templates_path     = "./prompts/phase3-message-templates.md"

[chat]
inbound_fetch_interval_seconds = 10
```

Per-field notes:

| Section        | Key                                       | Notes                                                                                                    |
|----------------|-------------------------------------------|----------------------------------------------------------------------------------------------------------|
| `[mediation]`  | `enabled`                                 | Master switch for Phase 3. `false` → daemon behaves as a pure Phase 1/2 daemon.                          |
| `[mediation]`  | `max_rounds`                              | Number of outbound+inbound pairs per session before `round_limit` escalation. Defaults to `2`.           |
| `[mediation]`  | `party_response_timeout_seconds`          | Triggers `party_unresponsive` escalation. Defaults to `1800` (30 min). Set to `0` to disable the timer.  |
| `[mediation]`  | `solver_auth_retry_*`                     | Bounded revalidation loop for Serbero's solver registration in Mostro. Defaults: 60s→3600s, 24h/24 caps. |
| `[reasoning]`  | `enabled`                                 | Must be `true` (alongside `[mediation].enabled`) for the engine task to spawn.                           |
| `[reasoning]`  | `provider`                                | `openai` (also covers OpenAI-compatible endpoints — vLLM, llama.cpp, Ollama, LiteLLM, any router proxy). `anthropic` / `ppqai` / `openclaw` are placeholders that fail loudly at startup. |
| `[reasoning]`  | `model`                                   | Whatever model the configured endpoint accepts (e.g., `gpt-5`, `gpt-4o-mini`, a self-hosted model name). |
| `[reasoning]`  | `api_base`                                | Where the HTTP client points. Change this to swap to any OpenAI-compatible endpoint without a rebuild.   |
| `[reasoning]`  | `api_key_env`                             | **Environment variable name** whose value holds the credential. Defaults to `SERBERO_REASONING_API_KEY`. The variable name is just configuration — point it at any var your secrets pipeline already sets. |
| `[reasoning]`  | `request_timeout_seconds`                 | Per-HTTP-call timeout. Floored to ≥ 1 s.                                                                 |
| `[reasoning]`  | `followup_retry_count`                    | Adapter-owned bounded retry budget (FR-104). Additional attempts after the initial request on retryable errors. `0` = no retry. |
| `[prompts]`    | `*_path`                                  | Paths to the versioned prompt bundle files. The default paths match the `prompts/` tree in this repo.    |
| `[chat]`       | `inbound_fetch_interval_seconds`          | Mostro-chat inbound polling cadence used by the engine ingest loop.                                      |

### Secrets and environment variable resolution

- `config.toml` **never** carries secrets. The `[reasoning].api_key` field is `skip_deserializing`; TOML cannot set it.
- At startup the daemon reads the env variable named by `[reasoning].api_key_env` (default: `SERBERO_REASONING_API_KEY`) and stores the trimmed value. Surrounding whitespace or trailing newlines are stripped so nothing breaks bearer-token auth.
- If `[reasoning].enabled = true` and the named variable is unset or empty, the daemon returns a loud `Error::Config` and Phase 3 stays off. Phase 1/2 behavior is unaffected.
- Choose a variable name that fits your secrets pipeline. The default is vendor-neutral so a freshly-cloned daemon does not imply "OpenAI-only"; point it at whatever variable your deployment environment is already exporting.

### Prompt bundle

The default layout — matched by `[prompts].*` defaults — is:

```text
prompts/
├── phase3-system.md             # mediation identity + authority limits + honesty discipline
├── phase3-classification.md     # 5 labels (snake_case canonical), 5 flags, confidence semantics
├── phase3-escalation-policy.md  # 12 triggers + handoff package shape
├── phase3-mediation-style.md    # tone, prohibited / preferred phrasings
└── phase3-message-templates.md  # first / follow-up / summary / escalation / timeout templates
```

The shipped bundle is real, working content matching `spec.md` §AI Agent Behavior Boundaries — assistance-only identity, no fund-moving authority, explicit honesty / uncertainty rules, allowed vs. disallowed outputs. Operators can amend it (e.g., to localize message templates) without code changes; the `policy_hash` regenerates deterministically at startup.

Every mediation session pins the bundle's `policy_hash` and `prompt_bundle_id` (SC-103). Every audit row in `mediation_sessions`, `mediation_messages`, `mediation_summaries`, `mediation_events`, and `reasoning_rationales` carries the same pair, so behavior is reproducible from git history and the audit trail can be replayed against the exact bundle bytes that produced it.

Missing files → `Error::PromptBundleLoad`; Phase 3 stays off, Phase 1/2 keeps running.

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

### Phase 3 mediation engine

When `[mediation].enabled = true` and the bring-up succeeds, an engine task runs alongside the Phase 1/2 loop. Each tick:

1. **Open** — for any new dispute that passes the eligibility gate, run the dispute-chat take-flow against Mostro, derive per-trade ECDH shared keys for both parties, persist a `mediation_sessions` row with the bundle pinned, and dispatch the first clarifying message to each party's **shared pubkey** (SC-107). Outbound rows land in `mediation_messages` with provenance.
2. **Ingest** — `fetch_inbound` polls Mostro's chat surface every `inbound_fetch_interval_seconds`. `ingest_inbound` authenticates the inner event's author against the expected trade pubkey, pins the inner kind to `TextNote`, dedups by `(session_id, inner_event_id)` (so a relay replay or daemon restart never double-counts), recomputes `round_count` from the transcript, and updates per-party last-seen timestamps.
3. **Classify** — once both parties have replied for the round, the engine calls the reasoning provider's `classify` method with the full prompt bundle (so the `policy_hash` pin is honest) and the transcript. The response is parsed into a snake_case-keyed JSON shape (`coordination_failure_resolvable`, `conflicting_claims`, `suspected_fraud`, `unclear`, `not_suitable_for_mediation`) plus a confidence score and flags.
4. **Apply policy** — fraud / conflicting-claims flags escalate immediately; `confidence < 0.5` escalates with `low_confidence`; `Summarize` paired with a non-cooperative label escalates with `invalid_model_output`; any output containing fund-moving / dispute-closing phrases is suppressed and escalated with `authority_boundary_attempt`. Otherwise the policy decides between `AskClarification`, `Summarize`, or `Escalate(reason)`.
5. **Summarize or escalate** — the cooperative path calls `summarize`, persists `mediation_summaries`, and routes a `mediation_summary` notification to the assigned solver (or broadcasts to all configured solvers if none is assigned). The session transitions `summary_pending → summary_delivered → closed`. The escalation path writes a `handoff_prepared` row with the Phase 4 package and sends a `mediation_escalation_recommended` notification.
6. **Resume after restart** — `startup_resume_pass` rebuilds the per-session ECDH key cache from `mediation_sessions` so a daemon restart never breaks dedup or outbound key derivation (FR-117). Sessions whose `policy_hash` no longer matches the loaded bundle are escalated with `policy_bundle_missing`.
7. **Auth retry** — if the initial solver-auth check fails, a bounded background loop revalidates with exponential backoff (knobs under `[mediation].solver_auth_retry_*`). Until it recovers, new session opens are deterministically refused; Phase 1/2 continues unaffected.

All rationale text is written only to the audit store (`reasoning_rationales`) and referenced by `rationale_id` (SHA-256 content hash) in general logs and `mediation_events.payload_json` (FR-120). The `RationaleText::Debug` impl redacts the body to `<N bytes redacted>`.

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

### Mediation summary (Phase 3, cooperative path)

`notif_type='mediation_summary'`, gift-wrapped to the assigned solver (targeted) or every configured solver (broadcast):

```text
<summary text from the reasoning provider>

Suggested next step: <single-line recommendation>
```

The summary text describes what each party reported and proposes a cooperative resolution. The "Suggested next step" line is advisory only — no fund-moving instructions are ever drafted (the policy layer suppresses them). The full rationale is preserved in `reasoning_rationales` and referenced by `rationale_id` in `mediation_events`.

### Mediation escalation (Phase 3, US4)

`notif_type='mediation_escalation_recommended'`, gift-wrapped to the assigned solver or all configured solvers:

```text
Mediation session <session_id> (dispute <dispute_id>) escalated —
trigger: <snake_case_trigger>. Needs human judgment.
```

The compact body keeps DMs readable across Nostr clients; the full handoff package (evidence refs, rationale refs, prompt bundle id, policy hash, assembled-at timestamp) lives alongside in `mediation_events` as a `handoff_prepared` row for Phase 4 to consume.

### Dispute-scoped escalation (FR-122, pre-take)

`notif_type='mediation_escalation_recommended'`, broadcast to **all** configured solvers (no session was opened, so there is no assigned solver):

```text
Dispute <dispute_id> escalated before mediation take —
trigger: <snake_case_trigger>. Serbero ran the reasoning verdict and
the policy layer said this dispute is not a mediation candidate.
No session was opened. Needs human judgment.
```

This fires when the reasoning verdict at session-open time is negative (e.g. `suspected_fraud` or `not_suitable_for_mediation`). No `TakeDispute` is issued and no `mediation_sessions` row is committed (SC-110).

### Final resolution report (FR-124)

`notif_type='mediation_resolution_report'`, broadcast to all configured solvers:

```text
Final resolution report for dispute <dispute_id>.
resolution: <settled|cancelled|...>
escalation_count: <N>
rounds: <N>
duration_seconds: <N>
handoff: <true|false>
```

Emitted once when a dispute that had any Phase 3 mediation context (session rows, dispute-scoped handoff events, or mediation messages) transitions to a resolved terminal state. Idempotent: duplicate `dispute_resolved` events do not trigger additional reports. Contains no rationale text (FR-120).

Notifications **never include** the initiator's primary pubkey — only their trade role (buyer / seller). Outbound mediation gift wraps address parties' **shared (per-trade) pubkeys**, never their primary pubkeys (SC-107). This matches the privacy clarification in `spec.md` Session 2026-04-16.

---

## Observability and Audit Trail

Serbero emits structured `tracing` spans and events at every decision point:

- `detected` / `duplicate_skip` / `persistence_failed`
- `notification_sent` / `notification_failed` (with `solver` and error)
- `state_transition` (with `from`, `to`, `trigger`)
- `assignment_detected` (with `assigned_solver`)
- `assignment_notification_sent` / `assignment_notification_failed`
- `renotification_tick` (with `count`)
- `start_attempt_started` / `start_attempt_stopped` (with `trigger`, `stop_reason`)
- `reasoning_verdict` / `reasoning_verdict_negative`
- `take_dispute_issued` (with `outcome: success|failure`)
- `solver_dispute_escalation_notified` (FR-122 dispute-scoped handoff)
- `solver_final_resolution_report_sent` (FR-124)

Use `SERBERO_LOG` to tune the filter:

```bash
SERBERO_LOG="serbero=debug,nostr_sdk=warn" ./target/release/serbero
```

### SQLite tables

Every audit-relevant fact is also in the database, so you can reconstruct the history of a dispute without grepping logs.

**Phase 1/2 tables:**

- `disputes` — one row per detected dispute, including `lifecycle_state`, `assigned_solver`, `last_notified_at`, `last_state_change`.
- `notifications` — one row per notification attempt (`initial`, `re-notification`, `assignment`, `mediation_summary`, `mediation_escalation_recommended`), with `status` (`sent` / `failed`) and `error_message`.
- `dispute_state_transitions` — every state change with `from_state`, `to_state`, `transitioned_at`, `trigger`.
- `schema_version` — tracks applied migrations; migrations are idempotent and wrapped in per-version transactions.

**Phase 3 tables (migration v3):**

- `mediation_sessions` — one row per opened session: `state`, `round_count`, the pinned `prompt_bundle_id` + `policy_hash`, `buyer_shared_pubkey` / `seller_shared_pubkey`, per-party last-seen timestamps.
- `mediation_messages` — every outbound and inbound message, dedup-keyed by `(session_id, inner_event_id)`, with the bundle pinned on outbound rows.
- `reasoning_rationales` — content-addressed (SHA-256) rationale text from every classify / summarize call, with provider, model, and bundle pinned. Operator-only audit store; FR-120 ensures the body never leaks into general logs or `mediation_events.payload_json`.
- `mediation_summaries` — one row per cooperative summary delivered, with classification, confidence, summary text, suggested next step, and the rationale reference id.
- `mediation_events` — every lifecycle / audit event (session-open, classification, summary-generated, escalation-triggered, handoff-prepared, auth-retry-{attempt,recovered,terminated}, etc.) with the bundle and (where applicable) rationale referenced by id.

Inspect with the usual `sqlite3` CLI:

```bash
# Phase 1/2 — recent disputes + notifications
sqlite3 serbero.db "SELECT dispute_id, lifecycle_state, assigned_solver, last_state_change \
                    FROM disputes ORDER BY detected_at DESC LIMIT 20;"

sqlite3 serbero.db "SELECT dispute_id, notif_type, status, sent_at, error_message \
                    FROM notifications ORDER BY sent_at DESC LIMIT 50;"

sqlite3 serbero.db "SELECT dispute_id, from_state, to_state, trigger, transitioned_at \
                    FROM dispute_state_transitions ORDER BY id DESC LIMIT 50;"

# Phase 3 — mediation sessions and their state
sqlite3 serbero.db "SELECT session_id, dispute_id, state, round_count, policy_hash \
                    FROM mediation_sessions ORDER BY started_at DESC LIMIT 20;"

# Phase 3 — full transcript for a session
sqlite3 serbero.db "SELECT direction, party, inner_event_created_at, substr(content,1,80) \
                    FROM mediation_messages WHERE session_id='<sid>' \
                    ORDER BY inner_event_created_at ASC;"

# Phase 3 — lifecycle / escalation events
sqlite3 serbero.db "SELECT kind, substr(payload_json,1,120), occurred_at \
                    FROM mediation_events WHERE session_id='<sid>' \
                    ORDER BY id ASC;"

# Phase 3 — rationale audit store (operator-only; gate behind filesystem permissions)
sqlite3 serbero.db "SELECT rationale_id, provider, model, policy_hash, generated_at \
                    FROM reasoning_rationales ORDER BY generated_at DESC LIMIT 20;"
```

### SC-102 audit (Phase 3 never executed a fund-moving action)

Re-confirm at any time that no Mostro admin action ever flowed through Serbero:

```bash
sqlite3 serbero.db "SELECT COUNT(*) FROM notifications \
                    WHERE notif_type IN ('admin_settle','admin_cancel');"
# Expected: 0
```

Combined with the constitutional invariant that Serbero holds no credentials for those actions, Phase 3 satisfies *I. Fund Isolation First*.

---

## Degraded-Mode Behavior

| Failure                                        | Behavior                                                                                                                                                                                                        |
|------------------------------------------------|-----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| Single relay drops                             | nostr-sdk auto-reconnects with backoff. Other relays continue serving events.                                                                                                                                   |
| All relays drop                                | Reconnection keeps retrying. Notifications halt until a relay comes back. The daemon keeps running.                                                                                                            |
| SQLite read failure                            | Notifications halt. The daemon logs the error, keeps retrying DB access, and resumes notifications when persistence recovers. Deduplication integrity is prioritized over delivery.                            |
| SQLite write failure on INSERT                 | No Phase 1 queue exists. The dispute may not be notified at all unless the same event is observed again after persistence recovers (e.g., a relay retransmission or operator replay).                          |
| Notification send failure                      | Logged as a `failed` row in `notifications` with the error message. Phase 1 does not retry individual sends. Phase 2's re-notification timer covers disputes that stay unattended.                            |
| Invalid solver pubkey in config                | Logged as a `failed` notification row; other solvers still receive the notification. The daemon keeps running.                                                                                                 |
| No solvers configured                          | Logged as a WARN at startup. Serbero still detects and persists disputes, but the notification loop is skipped — the audit trail is preserved.                                                                 |
| Serbero fully offline                          | Mostro operates normally. Solvers resolve disputes manually. When Serbero comes back and reconnects, it resumes detecting **new** events. Historic events delivered while offline are the relay's to replay.   |
| Phase 3 — prompt bundle missing / unloadable   | `Error::PromptBundleLoad` at startup; Phase 3 stays disabled for the run. Phase 1/2 detection + notification continue unaffected. Resumed sessions whose pinned `policy_hash` no longer matches the loaded bundle are escalated with `policy_bundle_missing`. |
| Phase 3 — reasoning provider health-check fails| SC-105: Phase 3 stays disabled for the run; engine task is not spawned; Phase 1/2 continues unaffected. Operator-actionable error logs `provider`, `model`, `api_base`, and the underlying error.              |
| Phase 3 — reasoning provider unreachable mid-session | The classify / summarize call surfaces `ReasoningError`; the session escalates with `reasoning_unavailable`. Adapter-owned bounded retry budget (`followup_retry_count`) covers transient errors first. |
| Phase 3 — reasoning provider returns garbage   | `MalformedResponse` → escalates with `reasoning_unavailable`. Structurally inconsistent shape (e.g. `Summarize` + non-cooperative label) escalates with `invalid_model_output`.                                |
| Phase 3 — reasoning output crosses authority boundary | Suppressed by `AUTHORITY_BOUNDARY_PHRASES` detection; session escalates with `authority_boundary_attempt`. The full output is preserved in the rationale store; general logs reference it by id only.        |
| Phase 3 — solver auth lost at startup          | Initial check fails → bounded auth-retry loop runs in the background with exponential backoff; session opens are deterministically refused until recovery. Phase 1/2 unaffected.                              |
| Phase 3 — solver auth revoked mid-session      | Outbound auth failure surfaces as `AuthorizationLost`; affected session escalates with `authorization_lost`. Auth-retry loop resumes.                                                                          |
| Phase 3 — party stops responding               | After `party_response_timeout_seconds`, session escalates with `party_unresponsive`. Set the timeout to `0` to disable the check (test / staging only).                                                       |
| Phase 3 — round limit reached                  | After `max_rounds` outbound+inbound pairs without convergence, session escalates with `round_limit`.                                                                                                          |
| Phase 3 — daemon restart mid-session           | `startup_resume_pass` rebuilds the per-session ECDH key cache from `mediation_sessions`; inbound dedup and outbound key derivation survive intact (FR-117). The restart-dedup integration test pins this. |
| Phase 3 — mediation summary undeliverable      | If the summary persists but every solver send fails (or no recipients are configured), session escalates with `notification_failed` so the audit trail surfaces it instead of stranding it at `summary_pending`. |

---

## Project Layout

```text
.
├── Cargo.toml, Cargo.lock
├── clippy.toml, rustfmt.toml
├── config.toml                          (you create this; gitignored)
├── prompts/                             # Phase 3 versioned prompt bundle
│   ├── phase3-system.md
│   ├── phase3-classification.md
│   ├── phase3-escalation-policy.md
│   ├── phase3-mediation-style.md
│   └── phase3-message-templates.md
├── src/
│   ├── main.rs                          # binary entry point
│   ├── lib.rs                           # re-exports modules for tests
│   ├── error.rs                         # Error + Result types
│   ├── config.rs                        # TOML + env loader
│   ├── daemon.rs                        # main loop + re-notification + Phase 3 bring-up
│   ├── dispatcher.rs                    # event routing by `s` tag
│   ├── nostr/                           # Client, subscriptions, gift-wrap notifier
│   ├── handlers/                        # s=initiated / s=in-progress handlers
│   ├── chat/                            # Phase 3 dispute-chat surface
│   │   ├── dispute_chat_flow.rs         # take-flow against Mostro
│   │   ├── inbound.rs                   # fetch + ingest with author auth + dedup
│   │   ├── outbound.rs                  # build wraps for shared pubkeys
│   │   └── shared_key.rs                # ECDH per-trade key derivation
│   ├── reasoning/                       # Phase 3 provider abstraction
│   │   ├── mod.rs                       # ReasoningProvider trait + factory
│   │   ├── openai.rs                    # OpenAI-compatible adapter
│   │   ├── not_yet_implemented.rs       # NYI guard for unshipped vendor names
│   │   └── health.rs                    # startup health check (SC-105)
│   ├── prompts/                         # Phase 3 prompt bundle loader + hash
│   │   ├── mod.rs                       # PromptBundle + load_bundle
│   │   └── hash.rs                      # deterministic SHA-256 of bundle bytes
│   ├── mediation/                       # Phase 3 engine
│   │   ├── mod.rs                       # run_engine, draft_and_send_initial_message,
│   │   │                                # deliver_summary, notify_solvers_escalation,
│   │   │                                # startup_resume_pass
│   │   ├── session.rs                   # open_session + auth gate
│   │   ├── start.rs                     # try_start_for: unified entry for event-driven + tick
│   │   ├── auth_retry.rs                # bounded solver-auth revalidation
│   │   ├── policy.rs                    # classification → action decision
│   │   ├── eligibility.rs               # composed eligibility predicate (FR-123)
│   │   ├── follow_up.rs                 # mid-session ingest + classify loop
│   │   ├── transcript.rs                # transcript builder for reasoning calls
│   │   ├── report.rs                    # FR-124 final solver-facing resolution report
│   │   ├── summarizer.rs                # summarize + AUTHORITY_BOUNDARY_PHRASES
│   │   ├── router.rs                    # targeted vs broadcast solver routing
│   │   └── escalation.rs                # 12 triggers + handoff package
│   ├── db/
│   │   ├── mod.rs                       # connection + pragmas
│   │   ├── migrations.rs                # schema_version (v1, v2, v3) + per-version txns
│   │   ├── disputes.rs                  # insert, get, lifecycle state helpers
│   │   ├── notifications.rs             # record_notification{,_logged}
│   │   ├── state_transitions.rs         # unattended dispute query
│   │   ├── mediation.rs                 # mediation_sessions + mediation_messages
│   │   ├── mediation_events.rs          # lifecycle / audit events
│   │   └── rationales.rs                # content-addressed rationale audit store
│   └── models/
│       ├── config.rs                    # typed config structs (incl. Phase 3 sections)
│       ├── dispute.rs                   # Dispute + LifecycleState state machine
│       ├── notification.rs              # NotificationStatus / NotificationType
│       ├── mediation.rs                 # ClassificationLabel, Flag, EscalationTrigger, …
│       └── reasoning.rs                 # ReasoningRequest / Response + RationaleText
├── tests/
│   ├── common/mod.rs                    # MockRelay harness + SolverListener
│   ├── phase1_detection.rs              ├── phase2_lifecycle.rs
│   ├── phase1_dedup.rs                  ├── phase2_assignment.rs
│   ├── phase1_failure.rs                ├── phase2_renotification.rs
│   ├── phase3_session_open.rs           ├── phase3_summary_escalation.rs
│   ├── phase3_session_open_gating.rs    ├── phase3_authority_boundary.rs
│   ├── phase3_response_ingest.rs        ├── phase3_escalation_triggers.rs
│   ├── phase3_response_dedup_restart.rs ├── phase3_provider_swap.rs
│   ├── phase3_stale_message.rs          ├── phase3_provider_not_yet_implemented.rs
│   ├── phase3_routing_model.rs          ├── phase3_cooperative_summary.rs
│   ├── phase3_event_driven_start.rs     ├── phase3_superseded_by_human.rs
│   ├── phase3_take_reasoning_coupling.rs├── phase3_external_resolution_report.rs
│   ├── phase3_followup_round.rs         ├── phase3_followup_summary.rs
│   ├── phase3_followup_reasoning_failure.rs
│   └── fixtures/prompts/                # stable bundle for tests (untouched)
└── specs/
    ├── 002-phased-dispute-coordination/ # Phase 1/2 spec + plan + tasks
    └── 003-guided-mediation/            # Phase 3 spec + plan + tasks + contracts + quickstart
```

---

## Running the Test Suite

The crate ships **228 tests**: 179 inline `#[cfg(test)]` lib unit tests (covering parsers, policy decisions, audit-store invariants, migrations, prompt loading, …) plus 49 integration tests that spin up an in-process `nostr-relay-builder::MockRelay` (and, where relevant, an `httpmock` reasoning endpoint) and exercise the daemon end-to-end.

```bash
# Unit tests only (fast)
cargo test --lib

# Full suite (unit + integration)
cargo test --all-targets

# Lint + format checks
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --all -- --check

# Release build
cargo build --release
```

**Phase 1/2 integration tests** cover the scenarios from `specs/002-phased-dispute-coordination/quickstart.md`:

| Test file                        | What it verifies                                                                 |
|----------------------------------|----------------------------------------------------------------------------------|
| `phase1_detection.rs`            | New dispute detected → every solver receives correct gift-wrapped DM             |
| `phase1_dedup.rs`                | Duplicate events and daemon restarts produce exactly one notification            |
| `phase1_failure.rs`              | Invalid solver pubkey → `failed` row recorded, other solvers still notified; no-solvers path persists without notifying |
| `phase2_lifecycle.rs`            | `new → notified → taken` transition chain recorded in correct order              |
| `phase2_assignment.rs`           | `s=in-progress` → lifecycle_state `taken`, assigned_solver set, assignment notification delivered, no further re-notifications |
| `phase2_renotification.rs`       | Unattended disputes re-notified past timeout; taken disputes are not             |

**Phase 3 integration tests** cover the scenarios from `specs/003-guided-mediation/quickstart.md`:

| Test file                                       | What it verifies                                                                                                   |
|-------------------------------------------------|--------------------------------------------------------------------------------------------------------------------|
| `phase3_session_open.rs`                        | Open session → take-flow → first clarifying message dispatched to both parties' shared pubkeys                      |
| `phase3_session_open_gating.rs`                 | Reasoning health-check failure → session open refused; Phase 1/2 still notifies (SC-105)                            |
| `phase3_response_ingest.rs`                     | Inbound replies authenticated, dedup-keyed by inner event id, transcript + last-seen updated                        |
| `phase3_response_dedup_restart.rs`              | Inbound dedup survives daemon restart (FR-117 — `startup_resume_pass` rebuilds the session-key cache)              |
| `phase3_stale_message.rs`                       | Stale inbound is persisted but does not advance the session                                                        |
| `phase3_routing_model.rs`                       | Targeted (assigned solver) vs broadcast routing; fallback when assigned solver is unknown                          |
| `phase3_cooperative_summary.rs`                 | US3 happy path: summary persisted, session closes, assigned solver notified, FR-120 redaction + SC-103 audit consistency pinned |
| `phase3_summary_escalation.rs`                  | Empty recipient list → `notification_failed` escalation (no stranded summaries)                                    |
| `phase3_authority_boundary.rs`                  | Fund-moving / dispute-closing output suppressed; session escalates with `authority_boundary_attempt`               |
| `phase3_escalation_triggers.rs`                 | All applicable triggers fire correctly: `conflicting_claims`, `fraud_indicator`, `low_confidence`, `party_unresponsive`, `round_limit`, `reasoning_unavailable`, `authorization_lost` |
| `phase3_provider_swap.rs`                       | Two `OpenAiProvider`s pointing at distinct httpmock endpoints both work; `openai-compatible` routes to the same adapter (US5) |
| `phase3_event_driven_start.rs`                  | SC-109: event-driven path opens session without the background tick running                                        |
| `phase3_superseded_by_human.rs`                 | External resolution (human solver) closes session + fires FR-124 final report to all solvers                       |
| `phase3_take_reasoning_coupling.rs`             | FR-122 / SC-110: negative reasoning verdict (fraud flag or model escalate) skips TakeDispute entirely              |
| `phase3_external_resolution_report.rs`          | FR-124: final solver-facing report emitted for every dispute with Phase 3 context; idempotent on re-fire           |
| `phase3_followup_round.rs`                      | SC-112: mid-session happy path — party replies trigger second outbound within one ingest tick                      |
| `phase3_followup_summary.rs`                    | SC-114: mid-session summarize branch fires exactly once and closes session                                         |
| `phase3_followup_reasoning_failure.rs`           | SC-115: three consecutive reasoning failures escalate with `reasoning_unavailable`                                 |
| `phase3_provider_not_yet_implemented.rs`        | Unshipped vendor names (`anthropic`, `ppqai`, `openclaw`) fail loudly at startup with an actionable error          |

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

- **Phase 1 — Detection + notification**: shipped.
- **Phase 2 — Lifecycle + re-notification + assignment visibility**: shipped.
- **Phase 3 — Guided Mediation** (low-risk coordination failures): shipped on `main`. Contacts dispute parties via gift wraps to their shared pubkeys, runs bounded clarifying rounds, classifies through a versioned prompt bundle + reasoning provider, and either delivers a cooperative summary to the assigned solver or escalates with a Phase 4 handoff package. Strict policy-layer validation suppresses any output that would cross Serbero's authority boundary.
- **Phase 4 — Escalation Execution**: planned. Phase 3 already prepares the `handoff_prepared` package (evidence refs, rationale refs, prompt bundle id, policy hash); Phase 4 will consume it — routing to write-permission solvers, re-escalation on no-acknowledge, and the operator UI surface.
- **Phase 5 — Additional Reasoning Adapters**: the OpenAI-compatible adapter shipped in Phase 3 already covers hosted OpenAI, vLLM, llama.cpp, Ollama, LiteLLM, and any router proxy exposing `/chat/completions`. Vendor-specific adapters (Anthropic, PPQai, OpenClaw) are tracked as future work behind a `not_yet_implemented` guard that fails loudly at startup so operators get an actionable message rather than silent coercion.

---

## License

Serbero is licensed under the [MIT License](LICENSE).
