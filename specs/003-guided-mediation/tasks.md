# Tasks: Guided Mediation (Phase 3)

**Input**: Design documents from `/specs/003-guided-mediation/`
**Prerequisites**: spec.md (required), plan.md (required), research.md, data-model.md, contracts/, quickstart.md

**Tests**: Included. The plan's Testing Strategy explicitly lists unit
tests (inline `#[cfg(test)]`) and integration tests per user story
(`tests/phase3_*.rs`). Integration tests run against an in-process
`nostr-relay-builder::MockRelay` plus a `MockReasoningProvider` and a
`MostroChatSim` that models the subset of the dispute-chat interaction
flow Phase 3 exercises (verified against current Mostro / Mostrix
behavior at test-fixture time).

**Organization**: Tasks are grouped by user story. All six user
stories (US1 P1, US2 P1, US3 P2, US4 P2, US5 P3, US6 P2) are in scope
for this phase. The task ids remain globally monotonic by authoring
time, so the US6 test task (`T095`) intentionally appears after the
US6 implementation ids (`T089`–`T094`). A later **Phase 10 —
Mediation Start-Flow Correction** (T097–T115) addresses the
gap-analysis amendments added to the spec on 2026-04-20 (FR-121
event-driven start, FR-122 take strictly coupled to reasoning, FR-123
composed eligibility predicate, FR-124 final solver report on external
resolution; see `spec.md` §"Mediation Start-Flow Ordering" and §"Final
Solver Report on External Resolution"). A subsequent **Phase 11 —
Mid-Session Follow-Up Loop** (T116–T126) closes the mid-session
advancement gap confirmed by the 2026-04-21 audit (FR-125..FR-131;
see `spec.md` §"Mid-Session Follow-Up Loop"). Both phases are
remediation passes on top of the already-merged US1–US6; neither
introduces a new user story. Phases 4 and 5 of the overall roadmap
remain out of scope; the Phase 4 handoff package is *prepared* but
not consumed here.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies on incomplete tasks)
- **[Story]**: Which user story this task belongs to (e.g., US1, US2, US3, US4, US5, US6)
- Include exact file paths in descriptions

## Path Conventions

Single Rust binary crate at repository root (additive to Phases 1/2).
Phase 3 introduces new submodules under `src/`: `src/chat/`,
`src/reasoning/`, `src/prompts/`, `src/mediation/`. A new repo-root
`prompts/` directory holds the versioned prompt bundle. Integration
tests live under `tests/` alongside existing Phase 1/2 tests.

Scope-control notes carried from `plan.md` (must be honored while
expanding tasks):

- The solver-auth revalidation loop is **one** `tokio::task` with
  truncated exponential backoff, terminal WARN alert, and no generic
  retry framework.
- Only **one** reasoning adapter ships in Phase 3 (`openai`, which
  also covers OpenAI-compatible endpoints via `api_base`). Other
  providers (`anthropic`, `ppqai`, `openclaw`) are
  `NotYetImplemented` stubs that fail loudly at startup.
- Mediation transport is the **dispute-chat interaction flow used by
  current Mostro clients**, verified against the Mostrix reference;
  not a generic ECDH shortcut, not a reimplementation of a freestanding
  protocol.

---

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: Pull in the Phase 3 dependencies, create the new module
skeleton under `src/`, and stand up the repo-root `prompts/` tree.

- [X] T001 Extend `Cargo.toml` with the Phase 3 dependencies: `reqwest` (with `rustls-tls`, `json`), `sha2`, `uuid` (with `v4`). Add `httpmock` as a dev-dependency for reasoning-provider test stubs. Do NOT bump `nostr-sdk` or `mostro-core`
- [X] T002 [P] Create the repo-root `prompts/` directory with stub files: `prompts/phase3-system.md`, `prompts/phase3-classification.md`, `prompts/phase3-escalation-policy.md`, `prompts/phase3-mediation-style.md`, `prompts/phase3-message-templates.md`. Each stub contains the skeleton prescribed in `contracts/prompt-bundle.md` §Shape (Scope / Rules / optional Examples) so hashing produces a stable `policy_hash` from day one
- [X] T003 [P] Create the Phase 3 module skeleton per `plan.md` §Project Structure (empty `mod.rs` / file stubs that `cargo check` accepts): `src/chat/{mod.rs,dispute_chat_flow.rs,shared_key.rs,outbound.rs,inbound.rs}`, `src/reasoning/{mod.rs,openai.rs,not_yet_implemented.rs,health.rs}`, `src/prompts/{mod.rs,hash.rs}`, `src/mediation/{mod.rs,session.rs,router.rs,policy.rs,summarizer.rs,escalation.rs,auth_retry.rs}`. Add each new module to `src/lib.rs` as `pub mod ...` so tests can reach internal types
- [X] T004 [P] Add `tests/fixtures/prompts/` with a `phase3-default` bundle mirroring the real `prompts/` layout, so integration tests get a deterministic bundle without depending on the repo-root `prompts/` directory at runtime

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: Shared infrastructure that every user story needs: error
variants, typed config, schema migration v3, prompt bundle loader +
hasher, reasoning-adapter boundary, and the daemon wiring that spawns
Phase 3 background tasks only when `[mediation].enabled`.

**⚠️ CRITICAL**: No user story work can begin until this phase is complete.

- [X] T005 Extend `src/error.rs` with Phase 3 error variants: `MediationDisabled`, `ReasoningUnavailable(String)`, `PromptBundleLoad(String)`, `AuthNotRegistered`, `AuthTerminated`, `ChatTransport(String)`, `ProviderNotYetImplemented(String)`. Keep the existing `Result` type
- [X] T006 [P] Implement `src/models/mediation.rs` — enums `MediationSessionState`, `EscalationTrigger`, `TranscriptParty`, `SuggestedAction`, `Flag`, `ClassificationLabel`, plus the `MediationSession` struct mirroring `data-model.md` §mediation_sessions. Include a `can_transition_to(next)` method enforcing the state machine diagram (self-transitions rejected)
- [X] T007 [P] Implement `src/models/reasoning.rs` — request/response types exactly per `contracts/reasoning-provider.md` (`ClassificationRequest`/`Response`, `SummaryRequest`/`Response`, `Flag`, `TranscriptEntry`, `PromptBundleView<'a>`, `ReasoningError`). Derive `Debug`/`Clone` where the contract doesn't forbid it
- [X] T008 [P] Extend `src/models/config.rs` with Phase 3 structs: `MediationConfig` (feature flag + per-round knobs + the four `solver_auth_retry_*` fields with the defaults from the spec), `ReasoningConfig` (provider, model, api_base, api_key_env, request_timeout_seconds, followup_retry_count), `PromptsConfig` (five path fields), `ChatConfig { inbound_fetch_interval_seconds }`. All `#[serde(default)]` so omitted sections leave the daemon in Phase 1/2-only mode
- [X] T009 Extend `src/config.rs::load_config` to accept the new sections and apply env-based credential resolution for `[reasoning].api_key_env` (read the named env var; return `Error::Config` if enabled but unset). Credentials MUST NOT appear in the returned `Config` as plaintext keys beyond the env-resolved field
- [X] T010 Add Phase 3 migration `v3` to `src/db/migrations.rs` per `data-model.md`: create `mediation_sessions`, `mediation_messages` (with the unique `(session_id, inner_event_id)` index), `mediation_summaries`, `mediation_events`, `reasoning_rationales`; create the index set listed in the data model. Bump `CURRENT_SCHEMA_VERSION` to `3` and add a test that v2→v3 is applied idempotently
- [X] T011 [P] Implement `src/prompts/hash.rs::policy_hash(system, classification, escalation, mediation_style, message_templates)` per `contracts/prompt-bundle.md` §Hashing: SHA-256 over the fixed-order, null-byte-delimited concatenation including the `serbero/phase3\0` prefix. Return a lowercase hex string
- [X] T012 Implement `src/prompts/mod.rs::load_bundle(config: &PromptsConfig) -> Result<PromptBundle>`: read all five configured paths, assemble a `PromptBundle { id, policy_hash, system, classification, escalation, mediation_style, message_templates }` (where `id` defaults to `"phase3-default"`), and return a loud error if any file is missing or unreadable. No caching beyond the returned value; daemon re-loads on config reload
- [X] T013 [P] Declare the `ReasoningProvider` trait in `src/reasoning/mod.rs` exactly matching `contracts/reasoning-provider.md` §Trait Definition. Export the request/response re-exports from `src/models/reasoning.rs`. Provide a `build_provider(config: &ReasoningConfig) -> Result<Arc<dyn ReasoningProvider>>` factory that dispatches on `provider` and returns the `NotYetImplemented` stub for unsupported values without ever falling through to OpenAI
- [X] T014 [P] Implement `src/reasoning/not_yet_implemented.rs` — a `NotYetImplementedProvider { provider_name }` struct whose `classify` / `summarize` / `health_check` all return `ReasoningError::Unreachable(format!("{provider_name} not yet implemented in Phase 3"))` immediately, no network
- [X] T015 Implement `src/reasoning/openai.rs::OpenAiProvider`: one `reqwest::Client` with `request_timeout_seconds`, one `health_check` (a small-tokens completion or the models-list endpoint — whichever the target endpoint supports), JSON-mode request shaping for `classify`, plain-text with adapter-side parsing for `summarize`. Bearer token from the env-resolved credential. Bounded retry via a plain `for _ in 0..followup_retry_count` loop on transient errors; no `tokio-retry` dependency
- [X] T016 Implement `src/reasoning/health.rs::run_startup_health_check(provider: &dyn ReasoningProvider) -> Result<()>`: invoked once at daemon startup after `build_provider` and whenever config reload succeeds. Surface failure as a loud error; the daemon caller decides whether to halt Phase 3 (see T020)
- [X] T017 [P] Extend `src/daemon.rs` to accept the Phase 3 config surface. Guard all Phase 3 wiring behind `if cfg.mediation.enabled && cfg.reasoning.enabled`. When disabled, the daemon must start identically to the Phase 1/2 daemon (no Phase 3 tasks spawned, no `prompts/` required)
- [X] T018 Wire the Phase 3 startup path in `src/daemon.rs`: after Phase 1/2 initialization, call `prompts::load_bundle`, `reasoning::build_provider`, `reasoning::run_startup_health_check`. On any failure, log an ERROR identifying the failing component and leave Phase 1/2 running — do NOT exit the daemon (SC-105)
- [X] T019 Spawn the Phase 3 mediation background engine (`tokio::task`) from `src/daemon.rs` only after the startup health check passes. The engine task loop is implemented in `src/mediation/mod.rs::run_engine(...)` and is wired here with a shutdown channel tied into the existing shutdown path.
- [X] T020 [P] Inline unit test in `src/prompts/hash.rs` (`#[cfg(test)] mod tests`): byte-identical bundles produce identical hashes; changing one byte in any of the five files changes the hash; the null-byte delimiter actually disambiguates adjacent file contents
- [X] T021 [P] Inline unit test in `src/models/mediation.rs` (`#[cfg(test)] mod tests`): every transition allowed by the state-machine diagram returns `true`; every other transition returns `false`, including all self-transitions
- [X] T022 [P] Inline unit test in `src/models/config.rs` (`#[cfg(test)] mod tests`): a minimal Phase-3-enabled `config.toml` parses into the expected `MediationConfig` / `ReasoningConfig` / `PromptsConfig`; a Phase-3-disabled config leaves the daemon in Phase 1/2-only mode
- [X] T023 [P] Inline unit test in `src/reasoning/mod.rs` (`#[cfg(test)] mod tests`): `build_provider({provider: "anthropic", ...})` returns a `NotYetImplementedProvider`, not the OpenAI adapter; the trait-object calls fail with `ReasoningError::Unreachable` containing the provider name

**Checkpoint**: Foundation ready — user story implementation can now begin.

---

## Phase 3: User Story 1 — Open a mediation session for a low-risk dispute (Priority: P1) 🎯 MVP

**Goal**: When Phase 2 signals a mediation-eligible dispute and the
classification policy marks it as low-risk coordination, Serbero
follows the dispute-chat interaction flow used by current Mostro
clients, reconstructs the per-party chat-addressing key material, and
emits the first clarifying message via gift-wrapped chat events —
identifying itself as an assistance system, persisting a new
`mediation_sessions` row, and pinning the exact prompt-bundle version.

**Independent Test**: Phase 1/2 already detected a dispute; the
`MostroChatSim` test harness replays the subset of the dispute-chat
interaction flow Phase 3 exercises; `MockReasoningProvider` returns a
fixed `CoordinationFailureResolvable` classification. Assert: a
`mediation_sessions` row appears in state `awaiting_response` with the
expected `policy_hash`; exactly two outbound `mediation_messages` rows
exist (buyer + seller) addressed to their respective
`*_shared_pubkey`; each outbound inner event is decryptable with the
reconstructed shared keypair; the content contains the assistance-
system identification drawn from the prompt bundle.

### Tests for User Story 1

> Write the failing integration tests first, then implement the
> handler wiring until they pass. Unit tests are inline in the
> relevant source files.

**US1 slice status (2026-04-18)**: US1 audit infrastructure is now
in place. `MostroChatSim` / `MockReasoningProvider` /
`UnhealthyReasoningProvider` live in `tests/common/mod.rs` (T024,
T025). `src/db/rationales.rs` (T032) and `src/db/mediation_events.rs`
(T033) ship with typed constructors, and `open_session` now records
a `session_opened` event atomically with the session + outbound
rows. The auth-retry loop + its gates (T042, T043), the policy /
draft extraction (T038, T039), and the engine loop wiring (T040)
remain deferred to later US1 continuation slices; US2–US5 phases are
unchanged.

T043 (auth gate on `open_session`) is *not* included in this slice:
it requires the `Authorized` / `Unauthorized` / `Terminated` state
produced by T042's auth-retry task, and there is no honest source of
that state before T042 lands. Implementing T043 against a synthesized
placeholder would drag T042's orchestration in under a different name.

- [X] T024 [P] [US1] Extend `tests/common/mod.rs` with `MostroChatSim` — a helper that models the subset of the dispute-chat interaction flow Phase 3 exercises (verified against the Mostrix reference at fixture-definition time), exposes per-party chat-addressing key material to the test, publishes gift-wrapped chat events on the existing `MockRelay`, and provides helpers for buyer/seller replies with controllable inner `created_at` values — *shipped: `MostroChatSim` promoted from inline in `tests/phase3_session_open.rs` to `tests/common/mod.rs`. Public API: `start(relay_url, buyer_trade_pk, seller_trade_pk)` + `pubkey()`. Behavior preserved verbatim (NIP-59 `since(now - 7d)` window, `AdminTakeDispute` → `AdminTookDispute` reply via `send_private_msg`, `SolverDisputeInfo` built from the passed trade pubkeys). Controllable inner `created_at` for buyer/seller replies stays deferred — US2 party-reply simulation uses `outbound::build_wrap` directly from the tests that need it.*
- [X] T025 [P] [US1] Extend `tests/common/mod.rs` with `MockReasoningProvider` — an in-process adapter (implementing `serbero::reasoning::ReasoningProvider`) whose `classify` / `summarize` / `health_check` return scripted responses that tests can queue. Support both success and each `ReasoningError` variant — *shipped: two providers promoted to `tests/common/mod.rs`. `MockReasoningProvider { clarification }` returns `CoordinationFailureResolvable` + `AskClarification(self.clarification)` with confidence `0.9`; `summarize` returns `ReasoningError::Unreachable`; `health_check` returns `Ok(())`. `UnhealthyReasoningProvider` returns `ReasoningError::Unreachable` from `health_check` and panics on `classify`/`summarize` so any gate-bypass regression surfaces loudly. Full scripted-queue machinery (per-call queueing of arbitrary `ReasoningError` variants) is deferred to the first test that needs it.*
- [X] T026 [P] [US1] Integration test `tests/phase3_session_open.rs`: Phase 1/2 harness + `MostroChatSim` + `MockReasoningProvider` scripted to return `CoordinationFailureResolvable` with confidence 0.9. Boot Serbero with `[mediation].enabled = true` and a fixture prompt bundle. Publish a dispute event that Phase 2 transitions to `notified`. Assert the outcomes listed in the "Independent Test" above
- [X] T027 [P] [US1] Integration test `tests/phase3_session_open_gating.rs`: with `[reasoning].enabled = false` (or the provider scripted to return `Unreachable` on `health_check`), publishing a mediation-eligible dispute MUST NOT create a `mediation_sessions` row and MUST NOT emit any chat event; Phase 1/2 continues to notify solvers normally (SC-105) — *shipped: `tests/phase3_session_open_gating.rs` imports `UnhealthyReasoningProvider` from `tests/common/mod.rs` (promoted there under T025) and drives the T044 gate through it. The full `TestHarness` daemon proves Phase 1/2 solver notification still fires, and explicit `COUNT(*)` assertions keep `mediation_sessions`, `mediation_messages`, and `mediation_events` at zero rows while the gate is active.*
- [X] T028 [P] [US1] Inline unit test in `src/chat/shared_key.rs` (`#[cfg(test)] mod tests`): given fixture material representing what current Mostro / Mostrix produce in the dispute-chat flow, `reconstruct_party_keys(...)` yields `nostr_sdk::Keys` whose public key matches the expected per-party chat pubkey. The fixture is derived from the `MostroChatSim` harness, keeping code and tests in sync on what "current behavior" means
- [X] T029 [P] [US1] Inline unit test in `src/chat/outbound.rs` (`#[cfg(test)] mod tests`): `build_gift_wrap(...)` produces a `kind 1059` event with a `p` tag equal to the per-party chat pubkey, a `kind 1` inner event signed by the reconstructed shared keys, and NIP-44 encryption applied with the correct key material
- [X] T030 [P] [US1] Inline unit test in `src/db/mediation.rs` (`#[cfg(test)] mod tests`): inserting a `MediationSession` with `policy_hash` and `prompt_bundle_id` round-trips via `get_session(session_id)`; the unique index on `(session_id, inner_event_id)` rejects duplicate inbound inserts

### Implementation for User Story 1

- [X] T031 [US1] Implement `src/db/mediation.rs::insert_session(...)`, `get_session(...)`, `set_session_state(...)`, `list_open_sessions()`; `insert_message(...)` honoring the unique `(session_id, inner_event_id)` index; `update_last_seen_inner_ts(session_id, party, ts)`. Every mutation persists synchronously before the caller triggers the next Nostr / reasoning step (research R-106) — *US1 subset shipped: `insert_session`, `insert_outbound_message` (unique-index honored), `latest_open_session_for`. `set_session_state` / `list_open_sessions` / `update_last_seen_inner_ts` land with US2 ingest.*
- [X] T032 [US1] Implement `src/db/rationales.rs::insert_rationale(...)` and `get_rationale(rationale_id)`. Rationale id is the SHA-256 of the rationale text (content-addressed) per research R-107. Enforce that nothing else writes to `reasoning_rationales` — this table is the controlled audit store (FR-120) — *shipped: `insert_rationale` + `get_rationale` + a public `rationale_id_for(text)` for callers that need the id without touching the DB. `INSERT OR IGNORE` on the content-hash primary key makes re-inserting the same text idempotent (retry-friendly). Module docstring flags FR-120: general logs MUST reference by id only, never inline the raw text.*
- [X] T033 [US1] Implement `src/db/mediation_events.rs::record_event(kind, session_id_opt, payload_json, rationale_id_opt, bundle_pin_opt)`. Provide typed constructors for each `kind` value enumerated in `data-model.md` so call sites cannot mis-spell them — *shipped: enum `MediationEventKind` with all 15 variants from the data-model table, `record_event(...)` low-level helper returning the autoincremented id, plus three typed constructors currently used on US1 paths (`record_session_opened`, `record_outbound_sent`, `record_classification_produced`). The `session_opened` constructor is wired into `mediation::session::open_session` inside the same DB transaction as the session + outbound rows, so audit + state rise and fall together. The remaining 12 kinds (outbound_sent payload emission at publish time, state_transition, escalation_recommended, handoff_prepared, auth-retry events, etc.) land when their call sites land (US2 / US3 / US4).*
- [X] T034 [US1] Implement `src/chat/shared_key.rs::reconstruct_party_keys(...)` — takes the material yielded by the dispute-chat interaction flow and returns a per-party `nostr_sdk::Keys`. Follow the Mostrix `chat_utils.rs` pattern; cite the file in a doc comment and state the verification discipline in the module header — *shipped as `derive_shared_keys` + `keys_from_shared_hex`; module header states verification discipline against Mostrix `chat_utils.rs`.*
- [X] T035 [US1] Implement `src/chat/dispute_chat_flow.rs::run(...)` — the in-tree equivalent of the dispute-chat interaction flow used by current Mostro clients (per `contracts/mostro-chat.md` §Dispute Chat Key Reconstruction). Return per-party chat-key material; on failure return a `ChatTransport(...)` error. Doc comment MUST state that this code is verified against current Mostro / Mostrix behavior and MUST be updated whenever that behavior changes — *shipped as `run_take_flow`: performs `AdminTakeDispute` DM, fetches `AdminTookDispute` response, extracts `SolverDisputeInfo.buyer_pubkey`/`seller_pubkey`, derives per-party shared keys. `since(now - 7d)` window covers NIP-59 timestamp tweak.*
- [X] T036 [US1] Implement `src/chat/outbound.rs::send_mediation_message(client, shared_keys, content, extra_tags) -> Result<EventId>` — build the NIP-44 `kind 1` inner event signed by the per-party shared keys, wrap in NIP-59 gift-wrap (`kind 1059`) with `p` tag = shared pubkey, publish via the existing `nostr_sdk::Client`. Never address outbound mediation content to a party's primary pubkey; the function signature MUST make this impossible — *shipped as `send_chat_message` + `build_wrap`. Note: the inner `kind 1` is signed by Serbero's sender keys (not by the shared keys) per Mostrix `send_admin_chat_message_via_shared_key`; the shared keys are used only as the `p`-tag target and as the NIP-44 ECDH counterpart (ephemeral ↔ shared), matching current Mostro client behavior.*
- [X] T037 [US1] Implement `src/mediation/session.rs::open_session(ctx, dispute_id) -> Result<MediationSession>` — gate on auth (T042), reasoning (T044), and the Phase 2 mediation-eligibility state; call `dispute_chat_flow::run`, persist the `MediationSession` row with `prompt_bundle_id` / `policy_hash` pinned from the currently loaded bundle, and record `session_opened` in `mediation_events` — *shipped: reasoning-health fast-path gate (T044), existing-session gate, take-flow call, classification call, per-party outbound dispatch, transactional persist of the session row + two outbound message rows + the `session_opened` audit event (via T033 `record_session_opened`) all in one DB lock scope. Auth gate (T042/T043) still deferred.*
- [X] T038 [US1] Implement `src/mediation/policy.rs::initial_classification(ctx, session) -> Result<ClassificationResponse>` — build the `ClassificationRequest` from the prompt bundle and session state, call `ReasoningProvider::classify`, run the policy-layer validation rules from `contracts/reasoning-provider.md` §Policy-Layer Validation, persist the full rationale to `reasoning_rationales`, and emit the `classification_produced` event. Return only the policy-validated result; the mediation engine never sees raw model output without validation
- [X] T039 [US1] Implement `src/mediation/mod.rs::draft_and_send_initial_message(ctx, session, classification)` — use the classification plus message-templates from the bundle to draft an outbound message per party (buyer + seller); send via `chat/outbound.rs`; insert `outbound` rows in `mediation_messages` with `inner_event_id` equal to the signed inner event id; emit `outbound_sent` events; transition session state from `opening` to `awaiting_response`
- [X] T040 [US1] Wire US1 end to end in `src/mediation/mod.rs::run(...)` (the engine task spawned by T019): scan for Phase 2 disputes that have entered the mediation-eligible state and do not yet have an open `mediation_sessions` row; call `open_session` followed by `initial_classification` followed by `draft_and_send_initial_message`. On any `ReasoningError::Unreachable`, escalate the session immediately (anticipates US4)
- [X] T041 [US1] Structured `tracing` instrumentation for the US1 path: one span per session with `dispute_id` / `session_id` / `prompt_bundle_id` / `policy_hash`; events `session_opened`, `classification_produced` (classification + confidence + `rationale_id` — **never** the full rationale text per FR-120), `outbound_sent` (per-party `shared_pubkey` + `inner_event_id`) — *shipped: `#[instrument]` on `open_session` with `dispute_id` field, `info!`/`debug!`/`warn!` events for session-open, already-open skip, deferred-action, and the final session-opened confirmation with `prompt_bundle_id`/`policy_hash`. Classification-rationale event awaits T038; inbound ingest events await US2.*

**Scope guards for US1**:

- [X] T042 [US1] Implement `src/mediation/auth_retry.rs::ensure_authorized_or_enter_loop(ctx)` — a single `tokio::task` that runs the startup verification once, and on failure enters the bounded revalidation loop (initial 60s → doubling up to 3600s, terminate at min(86400s, 24 attempts)), emits exactly one WARN-or-higher alert on terminal termination, and records `auth_retry_attempt` / `auth_retry_terminated` / `auth_retry_recovered` events. Must not grow into a state machine beyond `Authorized` / `Unauthorized` / `Terminated` (plan scope-control note)
- [X] T043 [US1] Gate US1 session opens on the auth-retry task's current state: while `Unauthorized`, `open_session` refuses; while `Authorized`, it proceeds; while `Terminated`, it refuses AND Phase 1/2 remains unaffected. Document the gate in `src/mediation/session.rs`
- [X] T044 [US1] Gate US1 session opens on reasoning-provider reachability: if the latest `health_check` failed, `open_session` refuses and Phase 1/2 continues normally (SC-105). Reflect the gate in a dedicated fast-path check so the test from T027 can assert it deterministically — *shipped as step (0) in `src/mediation/session.rs::open_session`: a per-call `health_check` runs before any relay I/O or DB work; on failure returns `OpenOutcome::RefusedReasoningUnavailable { reason }` with a `warn!` event and no rows written. Per-call (not cached) because US1 has no running engine loop to own a cached result; the shape is naturally cheap (real adapters implement `health_check` as a small-tokens or models-list request).*

**Checkpoint**: US1 complete — Phase 3 MVP. Serbero opens a mediation session, emits the first clarifying message over the Mostro chat transport, and pins the prompt bundle that governed it.

---

## Phase 4: User Story 2 — Collect responses and maintain session state (Priority: P1)

**Goal**: Ingest gift-wrapped party replies, decrypt/verify against
the reconstructed chat keys, persist verbatim, dedup by inner event
id, advance round counts and last-seen markers, and make every open
session cleanly resumable from SQLite alone after a daemon restart.

**Independent Test**: Starting from a US1 session in
`awaiting_response`, simulate a buyer and a seller reply via
`MostroChatSim`. Assert: decrypted content is persisted verbatim in
`mediation_messages`; re-publishing the same event does not create a
second row; the session's `round_count` advances on the round boundary;
a `last_seen_inner_ts` update prevents re-ingest on restart; restart
the daemon mid-session and assert open sessions resume with all
counters intact.

**US2 slice status (2026-04-18)**: inbound ingest happy-path +
dedup shipped. The remaining US2 tasks — restart-resume dedup test
(T046), stale-message test (T047), engine-driven ingest tick
(T051), restart-resume loop + policy-bundle-missing escalation
(T052), shared-key restart helper (T053), and the full US2 tracing
pass (T054) — are deferred to subsequent US2 continuation slices.
The ingest helpers (`fetch_inbound`, `ingest_inbound`) are
callable today but not yet driven from a periodic engine tick.

### Tests for User Story 2

- [X] T045 [P] [US2] Integration test `tests/phase3_response_ingest.rs`: open a US1 session, have `MostroChatSim` publish one buyer reply and one seller reply; assert two inbound `mediation_messages` rows with direction `inbound` and `content` matching the decrypted inner event; round counter advanced; `buyer_last_seen_inner_ts` and `seller_last_seen_inner_ts` updated to the inner `created_at` — *shipped with one honest deviation: the test seeds the open session row directly (FK-valid) rather than running the full US1 take-flow. That keeps US2 ingest coverage focused on T045 / T049 / T050 and avoids dragging in `MostroChatSim` transport plumbing. The party-reply simulator uses `outbound::build_wrap` with the buyer / seller trade keys, which mirrors Mostrix `send_user_order_chat_message_via_shared_key` — the inner `kind 1` is signed by the trade keys, not the shared keys. Replay is exercised in the same test to pin dedup; the dedicated restart-resume test stays T046.*
- [X] T046 [P] [US2] Integration test `tests/phase3_response_dedup_restart.rs`: publish the same inbound event twice — assert exactly one row, no double round count; restart the daemon (shutdown + spawn again) with the same `SERBERO_DB_PATH`; republish the same event; assert still exactly one row, session `round_count` unchanged
- [X] T047 [P] [US2] Integration test `tests/phase3_stale_message.rs`: publish an inbound message whose inner `created_at` predates the session's current last-seen marker; assert the row is persisted with `stale = 1` and does NOT advance session state
- [X] T048 [P] [US2] Inline unit test in `src/chat/inbound.rs` (`#[cfg(test)] mod tests`): `unwrap_and_verify(gift_wrap, shared_keys)` extracts the inner event, checks its signature, and returns the authoritative `(inner_event_id, created_at, content)` tuple; tampered inner events fail verification; outer gift-wrap timestamps are ignored — *shipped against the existing `unwrap_with_shared_key` function (name retained from the US1 slice; the T048 description uses `unwrap_and_verify` as a logical label). Three tests cover: inner signer vs. outer ephemeral signer, tampered ciphertext rejection, and rejection of inner events whose declared pubkey does not match the signing key.*

### Implementation for User Story 2

- [X] T049 [US2] Implement `src/chat/inbound.rs::fetch_inbound(client, session) -> Vec<InboundEnvelope>` — subscribe (or poll on `[chat].inbound_fetch_interval_seconds`) for `kind 1059` events with `p` equal to either party's `shared_pubkey`; unwrap with the session's shared keys; verify the inner event; return envelopes ordered by inner `created_at` — *shipped as `fetch_inbound(client, parties, fetch_timeout)`: one short-lived `fetch_events` call per party shared pubkey (7-day `since` window to cover the NIP-59 tweak), unwrap each candidate with the matching shared keys, drop individual events that fail decrypt/verify at `warn!` (the batch still returns), sort ascending by inner `created_at`. The `session` handle in the task description was replaced with a narrower `PartyChatMaterial` slice so the helper stays usable without loading a full typed session struct (no such struct exists yet — that's US2+ scope). Subscription-based polling on `[chat].inbound_fetch_interval_seconds` ties to the engine tick (T051), which is deferred.*
- [X] T050 [US2] Implement `src/mediation/session.rs::ingest_inbound(ctx, session, envelope)` — insert into `mediation_messages` with direction `inbound`, honoring the unique `(session_id, inner_event_id)` index (duplicate → noop); compare inner `created_at` against the per-party last-seen marker and set `stale = 1` when the inner timestamp predates it; on first-time ingest from a party, call `update_last_seen_inner_ts` and advance `round_count` at complete-round boundaries (one buyer + one seller reply per round unless policy says otherwise) — *shipped as `ingest_inbound(conn, session_id, envelope) -> IngestOutcome`. Behavior: reads the per-party last-seen marker, decides `stale`, opens a transaction, `INSERT OR IGNORE` (dedup), and on fresh non-stale persist, updates the per-party `*_last_seen_inner_ts` and recomputes `round_count` from the transcript via `db::mediation::recompute_round_count` (`min(fresh_buyer_inbound, fresh_seller_inbound)`). Does NOT transition session state — that is policy-layer scope.*
- [X] T051 [US2] Implement `src/mediation/mod.rs::run_ingest_tick(ctx)` called by the engine task's periodic loop: iterate every open session (`list_open_sessions`), call `fetch_inbound` then `ingest_inbound` per returned envelope, and let the policy layer (US3/US4) drive state transitions after ingestion
- [X] T052 [US2] Implement restart-resume in `src/mediation/mod.rs::run(...)`: on engine startup, load all sessions with non-terminal `state`, re-bind them to the loaded `prompts::load_bundle` whose hash matches the session's `policy_hash` (if the pinned bundle is no longer available, emit an actionable ERROR and mark the session `escalation_recommended` with trigger `policy_bundle_missing`), and resume the ingest / policy loop from the persisted last-seen markers
- [X] T053 [US2] Extend the `src/chat/dispute_chat_flow.rs` module with `load_chat_keys_for_session(session)` so restart can rebuild in-memory chat-key material without re-running the full dispute-chat interaction flow (data-model.md: only the derived `*_shared_pubkey` fields are persisted; raw secret is in-process). Document the discipline at the top of the file
- [X] T054 [US2] Structured `tracing` for the US2 path: one span per ingest tick with counts; `inbound_ingested` events with `session_id` / `party` / `inner_event_id` / `inner_created_at`; `state_transition` events when `ingest_inbound` changes state; `stale=true` rows logged at `debug`

**Checkpoint**: US2 complete — multi-round mediation conversations are durable, deduplicated, stale-safe, and restart-resumable.

---

## Phase 5: User Story 3 — Summarize a cooperative resolution for the assigned solver (Priority: P2)

**Goal**: When the mediation converges on a cooperative resolution
(both parties responsive, facts aligned, classification =
`CoordinationFailureResolvable` with sufficient confidence), call the
reasoning provider for a structured summary, route the DM via the
Phase 1/2 notifier per the Solver-Facing Routing section (targeted to
`disputes.assigned_solver` if set, broadcast otherwise), and persist
a `mediation_summaries` row pinning the same `policy_hash`.

**Independent Test**: Drive a two-round cooperative session via
`MostroChatSim`. Scripted `MockReasoningProvider` returns a
classification with `suggested_action = Summarize`, then a
`SummaryResponse`. Assert: exactly one `mediation_summaries` row for
the session, referencing the correct `rationale_id`; a Phase 1/2
notifier call delivers the summary DM following the routing rule —
targeted when `disputes.assigned_solver` is set, broadcast otherwise;
session transitions `classified → summary_pending → summary_delivered
→ closed`.

### Tests for User Story 3

- [X] T055 [P] [US3] Integration test `tests/phase3_cooperative_summary.rs`: direct-seed open-time cooperative flow (empty transcript — the two-round US1+US2 variant lands with a later slice when US2's in-memory transcript can feed the summarizer on restart); scripted `SummaryOnlyProvider` returns a `SummaryResponse`; assert the `mediation_summaries` row (rationale_id = SHA-256 of rationale text), targeted notifier delivery with `status = 'sent'`, session-state progression to `closed`, and the `summary_generated` audit event referencing the rationale id without inlining its text (FR-120)
- [X] T056 [P] [US3] Integration test `tests/phase3_routing_model.rs`: with `disputes.assigned_solver` set via a simulated Phase 2 `s=in-progress`, assert the summary DM routes ONLY to that solver; without `assigned_solver`, assert it broadcasts to every configured solver; with assignment flipping mid-session, assert the next notification switches to targeted
- [X] T057 [P] [US3] Inline unit test in `src/mediation/router.rs` (`#[cfg(test)] mod tests`): `resolve_recipients(solvers_cfg, assigned_solver_opt)` returns `Targeted(pubkey)` when `assigned_solver` is `Some`, `Broadcast(all_configured)` otherwise

### Implementation for User Story 3

- [X] T058 [US3] Implement `src/mediation/router.rs::resolve_recipients(solvers: &[SolverConfig], assigned_solver: Option<&str>) -> Recipients` per the spec's Solver-Facing Routing section. This is the only place routing is decided; all notifications flow through it
- [X] T059 [US3] Implement `src/mediation/summarizer.rs::summarize(ctx, session) -> Result<MediationSummary>` — assemble a `SummaryRequest` from the transcript + classification; call `ReasoningProvider::summarize`; run the policy-layer validation rules (reject authority-boundary attempts); persist `mediation_summaries` with `policy_hash` pinned; persist the rationale via `db::rationales::insert_rationale`; emit `summary_generated`
- [X] T060 [US3] Wire the summary-delivery path in `src/mediation/mod.rs`: when `policy::evaluate(classification)` returns `Summarize`, transition session `classified → summary_pending`, call `summarizer::summarize`, resolve recipients via `router::resolve_recipients`, deliver via the existing Phase 1/2 `notifier` with a Phase 3 notification type `mediation_summary`, then transition `summary_pending → summary_delivered → closed`
- [X] T061 [US3] Extend `src/models/notification.rs` and `src/db/notifications.rs` with a new `NotificationType::MediationSummary` variant and its `"mediation_summary"` SQL text form. Do NOT otherwise alter the Phase 1/2 notifier — Phase 3 reuses it verbatim
- [X] T062 [US3] Structured `tracing` for the US3 path: `summary_generated` and a `solver_summary_delivered` event per recipient; the rationale is referenced by id, not inlined (FR-120)

**Checkpoint**: US3 complete — cooperative low-risk disputes produce a ready-to-close artifact for the human solver without Serbero ever executing the close.

---

## Phase 6: User Story 4 — Detect escalation triggers and prepare a Phase 4 handoff (Priority: P2)

**Goal**: Any of the seven escalation triggers transitions the session
to `escalation_recommended`, persists the trigger + evidence, assembles
a structured Phase 4 handoff package, and stops sending further
clarifying messages to parties on that session. Phase 4 execution
mechanics are deliberately NOT implemented here.

**Independent Test**: For each of the seven triggers (conflicting
claims, fraud indicator, low confidence, party unresponsive past
timeout, round-limit reached, reasoning unavailable, authorization
lost), script the inputs that produce that trigger and assert: session
transitions to `escalation_recommended`; `mediation_events` records the
trigger kind and evidence refs; a `handoff_prepared` event records the
Phase 4 package reference; no further outbound chat messages to
parties; the Phase 1/2 solver notifier surfaces a "needs human
judgment" message per the Solver-Facing Routing rule.

### Tests for User Story 4

- [X] T063 [P] [US4] Integration test `tests/phase3_escalation_triggers.rs` with a sub-test per trigger:
  - `conflicting_claims_triggers_escalation`
  - `fraud_indicator_triggers_escalation`
  - `low_confidence_triggers_escalation`
  - `party_unresponsive_timeout_triggers_escalation`
  - `round_limit_triggers_escalation`
  - `reasoning_unavailable_triggers_escalation`
  - `authorization_lost_mid_session_triggers_escalation`
  Each sub-test scripts the input condition and asserts the outcomes enumerated in the "Independent Test" above
- [X] T064 [P] [US4] Integration test `tests/phase3_authority_boundary.rs`: the scripted reasoning response contains a "settle via admin-settle" suggestion; assert the policy layer suppresses it, the session escalates with trigger `authority_boundary_attempt`, a `Flag::AuthorityBoundaryAttempt` is recorded in the event payload, and NO outbound chat message is sent
- [X] T065 [P] [US4] Inline unit test in `src/mediation/policy.rs` (`#[cfg(test)] mod tests`): each rule in `contracts/reasoning-provider.md` §Policy-Layer Validation maps to an `EscalationTrigger`; the suppression path for authority-boundary attempts returns `Escalate` regardless of other flags

### Implementation for User Story 4

- [X] T066 [US4] Implement `src/mediation/policy.rs::evaluate(classification_or_summary) -> PolicyDecision` — returns one of `AskClarification(String)`, `Summarize`, `Escalate(EscalationTrigger)`. Implements the seven validation rules from the reasoning-provider contract. Authority-boundary suggestions (fund actions, dispute closure) are always suppressed and escalated
- [X] T067 [US4] Implement `src/mediation/escalation.rs::recommend(ctx, session, trigger, evidence_refs)` — transition the session to `escalation_recommended`; record `escalation_recommended` in `mediation_events` with the trigger and evidence refs; assemble the Phase 4 handoff package (dispute id, session id, trigger, transcript summary reference, `prompt_bundle_id`, `policy_hash`, rationale refs); persist a `handoff_prepared` event with the package reference; stop sending further clarifying messages on this session
- [X] T068 [US4] Implement the round-limit trigger: `src/mediation/session.rs::check_round_limit(session, max_rounds)` — called after each `ingest_inbound` and each `evaluate` call; when `round_count >= max_rounds` without convergence, invoke `escalation::recommend(.., trigger = RoundLimit, ..)`
- [X] T069 [US4] Implement the party-response timeout trigger: schedule a per-session timeout based on `[mediation].party_response_timeout_seconds` using a sentinel timestamp in `mediation_sessions`; on each engine tick, check for expired sessions and escalate with trigger `party_unresponsive`
- [X] T070 [US4] Implement the reasoning-unavailable trigger: when `ReasoningProvider::classify` or `summarize` exhausts `[reasoning].followup_retry_count`, escalate the current session with trigger `reasoning_unavailable` (plan degraded-mode table)
- [X] T071 [US4] Implement the authorization-lost trigger: when `chat/outbound.rs::send_mediation_message` returns an auth-related failure, escalate the session with trigger `authorization_lost` AND re-enter the auth-retry loop (from T042) so future sessions are gated until revalidation succeeds
- [X] T072 [US4] Wire the solver-facing "needs human judgment" notification: on transition to `escalation_recommended`, deliver a gift-wrap DM via the Phase 1/2 notifier using `router::resolve_recipients` per the Solver-Facing Routing rule, with a new `NotificationType::MediationEscalationRecommended` (register it alongside `MediationSummary` in T061). Do NOT execute any Phase 4 routing — Phase 4 consumes the persisted handoff package later
- [X] T073 [US4] Extend `src/models/mediation.rs::EscalationTrigger` to cover: `ConflictingClaims`, `FraudIndicator`, `LowConfidence`, `PartyUnresponsive`, `RoundLimit`, `ReasoningUnavailable`, `AuthorizationLost`, `AuthorityBoundaryAttempt`, `MediationTimeout`, `PolicyBundleMissing`. Every constructor path from T066–T071 MUST use one of these variants
- [X] T074 [US4] Structured `tracing` for the US4 path: `escalation_recommended` with `trigger` + evidence refs; `handoff_prepared` with the package ref; `authorization_lost` with the underlying error; `reasoning_call_failed` with provider/model/attempt count

**Checkpoint**: US4 complete — disputes that do not belong in guided mediation leave Phase 3 promptly, with a clean, auditable handoff package ready for Phase 4.

---

## Phase 7: User Story 5 — Operator swaps the reasoning provider endpoint without code changes (Priority: P3)

**Goal**: Within shipped Phase 3 scope, swapping the reasoning endpoint
across OpenAI-compatible targets (different `api_base`, different
`api_key_env`, same `provider = "openai"`) takes effect on restart
with no code change. Selecting a not-yet-implemented provider
(`anthropic`, `ppqai`, `openclaw`) fails at startup with an actionable
error rather than silently coercing to OpenAI (SC-104, FR-103).

**Independent Test**: Run the same mediation fixture against the
default OpenAI config and then against an OpenAI-compatible config
pointing at a different `api_base` / different env-var-sourced
credential; assert identical session state transitions and summary
shape with differing outbound HTTP surfaces. Separately assert that
`provider = "anthropic"` refuses to start Phase 3.

### Tests for User Story 5

- [X] T075 [P] [US5] Integration test `tests/phase3_provider_swap.rs`: run the same cooperative fixture twice — once with `provider = "openai"` pointing at `httpmock` on `http://127.0.0.1:PORT_A/v1`, once with the same `provider = "openai"` pointing at `http://127.0.0.1:PORT_B/v1` (different env var for the key). Assert: identical session state, summary shape, and mediation messages; differing outbound HTTP captured by each `httpmock`. No code rebuild between runs
  - **Shipped subset**: two `httpmock` servers on distinct ports; two providers constructed with different `api_base`/`api_key_env`; `health_check` dispatched on each adapter and each mock observed exactly one request — pins the portability invariant (adapter reads `api_base` at call time, does not silently coerce to NYI). Also pins `"openai-compatible"` routes to the OpenAI adapter via the same HTTP-observed path.
  - **TODO (deferred beyond US5)**: full end-to-end cooperative fixture (classify → summarize → solver DM) run against each mock with per-run outbound HTTP payload capture and assertions on `mediation_sessions` / `mediation_messages` identity. Requires the full MostroChatSim + solver listener harness already used by the Phase 3 session-open tests; folding that into a per-mock double-run is a meaningful test-infrastructure effort tracked separately.
- [X] T076 [P] [US5] Integration test `tests/phase3_provider_not_yet_implemented.rs`: with `provider = "anthropic"` (and separately `"ppqai"`, `"openclaw"`), Phase 3 MUST fail `reasoning::run_startup_health_check` with an actionable error naming the provider; no `mediation_sessions` row is ever created; Phase 1/2 detection and notification continue unaffected (SC-105)
  - **Shipped subset**: drives `reasoning::health::run_startup_health_check` (the exact public function `daemon::phase3_bring_up` invokes) for every NYI provider and asserts the actionable-error shape (name + shipped list + future-work). Verifies a freshly migrated DB has zero `mediation_sessions` rows AND an intact `disputes` table (SC-105 at the schema level).
  - **TODO (deferred beyond US5)**: spawn the real `daemon::run_with_shutdown` against an NYI config and observe that the engine task is never spawned while a Phase 1/2 dispute flows end-to-end. `phase3_bring_up` is private; a daemon-driven test needs either a targeted test hook or reuse of the existing Phase 1/2 harness (`TestHarness` + `spawn_daemon`). Tracked separately.
- [X] T077 [P] [US5] Inline unit test in `src/reasoning/openai.rs` (`#[cfg(test)] mod tests`): credential is read from the `api_key_env`-named env var, not from any config field; request is sent to `api_base`; request timeout is `request_timeout_seconds`; a transient error triggers retry up to `followup_retry_count` before surfacing `ReasoningError::Unreachable`

### Implementation for User Story 5

- [X] T078 [US5] Make the OpenAI adapter `api_base`-parametric: `OpenAiProvider::new(ReasoningConfig)` uses `cfg.api_base` directly in every URL (no hardcoded OpenAI host), uses `cfg.model`, `cfg.request_timeout_seconds`, and `cfg.followup_retry_count`. Documentation comment names this as the OpenAI-compatible portability surface shipped in Phase 3
- [X] T079 [US5] Surface `run_startup_health_check` failures loudly: `src/daemon.rs` MUST log an ERROR citing `provider`, `model`, `api_base`, and the underlying error kind, then leave `mediation.enabled` effectively off for this run. It MUST NOT exit the daemon (Phase 1/2 keeps running)
- [X] T080 [US5] Update the `NotYetImplementedProvider` to produce an error string explicitly listing which providers are currently shipped (`openai`, `openai-compatible`), so operators see a clear "landing other adapters is future work" message rather than a bare "not implemented"

**Checkpoint**: US5 complete — the shipped portability surface is configuration-only across OpenAI-compatible endpoints; unshipped adapters fail loudly.

---

## Phase 8: Polish & Cross-Cutting Concerns

**Purpose**: Final validation and hygiene before declaring Phase 3
ready to review.

- [X] T081 [P] Fill in the Phase 3 prompt bundle files under `prompts/phase3-*.md` with actual mediation identity, classification criteria, escalation policy, mediation style, and message templates — matching the constraints in spec §AI Agent Behavior Boundaries (assistance-only identity, no fund authority, explicit honesty / uncertainty, allowed / disallowed outputs)
- [X] T082 [P] Validate the `quickstart.md` Phase 3 flows end to end against the built `./target/release/serbero` binary using a local `MockReasoningProvider` backing (operator-facing smoke test). Update `quickstart.md` if any command or log line drifts
- [X] T083 [P] Run `cargo clippy --all-targets --all-features -- -D warnings` and `cargo fmt --all -- --check`; fix findings. Expect Phase 3 lifetime / async warnings around the reasoning adapter to flush out early
- [X] T084 [P] Run `cargo test --all-targets` including the full Phase 3 integration suite; expect 22 Phase 1/2 unit + 8 Phase 1/2 integration + the new Phase 3 unit and integration tests to all pass
- [X] T085 Cross-check `spec.md` SC-102 (audit claim: "Zero dispute-closing actions are executed by Serbero"): grep the full codebase for `admin-settle` / `admin-cancel` / fund-moving tokens — the count MUST remain zero in **executable code paths** (functions, methods, modules that run at runtime) under `src/`. Matches inside doc comments, descriptive error variants, the `AUTHORITY_BOUNDARY_PHRASES` suppression array (and the tests covering it), or any other non-executable string literal used purely for detection or documentation are permitted. Any `rg` hit in code that would actually issue or recommend such an action is a red flag and must be reviewed before merge.
- [X] T086 Verify `spec.md` SC-103 (auditability): run the quickstart cooperative fixture, then confirm that every row in `mediation_sessions`, `mediation_events`, `mediation_summaries`, and `reasoning_rationales` produced during the fixture carries a non-null, consistent `policy_hash` and `prompt_bundle_id`
- [X] T087 Verify `spec.md` SC-107 (transport): using the integration test harness, grep the outbound event stream for any `kind 4` / `kind 1059` gift-wrap whose `p` tag equals a party's primary pubkey. Expected count: zero
- [X] T088 Manual observability pass: walk a full cooperative session, a full escalation session, a reasoning-unavailable session, and an auth-retry termination through `tracing` logs + the SQLite audit tables. Confirm FR-120 (rationales only as reference ids in general logs) and FR-117 (restart resume) visually. Capture any drift back into the relevant story tasks

---

## Phase 9: User Story 6 — External dispute resolution detection (Priority: P2)

**Goal**: When a dispute resolves externally while Serbero is mediating
(cooperative release or solver action via Mostro), Serbero detects the
resolution, cleanly closes any active mediation session, and sends an
informational resolution report to the solver(s). No escalation, no
handoff — the dispute is already resolved.

### Tests for User Story 6

- [X] T095 [P] [US6] Integration test `tests/phase3_superseded_by_human.rs`

### Implementation for User Story 6

- [X] T089 [US6] Extend `src/models/dispute.rs::DisputeStatus` with `SellerRefunded`, `Settled`, and `Released`
- [X] T090 [US6] Extend `src/dispatcher.rs` to route `s=seller-refunded` / `s=settled` / `s=released` to `src/handlers/dispute_resolved.rs` via `RESOLUTION_STATUSES`
- [X] T091 [US6] Implement `src/handlers/dispute_resolved.rs`
- [X] T092 [US6] Resolution report notification helper in `src/mediation/mod.rs`
- [X] T093 [US6] `NotificationType::MediationResolutionReport`
- [X] T094 [US6] Reuse `HandlerContext` and read `prompt_bundle_id` / `policy_hash` from the pinned `mediation_sessions` row
- [X] T096 [US6] Update `specs/003-guided-mediation/tasks.md` with the US6 section

**Checkpoint**: US6 complete — externally resolved disputes close cleanly
with an informational report. No mediation activity continues after
resolution.

---

## Phase 10: Mediation Start-Flow Correction (FR-121 / FR-122 / FR-123 / FR-124)

**Purpose**: Close the gap between the merged US1–US6 implementation
and the intended Phase 3 product behavior, as documented in the
2026-04-20 spec amendments. In the current code, (A) mediation starts
on a periodic engine tick rather than from the dispute-detection
event, (B) `TakeDispute` runs before the reasoning verdict, (C)
eligibility is pinned to `lifecycle_state = 'notified'`, and (D) the
final solver-facing report only fires when an active session row
exists. This phase corrects all four, without regressing the already-
shipped user stories.

**Entry criteria**: US1–US6 complete (all T001–T096 marked `[X]`).
**Exit criteria**: SC-109, SC-110, SC-111 empirically pass; no single-
state eligibility check remains in `src/`; the event-driven start is
reachable with the engine tick disabled.

### Setup / shared infrastructure

- [X] T097 Extend `MediationEventKind` (lives in `src/db/mediation_events.rs:23`, not `src/models/mediation.rs`) with the new kinds defined in `data-model.md` §mediation_events: `StartAttemptStarted`, `StartAttemptStopped`, `ReasoningVerdict`, `TakeDisputeIssued`, `ResolvedExternallyReported`. Update `as_str()`, the `Display` impl, and the round-trip table test to cover them. Add typed constructors in the same file (`record_start_attempt_started`, `record_start_attempt_stopped`, `record_reasoning_verdict`, `record_take_dispute_issued`, `record_resolved_externally_reported`) so call sites cannot mis-spell them. Payload shapes match `data-model.md` exactly. All constructors accept `Option<&str>` for `session_id` (nullable — `mediation_events.session_id` is already nullable per `migrations.rs:220`), so they work for both dispute-scoped (pre-session) and session-scoped events.
- [ ] ~~T098~~ **[merged into T097 — redundant]** The codebase encodes "outcomes" as event-kind + session-state, not as a standalone `MediationOutcome` enum. T097's `ResolvedExternallyReported` event kind plus the existing `SupersededByHuman → Closed` transition (when a session row exists) are sufficient. No separate outcome variant needed. The spec's "Allowed Phase 3 outcomes" list in `spec.md` is conceptual documentation; it does not map to a code enum.

### A + C — composed eligibility + event-driven start (FR-121, FR-123)

- [X] T099 [P] Implement `src/mediation/eligibility.rs::is_mediation_eligible(conn, dispute_id) -> Result<bool>` computing the composed predicate from FR-123: non-resolved `disputes.lifecycle_state`, no active (non-terminal) `mediation_sessions` row, no `escalation_recommended` session, not `superseded_by_human`. Inline unit tests cover every branch (one eligible case + one rejected case per branch). No SQL `lifecycle_state = 'notified'` literal anywhere in the file. **Known limitation to address in a follow-up**: `Taken` is eligible without verifying `disputes.assigned_solver`, so a dispute taken by a human solver is also eligible for Serbero's mediation attempt; the take-flow will time out on Mostro-side (wasted 15 s + reasoning cost) but no incorrect side effect is committed. Properly gating `Taken` by `assigned_solver IS NULL OR = serbero_pubkey` needs Serbero's pubkey threaded into the predicate and is tracked as a follow-up task.
- [X] T100 Refactor `src/mediation/mod.rs::list_eligible_disputes` to call `eligibility::is_mediation_eligible` per candidate, removing the `WHERE d.lifecycle_state = 'notified'` literal. Keep the "no active session / not escalated" subquery only as a pre-filter index-friendly shortcut; the composed predicate is authoritative. Add a regression test that a dispute in a non-`notified` but non-resolved state with no active session is included in the eligible set.
- [X] T101 Implement `src/mediation/start.rs::try_start_for(ctx, dispute_id) -> Result<StartOutcome>` — the event-driven entry point. Flow: (1) check `is_mediation_eligible`; if false, record `start_attempt_stopped{reason: "ineligible"}` and return `NotEligible`. (2) record `start_attempt_started{trigger: "detected"}`. (3) delegate to the reworked `session::open_session` (T104). (4) translate the open-session result into a `StartOutcome` (`Started(session_id)`, `StoppedBeforeTake{reason}`, `TakeFailed{reason}`, `Error`). Return synchronously; do not spawn a task. The tick path (T100) reuses the same function with `trigger: "tick_retry"`.
- [X] T102 Wire `src/handlers/dispute_detected.rs` to call `mediation::start::try_start_for` synchronously after the existing persist + Phase 1/2 solver-notification steps. Failures of the start attempt MUST NOT abort or retry the solver-notification flow (they are independent paths). Log at `warn!` on `StartOutcome::TakeFailed` / `Error`, at `info!` on `NotEligible`, and at `info!` on `Started`. Do not introduce a tokio channel or background task here — the call is in-path.
- [X] T103 [P] Integration test `tests/phase3_event_driven_start.rs` (SC-109): **handler-only** test — no daemon is spawned, the engine task never runs. Invokes `dispute_detected::handle(&ctx, &event)` directly with `HandlerContext.phase3 = Some(..)` fully populated. Asserts: `mediation_sessions` has exactly one row for the dispute in `awaiting_response`, `mediation_messages` has exactly two outbound rows (buyer + seller), and `mediation_events` carries one `start_attempt_started` with `trigger = "detected"` and zero rows with `trigger = "tick_retry"`. The "no tick_retry row" assertion is the empirical proof that the tick is NOT required for new-dispute handling — stronger than booting the daemon with a long `ENGINE_TICK_INTERVAL` (the original task wording) because the tick physically cannot fire here.

### B — take strictly coupled to reasoning (FR-122)

- [ ] T104 Rework `src/mediation/session.rs::open_session` to enforce the FR-122 ordering exactly: (0) composed eligibility — reuses `eligibility::is_mediation_eligible`; (1) reasoning-provider health gate (existing fast-path from T044); (2) reasoning verdict — call the new `policy::classify_for_start(dispute_id, ...)` BEFORE any chat-transport or take step (see T104a); persist the rationale with `session_id = NULL` (the `reasoning_rationales.session_id` column is already nullable — `migrations.rs:185`); record `reasoning_verdict` **dispute-scoped** (`session_id = NULL`, payload carries `dispute_id`); on negative verdict, record `start_attempt_stopped{reason: "reasoning_verdict_negative"}` and return early with no session row; (3) issue `TakeDispute` via `chat::dispute_chat_flow::run_take_flow`; on failure, record `take_dispute_issued{outcome: "failure", reason}` (dispute-scoped) and return early (no session row); (4) on take success, record `take_dispute_issued{outcome: "success"}`, insert the `mediation_sessions` row (with `prompt_bundle_id` / `policy_hash` pinned), and retroactively stamp the new `session_id` onto the rationale via an UPDATE (or via T104a's `record_classification_for_session` which writes the session-scoped `classification_produced` event and updates the rationale FK in one transaction); (5) record `session_opened`; (6) draft and send the first party-facing messages. Commits MUST preserve the invariant "no `mediation_sessions` row exists before a successful `take_dispute_issued`". Remove the current step-2-before-step-4 ordering and any code paths that commit a session row pre-take. **Spec-tension note**: the `PolicyDecision::Escalate(trigger)` branch (current `session.rs:392–415` returns `EscalatedOnOpen` after committing a session row) needs a separate decision — see T104b below, which is blocking on operator input before coding starts.
- [ ] T104a Split `src/mediation/policy.rs::initial_classification` into two functions so reasoning can run without a committed session row: `classify_for_start(ctx, dispute_id, initiator_role, prompt_bundle, reasoning, provider_name, model_name) -> Result<(PolicyDecision, RationaleId)>` — persists the rationale dispute-scoped (`session_id = NULL`), runs the policy-layer validation rules, returns both the decision and the rationale id to the caller. `record_classification_for_session(conn, session_id, decision, rationale_id, prompt_bundle)` — writes the session-scoped `classification_produced` event and updates `reasoning_rationales.session_id` from NULL to the newly-minted `session_id` in a single transaction so the audit store's FK stays consistent. Callers on the opening path invoke `classify_for_start` before take; on successful take + session insert, invoke `record_classification_for_session`. The restart-resume path (T052) does not need the split — it only reads rationales, never re-writes.
- [ ] T104b **[DECIDED 2026-04-20: option (b) — strict FR-122, dispute-scoped escalation]** `PolicyDecision::Escalate(trigger)` on the opening path means "not mediation-eligible"; Serbero MUST NOT issue `TakeDispute` and no `mediation_sessions` row is committed. Instead, the handoff is persisted dispute-scoped so Phase 4 still sees a structured record. Spec text in `spec.md` §"Mediation Start-Flow Ordering" step 3 already reads this way (no spec amendment needed). Concrete code changes land in T104c + T104d.
- [ ] T104c Rewrite `src/mediation/escalation.rs::recommend` signature to accept `session_id: Option<&str>` instead of `&str`. When `Some(session_id)`, behavior is unchanged: transition the session row to `escalation_recommended` inside the transaction, write session-scoped `escalation_recommended` + `handoff_prepared` events, write the handoff package rows with the session_id. When `None`, no session row exists: skip the session state-transition step; write `escalation_recommended` + `handoff_prepared` events as dispute-scoped rows (`session_id = NULL`, `dispute_id` in `payload_json`); write the handoff package dispute-scoped. Update the `RecommendParams` struct accordingly. Document at the top of the module that Phase 4 must support both shapes and that `session_id = NULL` rows represent "Serbero evaluated the dispute, reasoning verdict said escalate, no mediation session was opened". Update the two existing callers (`session.rs::handle_authorization_lost` — always `Some`, and the engine-tick mid-session escalation path — always `Some`) to pass `Some(session_id)` explicitly. The dispute-scoped `None` shape is consumed only by the new opening-path escalation branch added in T104d.
- [ ] T104d Update the `PolicyDecision::Escalate(trigger)` branch in `src/mediation/session.rs::open_session` (currently `session.rs:392–415`) so it fires **before** the session row is inserted. Flow: after the reasoning verdict at step (2) returns `Escalate(trigger)`, record `reasoning_verdict` dispute-scoped with the trigger captured, record `start_attempt_stopped{reason: "policy_escalate", trigger}` dispute-scoped, call `escalation::recommend(.., session_id: None, trigger, ..)` to write the dispute-scoped handoff, and return a new `OpenOutcome::EscalatedBeforeTake { dispute_id, trigger }` variant. Remove the old `OpenOutcome::EscalatedOnOpen { session_id, trigger }` variant entirely — no migration shim; the only consumer (engine tick) is updated in the same commit. The engine's dispatch on `OpenOutcome` learns to translate `EscalatedBeforeTake` into the same solver-facing `MediationEscalationRecommended` DM that `EscalatedOnOpen` produced today, but with `session_id = None` in the payload.
- [ ] T104e Rewrite the relevant US4 sub-tests in `tests/phase3_escalation_triggers.rs` to match the new dispute-scoped shape. Specifically: any sub-test whose scripted classification arrives on the opening call and drives an `Escalate` verdict must now assert (i) zero `mediation_sessions` rows for the dispute, (ii) a `reasoning_verdict` event dispute-scoped with the classification, (iii) a `start_attempt_stopped{reason: "policy_escalate"}` event, (iv) `escalation_recommended` + `handoff_prepared` events with `session_id IS NULL` and `payload_json` referencing the dispute_id, (v) the solver-facing `MediationEscalationRecommended` DM still fires via the existing router. Mid-session escalation sub-tests (party_unresponsive_timeout, round_limit, authorization_lost_mid_session) stay unchanged — those DO have a session row because they fire AFTER a successful session open.
- [ ] T105 Audit and remove any fallback that could issue `TakeDispute` without a live positive reasoning verdict for the same attempt: cached verdicts, scripted-take test hooks, manual mode flags. If a test fixture exercised a pre-reasoning take, rewrite it to go through the reasoning layer or delete it. Document in the `open_session` module header that pre-reasoning take is forbidden and cite FR-122.
- [ ] T106 [P] Integration test `tests/phase3_take_reasoning_coupling.rs` (SC-110) with sub-tests:
  - `reasoning_unavailable_skips_take` — `UnhealthyReasoningProvider`; detect a mediation-eligible dispute; assert no `TakeDispute` outbound event, no `mediation_sessions` row, `start_attempt_stopped{reason: "reasoning_unhealthy"}` recorded.
  - `reasoning_negative_verdict_skips_take` — provider scripted to return a non-eligible classification; assert no take, no session row, `start_attempt_stopped{reason: "reasoning_verdict_negative"}` recorded with the classification captured.
  - `take_fails_no_session_row` — provider returns positive verdict; `MostroChatSim` rejects the `AdminTakeDispute`; assert no session row, no outbound party message, `take_dispute_issued{outcome: "failure"}` recorded, `reasoning_verdict` still recorded.
  - `ordering_audit_trail` — happy path; SQL query asserts event ordering strictly: `start_attempt_started` < `reasoning_verdict` < `take_dispute_issued{outcome: "success"}` < `session_opened` < first `outbound_sent`, using `mediation_events.id` (autoincrement) as the monotonic clock.

### D — final solver report for externally resolved disputes (FR-124)

- [ ] T107 Implement `src/mediation/report.rs` with two public functions:
  - `has_any_mediation_context(conn, dispute_id) -> Result<bool>` — returns true if ANY of: a `mediation_sessions` row exists for `dispute_id` (any state, including `escalation_recommended` and terminal states), OR a `mediation_events` row exists whose `session_id` joins to this dispute, OR a dispute-scoped `mediation_events` row exists whose `payload_json` references this `dispute_id` (start-attempt events, reasoning-verdict events for takes that failed).
  - `emit_final_report(ctx, dispute_id, final_dispute_status) -> Result<()>` — build the FR-124 payload (dispute id, session id if any, classification + confidence or "no classification recorded" marker, outbound-party-messages counter 0/1/2+, final observed dispute status, short narrative derived from lifecycle transitions); resolve recipients via `router::resolve_recipients`; deliver via the Phase 1/2 notifier with `NotificationType::MediationResolutionReport`; record `resolved_externally_reported` event with the full payload summary.
  Idempotency is already provided by the top-level guard in `handlers/dispute_resolved.rs:95–98` (short-circuits when `lifecycle_state == LifecycleState::Resolved`), so subsequent replays of the resolved event never reach the report path. `emit_final_report` does NOT need its own `(dispute_id, final_status)` de-dup check; keeping the function idempotent w.r.t. accidental double-invocation within the same handler call is sufficient, and a comment in the module header should state that the outer handler owns replay-idempotency.
- [ ] T108 Refactor `src/handlers/dispute_resolved.rs`: replace the current "no active session → early return" branch (`handlers/dispute_resolved.rs:284–290` in the 2026-04-20 gap analysis) with a call to `report::has_any_mediation_context`. If true, call `report::emit_final_report`; if false, log at `debug!` and return (Phase 1/2-only dispute, out of FR-124 scope). For sessions in `escalation_recommended`, DO NOT use `latest_open_session_for` (which excludes them) — use a broader lookup that includes escalated sessions, and include the "escalation was recommended but dispute resolved before Phase 4 acted" note in the narrative. Keep the Phase 4 handoff package in place; do not delete or mutate it.
- [ ] T109 **Deprecate and replace** `src/mediation/mod.rs::notify_solvers_resolution_report` (currently at `mod.rs:1095`, signature `(conn, client, solvers, dispute_id, session_id: &str, resolution_status: &str)`). The `session_id: &str` positional argument is the blocker: the FR-124 "reasoning verdict but no session row" case (T111 sub-test `reasoning_verdict_no_session_resolved_externally`) requires `Option<&str>`. Replace with `report::build_report_body(payload: &FinalReportPayload) -> String` + `report::deliver_report(ctx, dispute_id, payload) -> Result<()>` where `FinalReportPayload` carries: `dispute_id`, `session_id: Option<String>`, `classification: Option<(ClassificationLabel, f64)>`, `outbound_party_messages_count: u8` (clamped 0..=2), `final_dispute_status: String`, `narrative: String`. The body MUST NOT contain the full rationale text (FR-120); include only the rationale reference id if relevant. Migrate the sole existing caller (`handlers/dispute_resolved.rs:298–306`) to the new API in the same commit; remove `notify_solvers_resolution_report` entirely (no compatibility shim) — the function is crate-private (`pub(crate)`), so no external consumer depends on it.
- [ ] T110 Reuse `NotificationType::MediationResolutionReport` from T093. If the payload shape changes materially, bump the payload version in the DM body (a leading `"mediation_resolution_report/v2"` line or equivalent) so downstream log parsers can tell the formats apart. Do NOT introduce a new `NotificationType` variant unless required by the notifier code shape.
- [ ] T111 [P] Integration test `tests/phase3_external_resolution_report.rs` (US6 update, SC-111) with sub-tests:
  - `full_session_resolved_externally` — session with 2 outbound + 2 inbound messages; external resolution observed; exactly one FR-124 DM; `outbound_party_messages_count = 2`; body lists the classification.
  - `outbound_only_session_resolved_externally` — session with outbound messages but no party replies; external resolution; FR-124 DM fires; counter reflects the number of distinct parties messaged.
  - `escalation_recommended_resolved_externally` — session in `escalation_recommended`; external resolution fires FR-124 DM; narrative notes the escalation-was-recommended fact; Phase 4 handoff package row remains present.
  - `reasoning_verdict_no_session_resolved_externally` — scripted: reasoning produced a positive verdict but `TakeDispute` failed, so no session row was committed (T104 invariant). External resolution fires FR-124 DM with `session_id = null`, `outbound_party_messages_count = 0`, classification captured from the dispute-scoped `reasoning_verdict` event.
  - `no_mediation_context_no_report` — a dispute Phase 1/2 detected but for which no reasoning verdict, session, or event exists; external resolution does NOT emit a FR-124 DM. Assert zero `MediationResolutionReport` notifications for this dispute.
  - `idempotency_no_double_send` — a second `DisputeStatus` observation for the same resolved terminal state does NOT produce a second DM; `resolved_externally_reported` event count stays at 1.

### Polish for the corrections

- [ ] T112 Update `specs/003-guided-mediation/quickstart.md` so the cooperative walkthrough notes the event-driven start (first outbound message appears within a few seconds of dispute detection, independent of the tick interval) and the external-resolution-report flow (add a short section demonstrating a dispute closed outside Serbero that still produces a solver DM).
- [ ] T113 [P] Audit `src/` for any remaining single-state eligibility checks or comments describing the polling-only start flow: grep for `lifecycle_state = 'notified'`, `lifecycle_state=\"notified\"`, and narrative comments referring to "engine tick" / "periodic sweep" as the primary session-opener. Remove or correct each hit. The only tick-centric comments allowed after this pass are the ones in `mediation/mod.rs::run_engine_tick` describing the tick's reduced retry/resumption role.
- [ ] T114 [P] Run `cargo clippy --all-targets --all-features -- -D warnings` and `cargo fmt --all -- --check` after Phase 10 edits; fix findings. Expect new warnings around the reshaped `open_session` return type and the new `StartOutcome` enum — address them directly rather than silencing with `#[allow]`.
- [ ] T115 Verify the three new Success Criteria empirically. SC-109: via T103 (engine tick disabled; new dispute still reaches first outbound message). SC-110: via T106 `ordering_audit_trail` sub-test (event-id ordering proves reasoning-before-take and no pre-take session row). SC-111: via T111 (all five positive sub-tests fire exactly one DM; the no-context sub-test fires none). If any SC fails, treat that as a blocker on Phase 10 completion — fix the underlying code before marking T115 done.

**Checkpoint**: Phase 10 complete — mediation start is event-driven and
strictly gated on reasoning; eligibility is a composed predicate; the
final solver-facing report fires for every externally resolved
dispute Serbero touched, including escalated sessions and take-failed
attempts. The engine tick is a safety net, not the trigger.

---

## Phase 11: Mid-Session Follow-Up Loop (FR-125 / FR-126 / FR-127 / FR-128 / FR-129 / FR-130 / FR-131)

**Goal**: close the mid-session loop left stranded in `main` after
Phase 3 US1–US6. After a party reply is ingested, Serbero MUST
re-classify the transcript and dispatch the next side effect (another
`AskClarification`, a cooperative `Summarize`, or an `Escalate`) —
currently the loop only persists the inbound and increments
`round_count`, then goes silent.

**Scope**: one new helper (`advance_session_round`), one hook in
`run_ingest_tick` post-ingest, one migration (v4) adding two columns
to `mediation_sessions`, one mid-session drafter variant, three new
integration tests. No prompt-bundle changes; no new states; no
transport or key-lifecycle changes.

**Exit criteria**: SC-112, SC-113, SC-114, SC-115 empirically pass.
Alice/Bob end-to-end walkthrough against a real relay completes the
second outbound within one ingest-tick cycle of Bob's reply.

### Setup / schema

- [X] T116 Add migration v4 in `src/db/migrations.rs` behind the
  existing `schema_version` guard. Two `ALTER TABLE mediation_sessions
  ADD COLUMN` statements: `round_count_last_evaluated INTEGER NOT NULL
  DEFAULT 0` and `consecutive_eval_failures INTEGER NOT NULL DEFAULT
  0`. Backfill is the column default (all existing rows land on `0`).
  Unit-test the migration: walk a v3 DB forward to v4, assert both
  columns exist with default `0` on every pre-existing row. Document
  in the migration header that backfilling `round_count_last_evaluated`
  to `0` forces any in-flight session to be re-evaluated once on the
  first post-migration ingest tick — acceptable because the
  alternative (skipping pre-existing sessions) keeps them silent.

### Helpers (core loop)

- [X] T117 Implement `src/mediation/transcript.rs` (new module):
  `load_transcript_for_session(conn, session_id, max_rows: usize) ->
  Result<Vec<TranscriptEntry>>`. Reads `mediation_messages` for the
  session, orders by `inner_event_created_at ASC`, tags each row
  with its party role by matching `shared_pubkey` against the
  session's `buyer_shared_pubkey` / `seller_shared_pubkey`, excludes
  rows with `stale = 1`, and caps at the most recent `max_rows`
  entries via SQL `LIMIT` (so an unbounded transcript cannot blow up
  the transfer). Rows with an inbound `shared_pubkey` that matches
  neither side are dropped and logged at `warn!`. Reuses the
  existing `crate::models::reasoning::TranscriptEntry` type. Seven
  unit tests cover: unknown session, session with no messages,
  interleaved outbound/inbound normal flow, stale exclusion,
  unknown-shared-pubkey drop, cap returning the last N in ascending
  order, and a zero-cap edge case. Note on the spec vs schema: the
  spec referred to `ingest_status IN ('fresh','duplicate')` but the
  real schema stores non-stale rows via UNIQUE-index dedup (no
  duplicate rows land) plus a `stale INTEGER 0/1` flag; the code
  filters on `stale = 0`, equivalent for the reasoning-input
  purpose.

- [X] T118 Add `db::mediation::advance_evaluator_marker(tx, session_id,
  new_round_count)` — writes `round_count_last_evaluated =
  new_round_count` AND `consecutive_eval_failures = 0` in a single
  UPDATE. Accepts `&rusqlite::Transaction` so the caller can commit
  it atomically with the dispatched side effect's row writes
  (FR-127). Added `db::mediation::bump_consecutive_eval_failures(conn,
  session_id) -> Result<i64>` that increments the counter and returns
  the new value; caller uses the return value to decide whether to
  escalate (FR-130, threshold = 3). Four unit tests cover: marker
  advance + failure reset; rollback of the enclosing TX leaves the
  marker untouched; increment returns 1→2→3 across successive bumps;
  two bumps followed by one successful advance resets the streak
  back to 0.

- [X] T119 Implement the mid-session drafter as `draft_and_send_followup_message`
  in `src/mediation/mod.rs` (not `src/chat/outbound.rs` as the spec
  originally said — the open-time sibling `draft_and_send_initial_message`
  already lives in `mediation/mod.rs`, so co-locating keeps the two
  drafters next to each other). Accepts `round_number: u32` and
  `round_count_to_mark: i64`; emits `"Round {N}. Buyer: ..."` /
  `"Round {N}. Seller: ..."` as the per-party body prefix (no
  prompt-bundle changes — the prefix is inline, `policy_hash` is
  preserved). The drafter does NOT quote or re-send earlier outbound
  text. Single transaction commits the two `mediation_messages` rows
  + the `classified → awaiting_response` state flip + the
  `advance_evaluator_marker` call (from T118); publish of the
  gift-wraps happens OUTSIDE the transaction per the Non-Goals
  carve-out in spec.md. Reuses the existing per-party `shared_pubkey`
  addressing; no changes to `session_key_cache`. End-to-end coverage
  lands with T122 (integration test that drives the full round-2
  flow against MockRelay).

- [X] T120 Implement `src/mediation/follow_up.rs::advance_session_round(
  conn, client, serbero_keys, reasoning, prompt_bundle, session_id,
  session_key_cache, solvers, provider_name, model_name) -> Result<()>`.
  Landed in a new `src/mediation/follow_up.rs` module (not in
  `mod.rs` as the spec originally said — keeps the orchestrator
  apart from the drafters and gives it room to grow). **Design
  divergence from FR-129**: the session stays in `awaiting_response`
  throughout the loop; no mid-session transition to `classified` or
  `follow_up_pending` is written. The state machine rejects the
  direct `classified → awaiting_response` edge, and composing
  `classified → follow_up_pending → awaiting_response` inside one TX
  is ceremonial for outside observers because no tick ever sees the
  intermediate state. The single authoritative gate against
  re-dispatch is `round_count_last_evaluated` (FR-127); the drafter
  refreshes `last_transition_at` without touching `state`. Flow:
  1. Load session row; short-circuit if state NOT in
     {`awaiting_response`}. (The other states — `classified`,
     `summary_pending`, `summary_delivered`, `escalation_recommended`,
     `closed`, `superseded_by_human` — are either in-flight with
     another handler or terminal.)
  2. Gate on `round_count > round_count_last_evaluated`. If equal,
     return without side effects (FR-127 idempotency).
  3. Load transcript via T117.
  4. Call `reasoning.classify(ClassificationRequest { .. })`. On any
     error, call T118's failure bump; if the returned count is `>= 3`,
     delegate to `escalation::recommend(.., ReasoningUnavailable)`
     (the existing helper resets the counter implicitly because
     a subsequent successful evaluation would call
     `advance_evaluator_marker` which zeroes it; the escalation path
     transitions the session out of `awaiting_response` so future
     ingest ticks do not re-evaluate). Return without further action.
  5. Call `policy::evaluate` — the first production call site of that
     function (`src/mediation/policy.rs:221`).
  6. Open a transaction; transition `awaiting_response → classified`
     and write the `classification_produced` event inside it.
  7. Dispatch on the `PolicyDecision`:
     - `AskClarification(text)` → T119 drafter. The drafter MUST
       persist the two `mediation_messages` rows + the `classified →
       awaiting_response` state transition + `advance_evaluator_marker`
       in one DB transaction, then publish gift-wraps OUTSIDE the
       transaction (mirrors the open-time `draft_and_send_initial_message`
       pattern). A publish failure returns `Err` with rows committed
       and NO automatic retry — see spec §"Non-Goals (Phase 11)".
       On `Err`, call `bump_consecutive_eval_failures`; do NOT try
       to roll back the committed rows.
     - `Summarize { .. }` → call the existing `deliver_summary`
       (the function already handles the `summary_pending →
       summary_delivered → closed` progression); call
       `advance_evaluator_marker` in its own post-commit DB op since
       `deliver_summary` owns its own transaction scope.
     - `Escalate(trigger)` → call `escalation::recommend(..)` with the
       `Some(session_id)` form (which already transitions the session
       row); `consecutive_eval_failures` is reset by the escalation
       path.
  8. Any error past step 5 calls `bump_consecutive_eval_failures`
     and returns without committing.

- [X] T121 Hook `advance_session_round` into
  `src/mediation/mod.rs::run_ingest_tick`. After the `'envelope_loop`
  finishes for each session (not inside the `Fresh` arm, so both
  parties' replies are batched into one reasoning call rather than
  producing two per cycle), check a local `session_had_fresh` flag
  and call `advance_session_round` when it is `true`. Sequential per
  session by construction (the hook is in the `while let Some(res) =
  fetchers.join_next()` drain), satisfying FR-131's concurrency
  guard. Errors surface via `advance_session_round`'s own internal
  handling; this wrapper catches any `Err` return with `warn!` and
  continues the tick for other sessions. The `run_ingest_tick`
  signature grew to include `serbero_keys`, `reasoning`,
  `provider_name`, `model_name` — the caller in `run_engine` (one
  site) was updated.

### Integration tests

- [X] T122 [P] `tests/phase3_followup_round.rs` (SC-112 + SC-113):
  seeds a session at `round_count = 1` with two round-0 outbound
  rows and two inbound reply rows (the state a real US1+US2 run
  would have left on disk), populates `SessionKeyCache` with
  `DisputeChatMaterial`, and calls `follow_up::advance_session_round`
  directly. Asserts (SC-112) 4 outbound rows total, each new row
  carrying the `"Round 1. "` prefix and the scripted clarification
  text, state back at `awaiting_response`, and `round_count_last_evaluated
  = 1`. Asserts (SC-113) a second `advance_session_round` call with
  no new inbound produces zero new outbound rows and exactly one
  `classification_produced` audit row (idempotency gate holds).
  Implementation note: the test calls the orchestrator directly
  rather than driving `run_ingest_tick` so the harness stays
  focused — the ingest-tick hook path is exercised structurally by
  T121's single call site.

- [ ] T123 [P] `tests/phase3_followup_summary.rs` (SC-114): same
  harness but the scripted provider returns `Summarize {
  CoordinationFailureResolvable, 0.9 }` on the mid-session call.
  Assert `deliver_summary` fires exactly once; session ends `closed`
  with a `summary_delivered` event on file; the solver receives the
  summary DM.

- [ ] T124 [P] `tests/phase3_followup_reasoning_failure.rs` (SC-115):
  script the provider to fail (`ReasoningError::Unreachable`) three
  times in a row on the mid-session path. Drive three ingest ticks
  with a fresh inbound each time (or with the marker artificially
  reset). Assert on the third failure the session transitions to
  `escalation_recommended` with trigger `ReasoningUnavailable`, and a
  session-scoped `escalation_recommended` + `handoff_prepared` pair
  exists in `mediation_events`.

### Polish

- [ ] T125 Run `cargo clippy --all-targets --all-features -- -D
  warnings` and `cargo fmt --all -- --check` after Phase 11 edits;
  fix findings. Expected new warnings cluster around the drafter
  variant and the `advance_session_round` dispatch — address directly
  rather than silencing.

- [ ] T126 Update `specs/003-guided-mediation/quickstart.md` with a
  short walkthrough of the cooperative follow-up round: after the
  first outbound both parties reply, Serbero re-classifies and emits
  the second outbound without operator intervention. Point readers to
  the three new integration tests as the reference.

**Checkpoint**: Phase 11 complete — mid-session mediation rounds
progress on their own; `policy::evaluate` has a production call site;
Alice/Bob end-to-end walkthrough against a real relay completes the
second outbound within one ingest-tick cycle of Bob's reply.

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)**: No dependencies — can start immediately.
- **Foundational (Phase 2)**: Depends on Setup completion — BLOCKS all user stories.
- **User Story 1 (Phase 3)**: Depends on Foundational — standalone Phase 3 MVP.
- **User Story 2 (Phase 4)**: Depends on US1 (session open, outbound path, prompt pinning, DB helpers).
- **User Story 3 (Phase 5)**: Depends on US1 + US2 (needs a session with inbound ingest before summarizing).
- **User Story 4 (Phase 6)**: Depends on US1 at minimum; several triggers need US2 (round counter, timeout) and the reasoning adapter (T066). Can be developed in parallel with US3 once US1+US2 land.
- **User Story 5 (Phase 7)**: Depends on US1 for end-to-end testability; can be developed in parallel with US3/US4 once the OpenAI adapter (T015) exists.
- **Polish (Phase 8)**: Depends on all shipped user stories being complete.
- **User Story 6 (Phase 9)**: Depends on US1 (needs a session-open code path to close against). Currently marked complete on `main`.
- **Correction Phase 10**: Depends on US1–US6 complete. T097 lands first (event kinds; T098 was merged into T097 because outcomes are encoded as event-kind + session-state, not a standalone enum). T099–T103 (A+C) and T104–T106 (B) can land in parallel after T097 — they touch disjoint files except for `session.rs` where T104's ordering change must precede T101's `try_start_for` delegation. T107–T111 (D) depend on T097 for the new event kind but are otherwise independent of A/B/C. T112–T115 depend on all preceding Phase 10 tasks.
- **Correction Phase 11**: Depends on Phase 10 landed (shares `src/mediation/mod.rs` and the audit-trail invariants). T116 (migration v4) is the only blocker for every other task. T117–T119 are `[P]` after T116 — three disjoint files (`transcript.rs` new, `db/mediation.rs`, `chat/outbound.rs`). T120 (the `advance_session_round` helper) depends on T117/T118/T119. T121 wires T120 into the ingest tick. T122–T124 are `[P]` integration tests after T121. T125 / T126 are polish and depend on all preceding Phase 11 tasks.

### User Story Dependencies

- **US1 (P1)**: Depends on Foundational. Stand-alone after that.
- **US2 (P1)**: Extends US1 (persistence, routing, state machine reused).
- **US3 (P2)**: Extends US2 (needs real transcript to summarize).
- **US4 (P2)**: Extends US1+US2; several triggers need reasoning outputs.
- **US5 (P3)**: Extends US1+Foundational; cross-cuts all the others only in that it changes the reasoning endpoint.

### Within Each User Story

- Integration tests SHOULD be written first and initially fail.
- Models and DB helpers before handlers.
- Handlers before engine wiring.
- Engine wiring before structured tracing cleanup.

### Parallel Opportunities

- **Setup**: T002, T003, T004 in parallel.
- **Foundational**: T006, T007, T008 (model files), T011 (hash) in parallel; T013, T014 (reasoning trait + NYI stub) in parallel; all unit tests T020–T023 in parallel.
- **US1**: tests T024–T030 all `[P]`; implementation tasks T031–T036 can run in parallel per file; T037 onward depend on them.
- **US2**: tests T045–T048 all `[P]`; `inbound.rs` (T049) and `session.rs::ingest_inbound` (T050) can land in parallel.
- **US3**: tests T055–T057 all `[P]`; `summarizer.rs` and `router.rs` are disjoint files.
- **US4**: tests T063–T065 all `[P]`; each trigger implementation (T068–T071) is a different call site and can be parallelised.
- **US5**: all tasks are `[P]` — the adapter change is a handful of lines, the NYI stub change is independent, the test files are new.
- **Polish**: T081–T087 all `[P]`.
- **Phase 10 corrections**: T099 (eligibility module) and T103 / T106 / T111 (integration tests) are `[P]`. T104 (session.rs ordering) and T101 (start.rs) serialize on `src/mediation/session.rs`. T107 / T108 / T109 serialize on the resolved-dispute handler + report module. T113 / T114 are `[P]` audits.

---

## Parallel Example: User Story 1

```bash
# Launch all US1 integration tests in parallel (different files, no ordering):
Task: "Integration test tests/phase3_session_open.rs"
Task: "Integration test tests/phase3_session_open_gating.rs"

# Launch inline unit tests in parallel:
Task: "Inline unit tests in src/chat/shared_key.rs"
Task: "Inline unit tests in src/chat/outbound.rs"
Task: "Inline unit tests in src/db/mediation.rs"
```

---

## Implementation Strategy

### MVP First (User Stories 1 and 2)

1. Complete Phase 1 Setup (T001–T004).
2. Complete Phase 2 Foundational (T005–T023) — CRITICAL, blocks all stories.
3. Complete Phase 3 User Story 1 (T024–T044).
4. Complete Phase 4 User Story 2 (T045–T054).
5. **STOP and VALIDATE**: Serbero can now open a mediation session, exchange clarifying messages over the Mostro chat transport, persist session state durably across restart, and dedup replays. Run `quickstart.md` §Verify mediation end-to-end (US1 + US2 parts). Ship Phases 1–2 of Phase 3 as the MVP.

### Incremental Delivery

1. Setup + Foundational → foundation ready.
2. Add US1 → cooperative mediation starts reliably.
3. Add US2 → multi-round conversations are durable.
4. Add US3 → cooperative resolutions reach human solvers as clean artifacts.
5. Add US4 → out-of-scope disputes exit Phase 3 cleanly with a Phase 4 handoff.
6. Add US5 → operators can swap endpoints without code changes.
7. Add US6 → externally resolved disputes close without leaving state dangling.
8. Run Polish phase.
9. Run Phase 10 corrections (FR-121/122/123/124) to move start to event-driven, couple take to reasoning, broaden eligibility, and close the FR-124 reporting gap before tagging the Phase 3 release.

### Out of Scope for This Tasks File

- Phase 4 escalation execution (routing to write-permission solvers, acknowledgement tracking, re-escalation under load). This tasks file *prepares* the handoff; Phase 4 *consumes* it.
- Phase 5 reasoning-backend portability beyond the `openai` / `openai-compatible` endpoint (Anthropic, PPQ.ai, OpenClaw adapters).
- Multi-instance Serbero coordination.
- Replacing the Phase 1/2 notifier. Solver-facing DMs continue to use it unchanged.

---

## Notes

- `[P]` tasks = different files, no dependencies on incomplete tasks.
- `[Story]` label maps tasks to their user story for traceability.
- The Mostro chat transport uses the **dispute-chat interaction flow used by current Mostro clients**, verified against the Mostrix reference (`chat_utils.rs`, `execute_take_dispute.rs`). This is implementation reference material, not a protocol-level definition; whenever Mostro clients change behavior, `src/chat/*` must be updated to match. `tests/common/mod.rs::MostroChatSim` models the subset Phase 3 exercises.
- `admin-settle` / `admin-cancel` / fund-movement tokens MUST remain absent from `src/` after every task (SC-102, FR-115). This is the single most important invariant across the whole Phase 3 implementation.
- Full rationales go to `reasoning_rationales`; general logs carry only classification + confidence + `rationale_id` (FR-120).
- `nostr-sdk 0.44.1` API shapes confirmed in Phases 1/2 research remain authoritative for Phase 3 (no version bump). Verification points for the Mostro dispute-chat flow are in `research.md` R-101 and must be re-confirmed during implementation.
- Phase 3 does NOT introduce a generic retry framework. The OpenAI adapter and auth-retry loop use plain bounded loops. Resist scope growth here (plan scope-control note).
