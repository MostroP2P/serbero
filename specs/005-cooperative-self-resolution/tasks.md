---

description: "Task list for Cooperative Self-Resolution Nudge (Feature 005)"
---

# Tasks: Cooperative Self-Resolution Nudge

**Input**: Design documents from `/specs/005-cooperative-self-resolution/`
**Prerequisites**: plan.md (✅), spec.md (✅), research.md (✅), data-model.md (✅), contracts/ (✅), quickstart.md (✅)

**Tests**: This codebase has a strong integration-testing tradition (`tests/phase3_*.rs`); the plan lists six new test binaries explicitly, so test tasks are part of each user-story phase. They are NOT TDD-first gates — Rust's `cargo test` runs them after implementation lands.

**Organization**: Tasks grouped by user story (US1, US2, US3) per spec.md priorities. US1 + US2 are P1 and constitute the shippable MVP; US3 is P2 (regression-only, since its implementation is fully covered by US1+US2 work).

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies on incomplete tasks)
- **[Story]**: User story label (US1 / US2 / US3) — only on user-story phase tasks
- All paths are absolute under the repository root `/home/negrunch/dev/cancerbero/`

---

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: Module wiring + loader-discovery before any code lands.

- [ ] T001 Inspect `src/prompts/` (or wherever `PromptBundle` is loaded today — likely `src/prompts/mod.rs` and `src/prompts/bundle.rs`) and confirm the loader can be extended to parse one extra Markdown file alongside the existing bundle files (`phase3-system.md`, `phase3-classification.md`, etc.). Document the exact location of the loader call site for T010.
- [ ] T002 [P] Add `pub mod self_resolution;` to `src/mediation/mod.rs` (after the `pub mod ...;` lines for the existing siblings). Empty module file is added in T009; this task only registers the future module so the rest of the codebase compiles after T009 lands without further wiring.

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: Data-model and prompt-bundle scaffolding that ALL three user stories depend on.

**⚠️ CRITICAL**: No user-story work can begin until this phase is complete.

### Enum + struct extensions (parallelizable — all touch different files)

- [ ] T003 [P] Add `SelfResolutionOffered` variant to `MediationEventKind` in `src/db/mediation_events.rs`. The `Display` / `FromStr` impls already follow `snake_case`; serialised string is `"self_resolution_offered"`. Add the variant to any exhaustive match in the same file at the existing convention.
- [ ] T004 [P] Add `PartyRequestedHuman` variant to `EscalationTrigger` in `src/models/escalation.rs`. Serialised string `"party_requested_human"`. Update the `Display` / `FromStr` pair and any exhaustive match in the same file.
- [ ] T005 [P] Add `SuggestSelfResolutionWithSummary { confidence: f32 }` variant to `PolicyDecision` in `src/mediation/policy.rs`. The variant is a sibling of the existing `Summarize { classification, confidence }`. Update any exhaustive `match` over `PolicyDecision` in the same file (the dispatch happens in `follow_up.rs` and is wired in T018, but the compile errors here surface every site that needs updating).
- [ ] T006 [P] Extend `ClassificationResponse` in `src/models/reasoning.rs` with three additive fields, all `#[serde(default)]`:
  - `human_requested: bool` (defaults to `false` so out-of-date providers and round 0/1 responses parse cleanly).
  - `buyer_language: Option<String>` (ISO-639-1 code, e.g. `"en"`, `"es"`, `"pt"`; `None` when the classifier cannot determine the language confidently).
  - `seller_language: Option<String>` (same shape as `buyer_language`).

  The two language fields are needed because there is **no Rust-side language-detection helper in this codebase** — language inference today happens implicitly inside the classifier LLM call when it emits `buyer_clarification` / `seller_clarification` in the inferred language. The new branch needs structured language codes so the dispatch arm in T018 can pick the right `[xx]` section from the static template bundle. `None` falls back to `bundle.self_resolution.fallback_language` (typically `"en"`) per the contract in `template-bundle.md`.
- [ ] T007 [P] Add `self_resolution_threshold: f32` (default 0.75) and `self_resolution_enabled: bool` (default true) to `MediationConfig` in `src/models/config.rs`. Use `#[serde(default = "...")]` helpers per the existing convention. Extend the existing validation function for `MediationConfig` to assert `0.0 ..= 1.0` for the threshold (matches the existing pattern for similar f32 fields). Refusing to start on out-of-range values is the documented behaviour per `contracts/config.md`.
- [ ] T008 [P] Add the legal transition `(SummaryDelivered, EscalationRecommended)` to `MediationSessionState::can_transition_to` in `src/models/mediation.rs`. Update the existing `can_transition_to_*` test cases (around line 260) so the new edge is covered (legal) and the inverse `(EscalationRecommended, SummaryDelivered)` is asserted illegal.

### Self-resolution module + prompt bundle wiring

- [ ] T009 Create `src/mediation/self_resolution.rs` with: (a) `SelfResolutionTemplates { by_language: HashMap<String, SelfResolutionLanguageEntry>, fallback_language: String }`, (b) `SelfResolutionLanguageEntry { template: String, human_assistance_optin: String }`, (c) a pure function `render_for(language_code: Option<&str>, templates: &SelfResolutionTemplates) -> String` that returns `format!("{} {}", template, human_assistance_optin)` for the matched language (or `fallback_language` if `language_code` is `None` or absent from `by_language`). All pure functions, no I/O — keeps the keyword-audit unit test fast.
- [ ] T010 Extend `PromptBundle` in the file identified by T001 with `pub self_resolution: SelfResolutionTemplates` and update the loader to parse `prompts/phase3-self-resolution.md`. The bundle's existing SHA-256 hash automatically extends over the new file because the loader hashes the whole bundle directory. Add the parser implementation in `src/prompts/` per the loader's existing pattern (look at how `phase3-message-templates.md` is parsed for reference). The parser MUST tolerate empty / commented-only sections gracefully and MUST require at minimum one entry whose key matches the file's `fallback_language` setting.
- [ ] T011 Create `prompts/phase3-self-resolution.md` with the operator-note Markdown comment from `contracts/template-bundle.md` followed by `[en]`, `[es]`, and `[pt]` sections. Each section MUST contain `template = "..."` and `human_assistance_optin = "..."` per the contract. Use the seed text in `contracts/template-bundle.md` for the initial values (or refer to the original draft in `/tmp/serbero-005-draft/spec-draft/spec.md` Appendix A). The default `fallback_language` is `"en"`.
- [ ] T012 [P] Amend `prompts/phase3-system.md`'s "Output Rules" section: add `self_resolution_offered` to the Allowed list with the constraints "templated per-party invitation, in the party's detected language, with an explicit human-escalation opt-in; MUST NOT name a fund-moving action". Re-state the unchanged fund-action prohibition adjacent to the new entry.
- [ ] T013 [P] Amend `prompts/phase3-escalation-policy.md` to document `party_requested_human` as a new escalation trigger. Use one paragraph similar in tone to the existing trigger documentation (`reasoning_unavailable`, `low_confidence`, etc.).
- [ ] T014 [P] Amend `prompts/phase3-classification.md` (or whichever prompt file holds the classifier JSON-output schema) to document three additive fields:
  - `human_requested: bool` (optional; only requested by the runtime on rounds following a `self_resolution_offered` event for the session — covered by T021/T022 prompt assembly).
  - `buyer_language: string | null` (ISO-639-1 code, e.g. `"en"`, `"es"`, `"pt"`; emitted on **every** round, not gated on prior events). Instruct the model to set the field to its best-guess code from the buyer's most recent reply, or `null` when the most recent reply has no buyer content or is too short to disambiguate.
  - `seller_language: string | null` (same shape and instructions, for the seller).

  Add an example block showing the full classifier-output JSON with the three new fields populated. The two language fields are unconditionally part of the schema so the runtime always has structured language codes to drive `render_for(...)` (see T018) without needing a Rust-side language-detection helper.

**Checkpoint**: Foundation ready. The codebase still compiles, `cargo test` still passes (the new variants and fields are unused so far, but every existing exhaustive match has been updated). Ready to begin user-story implementation.

---

## Phase 3: User Story 1 (Priority: P1) 🎯 MVP — Cooperative case resolves without human solver action

**Goal**: When the model classifies a session as `coordination_failure_resolvable` with confidence ≥ the configured threshold, dispatch one templated, language-matched invitation per party in the existing chat transport AND deliver the existing solver-facing summary with `suggested_next_step = "self_resolution_offered_to_parties"`.

**Independent Test**: Per spec User Story 1 + the quickstart's "User Story 1 — Cooperative resolves without solver action" walkthrough. A controlled session with a scripted reasoning provider returning the cooperative label at confidence 0.85 must produce: two outbound `mediation_messages` rows (one per party, audience-tagged), one `self_resolution_offered` audit row, one `summary_generated` audit row, one `mediation_summary` notification to the assigned solver, and the session in `summary_delivered`.

### Tests for User Story 1

- [ ] T015 [P] [US1] Create `tests/phase3_self_resolution_template_audit.rs` — pure-Rust unit test (no DB, no relay, no `MockReasoningProvider`). Loads the prompt bundle from the `tests/fixtures/prompts/` directory (the fixture bundle the rest of the test suite uses), walks every `(language_code, SelfResolutionLanguageEntry)` cell, builds the rendered string `format!("{} {}", template, human_assistance_optin)`, and asserts the rendered string contains NONE of the banned substrings from the matrix in `contracts/template-bundle.md` (case-insensitive, ASCII-folded). The matrix is a `&[(&str, &[&str])]` constant in the test file. Backs FR-004 / SC-003.

### Implementation for User Story 1

- [ ] T016 [US1] Add the new branch to `policy::evaluate(...)` in `src/mediation/policy.rs`. Pre-condition predicate (top of the cooperative branch): `classification.label == ClassificationLabel::CoordinationFailureResolvable && classification.suggested_action == SuggestedAction::Summarize && confidence >= cfg.mediation.self_resolution_threshold && cfg.mediation.self_resolution_enabled && !session_has_self_resolution_offered(conn, session_id)`. When all true, return `PolicyDecision::SuggestSelfResolutionWithSummary { confidence }`. When any false, fall through to the existing `Summarize { classification, confidence }` branch (legacy behaviour preserved per FR-012 / SC-007).
- [ ] T017 [US1] Add the helper `fn session_has_self_resolution_offered(conn: &Connection, session_id: &str) -> Result<bool>` in `src/mediation/policy.rs` (or `src/db/mediation_events.rs` if it fits better there — wherever the existing `count_classification_events` lives is a good neighbour). Implementation: `SELECT 1 FROM mediation_events WHERE session_id = ?1 AND kind = 'self_resolution_offered' LIMIT 1`, return `Ok(row.is_some())`. Used by T016 and T023.
- [ ] T018 [US1] Add the dispatch arm for `PolicyDecision::SuggestSelfResolutionWithSummary` in `src/mediation/follow_up.rs`. The arm must, in order: (1) read each party's language from the structured classifier response — `classification.buyer_language.as_deref()` and `classification.seller_language.as_deref()` (added in T006). The runtime does NOT do its own language detection; the LLM is the single source of truth for language inference (consistent with how `buyer_clarification` / `seller_clarification` already inherit language implicitly today). (2) Call `mediation::self_resolution::render_for(language_code, &bundle.self_resolution)` per party — the helper itself falls back to `bundle.self_resolution.fallback_language` (typically `"en"`) when the passed code is `None` or absent from `by_language`, per the contract in `template-bundle.md`. (3) Open a SQL transaction and within it: write the `SelfResolutionOffered` audit event row (payload per `data-model.md` and `contracts/audit-events.md`, including the resolved per-party language codes) AND insert two `mediation_messages` rows (one per party, audience-tagged "buyer" / "seller", populated by `chat::outbound::build_wrap_with_audience`). (4) Commit. (5) Publish each gift-wrap outside the transaction (matching the commit-then-publish pattern of the existing initial drafter). (6) Call the existing `deliver_summary(...)` helper with `suggested_next_step = "self_resolution_offered_to_parties"`. The arm walks the session through `Classified → SummaryPending → SummaryDelivered` exactly as the legacy Summarize arm does, just with the extra outbound + audit row before `deliver_summary`.
- [ ] T019 [US1] Create `tests/phase3_self_resolution_happy_path.rs` integration test (US1 acceptance scenarios 1–3). Seed a session in `awaiting_response` with one buyer reply (Spanish) + one seller reply (Spanish: "ya recibí el fiat"). Wire a `MockReasoningProvider` that returns `CoordinationFailureResolvable` confidence 0.85 with `suggested_action = Summarize`. Run one engine cycle. Assert: (a) two outbound `mediation_messages` rows with `party = 'buyer'` / `'seller'` and content equal to the byte-exact `[es]` rendered template; (b) exactly one `self_resolution_offered` audit row with payload referencing the rationale id; (c) exactly one `summary_generated` audit row with `suggested_next_step = "self_resolution_offered_to_parties"`; (d) one `mediation_summary` notification published to the assigned solver; (e) session in `summary_delivered`.
- [ ] T020 [US1] Create `tests/phase3_self_resolution_one_shot.rs` integration test (FR-006 / SC-006). Continue from the happy-path scenario; ingest a fresh round of replies that produce another `CoordinationFailureResolvable` confidence 0.85. Assert: (a) the second round does NOT produce a second `self_resolution_offered` audit row, (b) the second round does NOT produce a second outbound pair of party invitations, (c) the second round still produces a normal `summary_generated` row (legacy Summarize falls through, since the one-shot guard fails the new branch's pre-condition).

**Checkpoint**: User Story 1 fully functional. The cooperative invitation path ships end-to-end. With `self_resolution_enabled = false`, the legacy path is byte-for-byte unchanged.

---

## Phase 4: User Story 2 (Priority: P1) — Party opts in to human assistance

**Goal**: When a party reply on a round following a `self_resolution_offered` event explicitly requests human assistance, escalate the session to the assigned solver under the new `party_requested_human` trigger.

**Independent Test**: Per spec User Story 2 acceptance scenarios. After a session has received the cooperative invitation, an inbound buyer reply containing "necesito un humano que revise esto" results in `escalation_recommended` row with `trigger = party_requested_human` and a Phase 4 `handoff_prepared` row.

### Implementation for User Story 2

- [ ] T021 [US2] Update OpenAI classifier prompt assembly in `src/reasoning/openai.rs` with two changes:
  1. **Unconditional**: include in every classifier prompt the instruction to emit `buyer_language` and `seller_language` (ISO-639-1 codes; null when undeterminable), per the schema documented in T014. This change is NOT gated on prior `self_resolution_offered` — every round needs the language codes so the runtime always has structured per-party language available, even outside the cooperative branch (cheap and keeps round 0/1 ready in case the cooperative branch fires later).
  2. **Conditional**: on rounds where the session has a prior `self_resolution_offered` audit row (call `session_has_self_resolution_offered` from T017, threading through the request-context plumbing the existing classifier prompt assembly already uses), append the additional instruction block from `contracts/classifier-output.md` that documents the `human_requested: bool` field with example phrases.

  The provider parser already accepts all three fields via T006; this task only adds the prompt-side requests.
- [ ] T022 [US2] Update Anthropic classifier prompt assembly in `src/reasoning/anthropic.rs` with the same two changes as T021: (1) unconditional emission of `buyer_language` / `seller_language` on every round; (2) conditional `human_requested` instruction block on rounds following a `self_resolution_offered` event. Both adapters MUST land in the same PR set so a deploy that runs the Anthropic adapter in production cannot accidentally lose the opt-in path or the language-code emission.
- [ ] T023 [US2] Add the policy short-circuit at the top of `policy::evaluate(...)` in `src/mediation/policy.rs`, BEFORE the existing classification-label dispatch: `if classification.human_requested && session_has_self_resolution_offered(conn, session_id) { return PolicyDecision::Escalate(EscalationTrigger::PartyRequestedHuman); }`. The predicate guard prevents abuse from earlier rounds and from buggy providers that emit the field where the prompt did not request it.
- [ ] T024 [US2] Verify `escalation::recommend(...)` in `src/mediation/escalation.rs` accepts the new `EscalationTrigger::PartyRequestedHuman` variant end-to-end. If the helper has any exhaustive `match` over `EscalationTrigger`, add the arm with the appropriate string serialisation; otherwise no change is needed (the existing `Display` impl on the enum carries the new variant automatically).
- [ ] T025 [US2] Create `tests/phase3_self_resolution_opt_in.rs` integration test (US2 acceptance scenarios 1–2). Continue from the happy-path scenario; ingest a fresh buyer reply with explicit human-assistance text; configure the `MockReasoningProvider` to return `human_requested = true` on this round (any classification label). Assert: (a) one new `escalation_recommended` audit row with `trigger = party_requested_human`; (b) one new `handoff_prepared` audit row (Phase 4 takes over from there); (c) session is in `escalation_recommended`; (d) NO second `self_resolution_offered` row is written. Then ingest a separate session where the round-N+1 reply is unrelated ("ok, esperamos") and `human_requested = false`; assert NO escalation fires.

**Checkpoint**: User Stories 1 + 2 functional. Feature is shippable as MVP.

---

## Phase 5: User Story 3 (Priority: P2) — No lock-in on cooperative branch

**Goal**: A non-cooperative classification on a round following the cooperative invitation MUST escalate under its own standard trigger, NOT under `party_requested_human` or any cooperative-branch hold.

**Independent Test**: Per spec User Story 3 acceptance scenario 1. After invitation, a round-N+1 classification of `ConflictingClaims` confidence 0.92 produces `Escalate(ConflictingClaims)`, not `Escalate(PartyRequestedHuman)`.

US3 has no implementation work beyond what T016 and T023 already deliver: T016's pre-condition predicate ensures the new branch only fires for `CoordinationFailureResolvable + Summarize`, so a `ConflictingClaims` classification falls through to the existing `Escalate(ConflictingClaims)` path; T023's predicate guard requires both `human_requested == true` AND a prior `self_resolution_offered` row, so an opt-in trigger does not accidentally hijack a legitimate non-cooperative escalation. What remains is the explicit regression test.

### Implementation for User Story 3

- [ ] T026 [US3] Create `tests/phase3_self_resolution_no_lock_in.rs` integration test (US3 acceptance scenario 1). Continue from the happy-path scenario; ingest a round-N+1 reply ("the seller never released, they lied"). Wire the `MockReasoningProvider` to return `ConflictingClaims` confidence 0.92 with `human_requested = false`. Assert: (a) one new `escalation_recommended` audit row with `trigger = conflicting_claims`; (b) NOT `party_requested_human`; (c) session in `escalation_recommended`. The cooperative branch must not bias subsequent rounds.

**Checkpoint**: All three user stories functional and independently testable.

---

## Phase 6: Polish & Cross-Cutting Concerns

**Purpose**: Hardening, regression coverage, operator-facing documentation, and pre-merge sanity.

- [ ] T027 [P] Create `tests/phase3_self_resolution_kill_switch.rs` integration test for SC-007 + the sub-threshold regression. Three side-by-side sessions (different dispute ids) with identical inbound round-1 transcripts and identical scripted classifier responses. Run the engine on:
  - **Session A**: `self_resolution_enabled = true`, `self_resolution_threshold = 0.75`, classifier returns `CoordinationFailureResolvable` confidence `0.85`.
  - **Session B**: `self_resolution_enabled = false`, `self_resolution_threshold = 0.75`, classifier returns the same `CoordinationFailureResolvable` confidence `0.85` (kill-switch case).
  - **Session C**: `self_resolution_enabled = true`, `self_resolution_threshold = 0.75`, classifier returns `CoordinationFailureResolvable` confidence `0.50` (sub-threshold case — backs FR-010).

  Assertions:
  - Session A: contains `self_resolution_offered` row; `summary_generated` payload `suggested_next_step = "self_resolution_offered_to_parties"`.
  - Session B: NO `self_resolution_offered` row; `summary_generated` payload `suggested_next_step` carries the legacy value (whatever the existing cooperative-summary path uses), NOT `"self_resolution_offered_to_parties"`.
  - Session C: NO `self_resolution_offered` row (sub-threshold takes the legacy path); `summary_generated` payload identical to session B's. Backs the byte-for-byte regression guarantee in both kill-switch and sub-threshold dimensions.
- [ ] T028 [P] Add a startup-time health-check probe for the `human_requested` field on the configured reasoning provider per R-003 in `research.md`. Implementation: extend the existing reasoning health-check (already invoked from `daemon.rs` startup) to include a one-shot classification call whose mock transcript is crafted to elicit `human_requested = true` on the response, then assert the parsed response carries the field with that value. If the field is absent or `false`, log a warning at `info!` level (`reasoning health-check: provider does not echo human_requested; cooperative-self-resolution opt-in path will silently fail for this provider until updated`). Do not fail the daemon — operators may intentionally run with kill-switch off until the provider catches up.
- [ ] T029 [P] Add operational structured-tracing events needed for SC-001 and SC-002 baseline measurement (R-002 in research.md). Implementation: emit two pinned `info!` events. Pinned event names (downstream tooling joins on these):
  - **`cooperative_case_detected`** — emitted from the `self_resolution_offered` event-emit code path in T018, the moment Serbero decides to dispatch the cooperative invitation. Structured fields: `session_id` (string), `dispute_id` (string), `confidence` (f32), `prompt_bundle_id` (string), `buyer_language` (string-or-null), `seller_language` (string-or-null), `occurred_at_unix` (i64).
  - **`cooperative_case_closed_externally`** — emitted from the `dispute_resolved` handler when the session being closed had a prior `self_resolution_offered` audit row (cheap existence check, same predicate shape as T017). Structured fields: `session_id` (string), `dispute_id` (string), `elapsed_secs` (i64, computed as `now - first_self_resolution_offered_at`), `prompt_bundle_id` (string), `occurred_at_unix` (i64).

  Both events MUST carry the dispute id so downstream tooling can join them. The two-event pair lets the operator compute SC-001 (cooperative-case detection volume) and SC-002 (cooperative-case external-resolution rate, plus median elapsed). No new metrics endpoint; the existing `tracing` setup is the metrics surface for this codebase.
- [ ] T030 [P] Update `config.example.toml` (or whichever operator-facing example config the repo ships) with the two new `[mediation]` keys and the operator-facing comment block from `contracts/config.md`.
- [ ] T031 Run the `quickstart.md` walkthrough end-to-end against a local daemon: trigger a session that produces `CoordinationFailureResolvable` confidence ≥ 0.75 (via a real or mocked reasoning provider), verify the daemon log lines match the expected sequence, verify the `mediation_events` rows match the audit-row sequence invariant, and verify the kill-switch SC-007 recipe.
- [ ] T032 Run `cargo clippy --all-targets --all-features` and address any new lints introduced by this feature. The codebase currently runs clippy-clean; preserve that property.
- [ ] T033 Run `cargo fmt --all` to apply the existing formatter rules.
- [ ] T034 Run the full test suite (`cargo test`). Confirm all 267+ existing tests pass and the new tests from US1/US2/US3/Polish (T015, T019, T020, T025, T026, T027) pass. Zero regressions tolerated.

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)**: No dependencies — can start immediately.
- **Foundational (Phase 2)**: Depends on Setup. **BLOCKS** all user-story phases.
- **User Story 1 (Phase 3)**: Depends on Foundational. P1 / MVP. Tasks within parallelizable per the [P] markers.
- **User Story 2 (Phase 4)**: Depends on Foundational AND User Story 1's T017 (`session_has_self_resolution_offered` helper is shared). T021 / T022 / T023 / T024 can run in parallel once T017 lands; T025 depends on T021–T024.
- **User Story 3 (Phase 5)**: Depends on Foundational. T026 also reuses session shapes from US1, so it's easier to write after US1's T019 lands. No code dependency on US2 — US3 can ship with US1 alone if needed.
- **Polish (Phase 6)**: Depends on all desired user stories being complete. T027 is a regression test that requires T016 + T018 (US1 implementation). T028, T029, T030 are operator-side and can land in parallel with any of US1/US2.

### Within Each User Story

- Models / data shapes (Phase 2 tasks T003–T008) before code that depends on them.
- T017 (helper) before T016 (uses it) and T023 (uses it).
- T016 + T018 (cooperative branch + dispatch) before T019 (test that exercises them).
- T021–T024 (opt-in code) before T025 (test that exercises them).
- T015 (template audit) is independent and can land at any point after T011 + T009 ship.

### Parallel Opportunities (within phases)

**Phase 2 — independent files**: T003, T004, T005, T006, T007, T008 — all touch different files, all parallelizable.

**Phase 3 — partial parallel**: T015 has no dependency on T016–T018 (different file). T016 → T017 → T018 → (T019 || T020) is the serial chain inside the implementation.

**Phase 4 — partial parallel**: T021 || T022 (different files), then T023 (policy file), then T024 (verification, may be a no-op), then T025 (test).

**Phase 6 — heavily parallel**: T027, T028, T029, T030 are all independent. T031 is the integration walkthrough; runs after the others. T032 / T033 / T034 are the pre-merge sanity sweep.

---

## Implementation Strategy

### MVP First (User Story 1 only)

1. Complete Phase 1 (Setup) — small, ~30 min.
2. Complete Phase 2 (Foundational) — the bulk of the data-model + prompt-bundle work, ~1 day.
3. Complete Phase 3 (User Story 1) — the cooperative invitation path itself.
4. **STOP and VALIDATE**: run T019 + T020 + a manual session against a local daemon. Verify the user observation that motivated this feature (party gets a Spanish acknowledgement after confirming receipt) is now resolved.
5. Optionally deploy with `self_resolution_enabled = false` for a few days, capture SC-001 / SC-002 baselines, then flip the switch.

### Incremental Delivery (recommended)

1. Setup + Foundational → foundation ready.
2. Add User Story 1 + the kill-switch test (T027) → ship MVP. Cooperative cases now get a templated invitation; legacy path still available behind kill-switch.
3. Add User Story 2 → ship the opt-in. Now parties have an explicit "I want a human" off-ramp.
4. Add User Story 3 (regression-only) → ship the lock-in regression test.
5. Polish (T028 / T029 / T030 / T031 / T032 / T033 / T034) — can land alongside any of the above.

### Parallel Team Strategy

With multiple developers:

1. Together: Setup + Foundational (T001–T014).
2. Once Foundational is done:
   - **Developer A**: User Story 1 (T015 → T016 → T017 → T018 → T019 → T020).
   - **Developer B**: User Story 2 (T021 || T022, then T023, T024, T025) — depends on T017 from Developer A (sync point).
   - **Developer C**: Polish — T027 (test, depends on US1), T028 (health-check), T029 (counters), T030 (config example).

---

## Notes

- [P] tasks = different files, no incomplete-task dependencies.
- [Story] label maps task to spec.md user-story id for traceability.
- Each user story is independently completable and testable.
- Verify tests pass (and that the legacy path is unchanged with kill-switch off) before merging.
- Commit per task or logical group; the speckit `after_implement` hook offers an auto-commit if you want to opt into it (see `.specify/extensions/git/git-config.yml`).
- This feature ships **zero SQL migrations**. The migration counter on `main` stays at v5 (Phase 4).

---

## Format Validation

All 34 tasks above conform to the strict checklist format:

- ✅ Every line starts with `- [ ]`.
- ✅ Every task has a sequential ID (T001 through T034).
- ✅ User-story phase tasks (T015–T020 for US1, T021–T025 for US2, T026 for US3) carry the `[US1]` / `[US2]` / `[US3]` story labels.
- ✅ Setup, Foundational, and Polish phase tasks do NOT carry a story label.
- ✅ Every task description includes the exact file path it touches.
- ✅ `[P]` markers are applied only where the task is genuinely parallelizable (different file, no incomplete dependency).
