# Implementation Plan: Guided Mediation (Phase 3)

**Branch**: `003-guided-mediation` | **Date**: 2026-04-17 | **Spec**: [spec.md](spec.md)
**Input**: Feature specification from `specs/003-guided-mediation/spec.md`

## Summary

Phase 3 extends the existing Serbero daemon with guided mediation for
low-risk coordination disputes. Serbero participates in Mostro's
solver-take flow with its registered solver identity, acquires the
chat-addressing shared-key material defined by Mostro's chat
protocol, and exchanges structured clarifying messages with dispute
parties through that transport. A mandatory reasoning provider
classifies disputes, drafts next messages, and produces structured
summaries; Serbero's policy layer validates every reasoning output
against the authority and escalation rules and enforces a clean
Phase 4 handoff when mediation is not the right instrument. All
behavior-controlling artifacts (system prompt, classification /
escalation policies, message templates, style rules) live in
versioned `prompts/` files; SQLite holds operational mediation state,
messages, summaries, events, and an isolated audit store for full
reasoning rationales.

## Technical Context

**Language/Version**: Rust (stable, edition 2021) — same toolchain as Phases 1 and 2.
**Primary Dependencies**: `nostr-sdk 0.44.1`, `mostro-core 0.8.4`, `rusqlite` (bundled), `tokio`, `serde`, `toml`, `tracing`. **New for Phase 3**: `reqwest` (HTTP client for reasoning providers), `sha2` (prompt-bundle hashing and rationale reference ids), `uuid` (session ids).
**Storage**: SQLite via rusqlite (direct, no ORM). Extended schema: `mediation_sessions`, `mediation_messages`, `mediation_summaries`, `mediation_events`, `reasoning_rationales`. Prompt/policy content stays out of SQLite (FR-105 / TC-103); only content hashes and bundle ids are persisted.
**Testing**: `cargo test` for unit + integration. Integration tests reuse the `nostr-relay-builder::MockRelay` harness from Phases 1/2; a `httpmock` (or equivalent in-tree stub) fronts the reasoning provider so tests do not depend on network reachability to a real LLM vendor.
**Target Platform**: Linux server (single-instance daemon). Same deployment assumption as earlier phases.
**Project Type**: Long-lived daemon / background service (additive to the existing binary crate; no new crate is introduced).
**Performance Goals**: Phase 3 is not latency-bound. Acceptable targets for planning: a mediation round (outbound → inbound → classification / draft) completes within tens of seconds on a healthy reasoning provider; the re-notification timer and Phase 1/2 dispatcher latencies remain unchanged by Phase 3 work.
**Constraints**: Single instance (no multi-instance coordination). Reasoning provider reachability is mandatory — if it fails, mediation halts but Phase 1/2 MUST remain fully operational (TC-102). Credentials are never persisted in `config.toml` (FR-104). The Phase 1/2 notifier, subscription filter, and SQLite-backed dedup path are NOT altered by Phase 3.
**Scale/Scope**: Low dispute volume (tens to hundreds per day) with a small number of active mediation sessions (single digits concurrent in typical operation). Prompt bundles are kilobytes; stored rationales are bounded per-session.

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

| Principle | Status | Verification |
|-----------|--------|--------------|
| I. Fund Isolation First | PASS | Phase 3 adds mediation messaging and summarization only. `admin-settle` and `admin-cancel` remain forbidden (FR-115). Reasoning provider outputs are advisory and pass through Serbero's policy layer (FR-116) — any "suggested settlement" is suppressed and escalated. |
| II. Protocol-Enforced Security | PASS | Transport is Mostro's own chat protocol and its solver-take flow (TC-101, Mediation Transport Requirements). Solver authorization is enforced by Mostro, not by Serbero (TC-104, Solver Identity and Authorization). Mediation does not depend on model behavior to enforce boundaries. |
| III. Human Final Authority | PASS | Phase 3 explicitly escalates (not resolves) conflicting claims, fraud indicators, low confidence, party non-response, round-limit, and reasoning-unavailability (FR-111 / FR-112). Summary delivery routes to a human solver; the solver retains final authority. |
| IV. Operator Notification | PASS | Phase 3 adds mediation-specific notifications on top of the Phase 1/2 notifier (FR-113), does not replace or silence it, and broadcasts / targets per the Solver-Facing Routing section. |
| V. Assistance Without Authority | PASS | Serbero identifies itself as an assistance system in every party-facing message (FR-108) and MUST NOT present itself as the final authority. Disallowed outputs (autonomous closure, binding decisions, fund-related instructions) are enumerated in AI Agent Behavior Boundaries. |
| VI. Auditability | PASS | Every mediation session, outbound / inbound message, state transition, escalation trigger, and reasoning call is persisted. Rationales are kept in a controlled audit store with reference ids surfaced to general logs (FR-120). `policy_hash` and `prompt_bundle_id` are pinned per session so behavior is reproducible from git history. |
| VII. Graceful Degradation | PASS | If the reasoning provider is unreachable, mediation halts but Phase 1/2 detection and notification remain fully operational (TC-102, SC-105). If Serbero is offline, Mostro operators continue resolving disputes manually. |
| VIII. Privacy | PASS | Mediation traffic uses shared-pubkey addressing per Mostro's protocol, not party primary pubkeys (FR-101, SC-107). General logs carry only classification, confidence, and a rationale reference id — never the full rationale or party statements (FR-120). Solver-facing notifications continue to omit party pubkeys per the Phase 1/2 privacy invariant. |
| IX. Nostr-Native | PASS | Party communication is Nostr-native (gift-wrapped chat events on Mostro's transport). Solver-facing DMs remain Nostr-native via the existing notifier. The reasoning provider is over HTTP but is not a coordination channel — it is an advisory compute endpoint. |
| X. Portable Reasoning Backends | PASS | Reasoning is behind a provider-adapter boundary (Reasoning Provider Configuration) that supports OpenAI, Anthropic, PPQ.ai, and OpenAI-compatible endpoints via config alone (FR-103, SC-104). A future OpenClaw adapter fits the same boundary. The mediation call sites see a single request/response shape; vendor switching does NOT require code changes. |
| XI. Incremental Scope | PASS | Phase 3 is declared additive on top of Phases 1–2 and explicitly defers Phase 4 execution mechanics, deeper reasoning-backend abstraction, and multi-instance coordination. |
| XII. Honest Behavior | PASS | Honesty is surfaced in both the prompt bundle (`phase3-mediation-style.md`) and the code path: malformed / low-confidence / fraud-flag reasoning outputs trigger escalation rather than a manufactured classification (FR-111, edge case "Reasoning provider returns malformed JSON"). |
| XIII. Mostro Compatibility | PASS | Serbero writes mediation chat events only. It does not sign dispute-closing actions, does not invent a parallel chat transport, and positions Phase 4 (escalation execution) as a separate phase that consumes Phase 3's handoff. |

**Gate result**: ALL PASS. No violations. `Complexity Tracking` at the end of this file remains empty.

### Scope-control notes (carried forward to tasks)

- **Solver authorization revalidation loop**: implement as the minimal surface the spec requires — one background `tokio::task` with truncated exponential backoff between `solver_auth_retry_initial_seconds` and `solver_auth_retry_max_interval_seconds`, terminating at the first of `solver_auth_retry_max_total_seconds` / `solver_auth_retry_max_attempts`, emitting one WARN-or-higher terminal alert on termination. Handful of config knobs, single task, terminal-alert logging. No generic retry framework. No state machine beyond "authorized" / "unauthorized" / "terminated". No plugin surface.
- **Reasoning provider adapter**: ship exactly one adapter in Phase 3 (`openai`, which also covers "openai-compatible" endpoints by changing `api_base`). Leave Anthropic / PPQ.ai adapters as `NotYetImplemented` entries in the enum so the mediation call sites stay generic but the code surface is small. Phase 5 can land additional adapters without touching mediation logic.

## Project Structure

### Documentation (this feature)

```text
specs/003-guided-mediation/
├── plan.md
├── research.md
├── data-model.md
├── quickstart.md
├── contracts/
│   ├── reasoning-provider.md      # Phase 3 reasoning-adapter contract
│   ├── mostro-chat.md             # Phase 3 chat-transport contract (refs Mostrix)
│   └── prompt-bundle.md           # Phase 3 prompt-bundle shape + hashing rules
├── checklists/
│   └── requirements.md
└── spec.md
```

### Source Code (repository root)

Phase 3 is additive on top of the existing Serbero crate. No new crate
is introduced; tests stay in the same `tests/` tree.

```text
src/
├── main.rs
├── lib.rs                           # re-exports Phase 3 modules
├── config.rs                        # extended: [mediation], [reasoning], [prompts], [chat]
├── daemon.rs                        # spawns Phase 3 background tasks when [mediation].enabled
├── dispatcher.rs                    # unchanged surface; mediation has its own ingestor
├── error.rs                         # new Phase 3 error variants
├── models/                          # typed config, lifecycle, notification (existing)
│   ├── config.rs                    # extend with MediationConfig, ReasoningConfig, PromptsConfig, ChatConfig
│   ├── mediation.rs                 # new: MediationSessionState, MediationOutcome, EscalationTrigger
│   └── reasoning.rs                 # new: ReasoningRequest, ReasoningResponse, ProviderKind
├── nostr/                           # existing: client, subscriptions, notifier
│   └── notifier.rs                  # unchanged; reused by Phase 3 via Solver-Facing Routing
├── chat/                            # new: Mostro chat transport
│   ├── mod.rs
│   ├── take_flow.rs                 # Mostro solver-take participation (refs execute_take_dispute.rs)
│   ├── shared_key.rs                # chat-addressing key material (refs chat_utils.rs)
│   ├── outbound.rs                  # wrap kind-1 inner event → NIP-59 gift-wrap → p=shared_pubkey
│   └── inbound.rs                   # fetch, unwrap, decrypt, verify, surface inner event + created_at
├── reasoning/                       # new: reasoning provider adapter boundary
│   ├── mod.rs                       # pub trait ReasoningProvider; single request/response shape
│   ├── openai.rs                    # the one adapter shipped in Phase 3
│   ├── not_yet_implemented.rs       # Anthropic/PPQ stubs returning ProviderUnavailable
│   └── health.rs                    # startup + config-reload health check (Reasoning Provider Configuration)
├── prompts/                         # new: prompt-bundle loader + hasher
│   ├── mod.rs                       # load all prompt files referenced by [prompts]
│   └── hash.rs                      # deterministic policy_hash over bundle bytes
├── mediation/                       # new: core mediation orchestration
│   ├── mod.rs                       # MediationEngine entry point
│   ├── session.rs                   # session lifecycle + state transitions
│   ├── router.rs                    # Solver-Facing Routing rule (broadcast vs. targeted)
│   ├── policy.rs                    # validates reasoning output against authority/escalation rules
│   ├── summarizer.rs                # cooperative-summary pipeline → notifier
│   ├── escalation.rs                # escalation-trigger detection + Phase 4 handoff package
│   └── auth_retry.rs                # bounded solver-auth revalidation loop (scope-controlled)
├── db/
│   ├── migrations.rs                # + Phase 3 migration (v3): new tables, new columns
│   ├── mediation.rs                 # new: mediation_sessions + mediation_messages helpers
│   ├── mediation_events.rs          # new: mediation_events + mediation_summaries helpers
│   └── rationales.rs                # new: controlled audit store for full rationales
└── handlers/
    └── dispute_updated.rs           # existing; extended only to flip mediation sessions to superseded_by_human on Phase 2 s=in-progress from an external solver

prompts/
├── phase3-system.md
├── phase3-classification.md
├── phase3-escalation-policy.md
├── phase3-mediation-style.md
└── phase3-message-templates.md

tests/
├── common/mod.rs                    # existing harness; extended with MockReasoningProvider + MostroChatSim helpers
├── phase1_*.rs                      # unchanged
├── phase2_*.rs                      # unchanged
├── phase3_session_open.rs           # US1 — opens session, takes flow, first message addressed to shared pubkey
├── phase3_response_ingest.rs        # US2 — inbound decrypt/verify, dedup, restart resume
├── phase3_cooperative_summary.rs    # US3 — summary flows to solver per Solver-Facing Routing
├── phase3_escalation_triggers.rs    # US4 — each trigger transitions to escalation_recommended + handoff
├── phase3_provider_swap.rs          # US5 — provider change via config + env, no code change
├── phase3_auth_retry.rs             # revalidation loop — success resume + terminal alert path
└── phase3_reasoning_unavailable.rs  # halt behavior, Phase 1/2 unaffected
```

**Structure Decision**: Keep the single Rust binary crate from Phases 1 and 2. Phase 3 is split into cohesive submodules (`chat/`, `reasoning/`, `prompts/`, `mediation/`) under `src/` and a repo-root `prompts/` tree for the versioned bundle. No workspace, no library crate, no new top-level tests directory structure. This mirrors the Phase 1/2 structure decision and keeps the blast radius of Phase 3 contained.

## Module Architecture

### Flow: Session open (US1)

```text
Phase 2 Dispute (notified/taken)
       │
       │  mediation::engine::maybe_start(dispute_id)
       ▼
┌────────────────────────────────┐
│  mediation/engine.rs          │  Guards: [mediation].enabled, reasoning healthy, auth OK
└───────────────┬────────────────┘
                │
                ▼
┌────────────────────────────────┐     ┌──────────────────────┐
│  chat/take_flow.rs            │────▶│  mostro solver-take  │  (protocol/chat.html)
│  Serbero takes dispute via    │     │  produces shared-key │
│  its registered solver        │     │  material per dispute│
└───────────────┬────────────────┘     └──────────┬───────────┘
                │                                 │
                │ shared_key material (per party) │
                ▼                                 ▼
┌────────────────────────────────┐     ┌──────────────────────┐
│  chat/shared_key.rs           │────▶│  addressing keypair  │
│  reconstruct Keys per party   │     │  for buyer / seller  │
└───────────────┬────────────────┘     └──────────────────────┘
                │
                ▼
┌────────────────────────────────┐
│  reasoning/* (first classify) │  classification + confidence (+ rationale → audit store)
└───────────────┬────────────────┘
                │
                ▼
┌────────────────────────────────┐     ┌──────────────────────┐
│  chat/outbound.rs             │────▶│  gift-wrap (kind 1059│
│  NIP-44 inner event, p tag =  │     │  p = shared pubkey)  │
│  shared pubkey                │     └──────────────────────┘
└───────────────┬────────────────┘
                │
                ▼
┌────────────────────────────────┐
│  db/mediation.rs              │  INSERT mediation_sessions row,
│                               │  pin prompt_bundle_id + policy_hash,
│                               │  INSERT outbound mediation_messages rows
└────────────────────────────────┘
```

### Flow: Inbound ingestion (US2)

```text
Mostro chat relay
       │
       │  gift-wraps addressed to shared pubkeys of active sessions
       ▼
┌────────────────────────────────┐
│  chat/inbound.rs              │  periodic fetch per [chat].inbound_fetch_interval_seconds
│  unwrap + decrypt + verify    │  authoritative = inner event content + created_at
└───────────────┬────────────────┘
                │
                ▼
┌────────────────────────────────┐
│  db/mediation.rs              │  dedup by inner event id; increment round counter only
│                               │  on first ingest
└───────────────┬────────────────┘
                │
                ▼
┌────────────────────────────────┐
│  mediation/policy.rs          │  decide: ask follow-up | summarize | escalate
└────────────────────────────────┘
```

### Flow: Cooperative summary (US3) and Solver-Facing Routing

```text
mediation/summarizer.rs
       │
       │  reasoning::classify + reasoning::summarize
       ▼
┌────────────────────────────────┐
│  policy.rs validates           │  advisory-only; authority-boundary check
└───────────────┬────────────────┘
                │
                ▼
┌────────────────────────────────┐     ┌──────────────────────┐
│  mediation/router.rs          │────▶│  phase 1/2 notifier  │
│  if disputes.assigned_solver   │     │  targeted or         │
│     → route to that solver     │     │  broadcast           │
│  else                         │     │                      │
│     → broadcast to configured │     │                      │
│        solvers                │     │                      │
└───────────────┬────────────────┘     └──────────────────────┘
                │
                ▼
        mediation_summaries row
```

### Flow: Escalation trigger → Phase 4 handoff (US4)

```text
mediation/policy.rs evaluator
       │
       │  any trigger: conflicting | fraud | low_conf | party_unresponsive |
       │               round_limit | reasoning_unavailable | authorization_lost
       ▼
┌────────────────────────────────┐
│  mediation/escalation.rs      │  write mediation_events row with trigger +
│                               │  evidence refs; assemble phase4 handoff
│                               │  package (dispute_id, session_id, trigger,
│                               │  transcript summary ref, prompt bundle)
└───────────────┬────────────────┘
                │
                ▼
┌────────────────────────────────┐
│  sessions.state =             │  stop further party messages on this session
│  escalation_recommended       │
└───────────────┬────────────────┘
                │
                ▼
┌────────────────────────────────┐
│  router.rs → notifier          │  surface "needs human judgment" to solvers
│                                │  (per Solver-Facing Routing)
└────────────────────────────────┘
```

### Flow: Solver auth revalidation (scope-controlled background loop)

```text
┌────────────────────────────────┐
│  mediation/auth_retry.rs      │  tokio::task
└───────────────┬────────────────┘
                │  initial startup verify → if fail, loop
                ▼
       ┌─────────────────────┐    success   ┌──────────────────────┐
   ─▶ │ verify solver record│ ───────────▶ │ resume normal Phase3 │
   │  │ against Mostro       │              │ operation (no restart│
   │  └─────────┬───────────┘              │  required)           │
   │            │ fail                      └──────────────────────┘
   │  backoff 60s → 120s → ... → 3600s (cap)
   │  also: re-verify on config reload
   │
   └── termination at
       min(max_total_seconds=86400, max_attempts=24)
       → one WARN alert, stop retrying
```

## Deduplication and State Invariants

### Phase 3 dedup

- **Inbound chat events** dedup by inner event id (after unwrap + verify). Outer gift-wrap id is NOT used.
- **Round counter** advances only on first-time ingest of an inbound event from a given party within an active session. Replays never double-count.
- **Out-of-order messages** (inner `created_at` predates last-seen marker) are persisted with `stale=true` for audit, do not move session state.
- **Restart resume**: every mediation session is recoverable from SQLite alone — session state, last-seen markers per party, round count, pinned prompt-bundle hash.

### Interaction with Phase 2 assignment

- If Phase 2 records `disputes.assigned_solver` for a dispute during an active mediation session, `mediation/router.rs` switches that session's solver-facing DMs from broadcast to targeted at the next notification.
- If the `assigned_solver` is NOT Serbero's own registered solver pubkey, the session transitions to `superseded_by_human`. Serbero stops sending party messages; Phase 4 handoff (if already prepared) stays in place for reference.

## Degraded-Mode Behavior (Phase 3 extensions)

| Failure | Behavior |
|---------|----------|
| Reasoning provider unreachable | Bounded retry per `[reasoning].followup_retry_count` and `[reasoning].request_timeout_seconds`. On exhaustion, session escalates with trigger `reasoning_unavailable`. Phase 1/2 unaffected (SC-105). |
| Reasoning provider returns malformed output | Treated as reasoning failure (not as "classification = Unclear"). Session escalates with trigger `reasoning_unavailable`. |
| Mostro chat event fails to send | Record the failure, escalate the session with trigger `authorization_lost` if the failure is auth-related; otherwise retry within the round's budget. Do NOT fall back to direct DMs. |
| Mostro chat protocol shape change | Halt new mediation sessions; log actionable error; existing sessions transition to `escalation_recommended` with trigger `authorization_lost` or `mediation_timeout`. |
| Solver permission revoked mid-session | Session transitions to `escalation_recommended` with trigger `authorization_lost`. Background auth-retry loop re-enters and gates any future sessions. |
| Startup solver-auth verify fails | Background revalidation loop runs; no new mediation sessions until success. Phase 1/2 fully operational. |
| Prompt bundle file missing or unreadable at startup | Startup fails loudly for Phase 3 only (mediation stays disabled). Phase 1/2 still boots if `[mediation].enabled = false` is effectively forced. |
| SQLite write failure on mediation row | Same discipline as Phase 1: record, do not fabricate further; escalate the session if the failure prevents correct state tracking. |

## Configuration Surface (Phase 3 additions)

Phase 3 extends `config.toml` per the spec's Configuration Surface
section. All fields default to safe values so operators who do not
want Phase 3 can leave them unset or set `[mediation].enabled = false`
and keep the Phase 1/2 daemon behavior verbatim.

```toml
[mediation]
enabled = true
max_rounds = 2
party_response_timeout_seconds = 1800
followup_retry_count = 1

# Solver authorization revalidation (scope-controlled; see plan notes)
solver_auth_retry_initial_seconds    = 60
solver_auth_retry_max_interval_seconds = 3600
solver_auth_retry_max_total_seconds  = 86400
solver_auth_retry_max_attempts       = 24

[reasoning]
enabled                  = true
provider                 = "openai"
model                    = "gpt-5"
api_base                 = "https://api.openai.com/v1"
api_key_env              = "OPENAI_API_KEY"
request_timeout_seconds  = 30

[prompts]
system_instructions_path   = "./prompts/phase3-system.md"
classification_policy_path = "./prompts/phase3-classification.md"
escalation_policy_path     = "./prompts/phase3-escalation-policy.md"
mediation_style_path       = "./prompts/phase3-mediation-style.md"
message_templates_path     = "./prompts/phase3-message-templates.md"

[chat]
inbound_fetch_interval_seconds = 10

[timeouts]
# (optional cross-cutting overrides)
```

Environment variable overrides from Phases 1/2 (`SERBERO_CONFIG`,
`SERBERO_PRIVATE_KEY`, `SERBERO_DB_PATH`, `SERBERO_LOG`) continue to
apply. Reasoning credentials are supplied via `[reasoning].api_key_env`
→ the named env var; credentials MUST NOT appear in `config.toml`.

## Testing Strategy

### Unit tests (inline `#[cfg(test)]` in `src/`)

- `prompts::hash`: deterministic hash over the exact bundle bytes; ordering-insensitive where the spec permits; non-equal bundles produce distinct hashes.
- `mediation::router`: the Solver-Facing Routing rule — targeted when `disputes.assigned_solver` is set, broadcast otherwise; switch on assignment change.
- `mediation::policy`: advisory-only validation; escalation-trigger detection for each enumerated trigger; authority-boundary suppression of "settlement" suggestions.
- `mediation::session` state transitions: every transition allowed by the session state machine; every transition outside it rejected.
- `reasoning::openai`: request shaping; credential sourced from env var; timeout and retry behavior per config.
- `db::mediation*` + `db::rationales`: CRUD, dedup, restart resume, rationale audit-store insert.

### Integration tests (`tests/phase3_*.rs`)

- `phase3_session_open.rs` (US1): MockRelay + Mostro-chat simulator + MockReasoningProvider; verifies the first outbound chat event is addressed to the ECDH-derived shared pubkey produced by the simulated solver-take flow and contains content drawn from the configured prompt bundle.
- `phase3_response_ingest.rs` (US2): inbound gift-wrap decrypt / verify, dedup on inner event id, round counter advance; restart-resume preserves round count and last-seen markers.
- `phase3_cooperative_summary.rs` (US3): cooperative two-round flow drives a summary delivered through the Phase 1/2 notifier per the Solver-Facing Routing rule; `mediation_summaries` row present with the session's `policy_hash`.
- `phase3_escalation_triggers.rs` (US4): one sub-test per enumerated trigger (conflicting, fraud, low_conf, party_unresponsive, round_limit, reasoning_unavailable, authorization_lost); each transitions the session to `escalation_recommended` and assembles the Phase 4 handoff package.
- `phase3_provider_swap.rs` (US5): swap provider from `openai` to an `openai-compatible` endpoint at a different `api_base` via config + env; no code change; mediation continues against the new endpoint.
- `phase3_auth_retry.rs`: startup verification fails → loop runs with deterministic short backoff values; success on attempt N resumes mediation without restart; termination path emits one WARN alert and stops retrying.
- `phase3_reasoning_unavailable.rs`: reasoning-provider stub always errors; verifies mediation halts, new sessions are refused, open sessions escalate with `reasoning_unavailable`, and Phase 1/2 continues notifying solvers normally.

### Test infrastructure

- `tests/common/mod.rs`: add `MockReasoningProvider` (in-process stub fronting an `httpmock` or hand-rolled HTTP server) and `MostroChatSim` that performs the solver-take handshake and replays gift-wrapped chat events — built on top of `nostr-relay-builder::MockRelay` reused from Phases 1/2.
- Prompt bundle fixtures under `tests/fixtures/prompts/phase3-*.md` so integration tests do not depend on the real `prompts/` directory at runtime.

## Phased Implementation Order (within Phase 3)

This is the order the `/speckit.tasks` command should expand. Keep
the revalidation loop and reasoning adapter scope-controlled per the
notes above.

1. **Project setup**: add `reqwest`, `sha2`, `uuid` to Cargo.toml; stub prompt-bundle fixtures; create the empty module skeleton under `src/chat`, `src/reasoning`, `src/prompts`, `src/mediation`.
2. **Phase 3 config + loader**: extend `src/models/config.rs` with `MediationConfig`, `ReasoningConfig`, `PromptsConfig`, `ChatConfig`, plus validation. Environment-override paths stay in `src/config.rs`.
3. **Schema migration v3**: add `mediation_sessions`, `mediation_messages`, `mediation_summaries`, `mediation_events`, `reasoning_rationales` tables. Add any missing Phase 3-only columns on `disputes` if needed.
4. **Prompt bundle loader + hasher**: load the configured paths at startup, compute the deterministic `policy_hash`, fail loudly on missing / unreadable files.
5. **Reasoning provider adapter boundary + `openai` adapter**: the trait surface plus the one adapter. NYI stubs for other providers. Health check at startup + config reload.
6. **Mostro chat transport**: `chat/take_flow.rs`, `chat/shared_key.rs`, `chat/outbound.rs`, `chat/inbound.rs`. Match Mostrix `execute_take_dispute.rs` + `chat_utils.rs` patterns.
7. **Mediation engine**: session lifecycle, policy validator, router (Solver-Facing Routing), summarizer, escalation / handoff package. This is the largest task block; split per story.
8. **Auth revalidation loop**: the scope-controlled background task.
9. **Integration tests**: one file per user story + the two cross-cutting failure tests listed above.
10. **Polish**: clippy / fmt / quickstart validation; cross-check `SC-102` audit claim (grep for any `admin-settle` / `admin-cancel` path — must remain zero).

## Complexity Tracking

> No constitution violations to justify. All gates pass. Section
> retained per template for reviewer convenience.
