# Implementation Plan: Cooperative Self-Resolution Nudge

**Branch**: `005-cooperative-self-resolution` | **Date**: 2026-04-27 | **Spec**: [spec.md](./spec.md)
**Input**: Feature specification from `/specs/005-cooperative-self-resolution/spec.md`

## Summary

Extend the Phase 3 mediation pipeline with a single new policy branch
that fires when classification is `coordination_failure_resolvable`,
suggested action is `summarize`, confidence ≥ a configurable
threshold (default 0.75), and no prior `self_resolution_offered`
event exists for the session. The branch dispatches **two outputs in
parallel**:

1. **Two party-facing messages** through the existing chat transport,
   one to the buyer and one to the seller, in their detected
   languages, drawn from a **static repository-hosted template
   bundle** (`prompts/phase3-self-resolution.md`). The bundle lives
   alongside the existing prompt artifacts and is loaded at startup
   into the same `PromptBundle` infrastructure used by the rest of
   Phase 3. The model decides only WHETHER to fire the branch; it
   never authors the party-facing text.
2. **The existing structured solver summary** through the existing
   `mediation_summary` notifier path, with `suggested_next_step =
   "self_resolution_offered_to_parties"` so the solver can
   distinguish a self-resolution-offered case from a vanilla
   cooperative summary at a glance.

Two new audit / control surfaces ride on the existing infrastructure:

- A new `MediationEventKind::SelfResolutionOffered` variant
  (audit-only — no schema change; the `mediation_events.kind` column
  is already an unconstrained TEXT field).
- A new `EscalationTrigger::PartyRequestedHuman` variant emitted by
  `policy::evaluate` when the classifier on a follow-up round flags
  an explicit human-assistance request from a party.

Configuration is two new keys under `[mediation]` —
`self_resolution_threshold` (f32, default 0.75) and
`self_resolution_enabled` (bool, default true; kill-switch). With
the kill-switch off, the pipeline behaves byte-for-byte the same as
it does today (SC-007).

The feature is **strictly additive**: no DB migration, no changes to
the existing `Summarize` decision path beyond adding a new sibling
variant. The default / happy-path session lifecycle still ends at
`summary_delivered` (exactly as the recently-shipped fix in `main`
left it). For the explicit human-assistance opt-in, however, a new
`SummaryDelivered → EscalationRecommended` edge lets the session
re-open into the Phase 4 dispatcher when a party reply asks for a
human after the cooperative invitation. The carve-out is gated on
the presence of a prior `self_resolution_offered` audit row, so
legacy `summary_delivered` sessions stay terminal exactly as before.

## Technical Context

**Language/Version**: Rust stable, edition 2021 (same toolchain as Phases 1/2/3 and Phase 4).
**Primary Dependencies**: `nostr-sdk 0.44.1` (gift-wrap transport, reused), `mostro-core 0.9.1`, `rusqlite` (bundled, no new migration needed), `tokio` (existing runtime), `serde` + `serde_json` (config + classifier output deserialisation), `tracing`, `thiserror`. **No new crate pulls.** The reasoning provider trait already supports the existing classification round trip; the new `human_requested: bool` field rides through it as an additive struct field.
**Storage**: SQLite. **No migration.** The new audit event kind reuses the existing `mediation_events` table (kind is TEXT). The new policy decision variant and the new escalation trigger variant are pure Rust enum extensions.
**Testing**: `cargo test` + the existing integration-test harness (`nostr-relay-builder::MockRelay`, `common::SolverListener`, in-memory rusqlite, scripted `MockReasoningProvider`). No new test dependency. Five new integration-test binaries under `tests/` (one per user-story slice plus a keyword-audit unit test for the templates).
**Target Platform**: Linux server (the daemon is already Linux-targeted; no platform-specific code added).
**Project Type**: Single-binary Rust daemon — the existing `serbero` binary gains one new prompt bundle file, a small set of model / policy enum extensions, and one new dispatch arm in `mediation::follow_up`.
**Performance Goals**: SC-005 — "session reaches the assigned solver within one engine cycle" on the human-assistance opt-in path is met automatically by reusing the existing escalation pipeline, which already operates at one-engine-cycle latency. No new perf budget needed.
**Constraints**: SC-007 (zero externally observable behaviour change with kill-switch off) demands the new branch be gated cleanly so it cannot fire when `self_resolution_enabled = false`. FR-004 / SC-003 require an automated fund-action keyword check on every change to the template bundle. FR-009 + FR-120 (carried over from Phase 3) require the audit row to reference rationale-id only, never inline rationale text.
**Scale/Scope**: Same scale as the existing Phase 3 mediation pipeline. The new branch fires at most once per session and adds two outbound gift-wrap publishes (already the cost of any `AskClarification` round) plus one extra audit-event row. Negligible incremental load.

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

All 13 Serbero-constitution principles are satisfied by the
cooperative self-resolution spec. Mapping below is the evidence
examined at this gate; all references are to FRs and SCs in
`spec.md`.

| # | Principle | Compliance source |
|---|-----------|-------------------|
| I | Fund Isolation First | FR-004 — invitation text MUST NOT name, instruct, suggest, or imply any fund-moving action; verified by an automated keyword check (SC-003). The feature does not grant Serbero any new permission, does not call admin-settle / admin-cancel, and does not move funds. |
| II | Protocol-Enforced Security Boundaries | FR-003 — invitation text comes from static repository templates, NOT from the LLM. The model only chooses *whether* to fire the branch; if it picked the wrong classification, the existing `Escalate` paths fire normally. The boundary that "Serbero must not author fund-action text" is enforced structurally by the template-only delivery, not by an LLM-prompt restriction. |
| III | Human Final Authority | FR-008 + US2 — explicit human-assistance opt-in escalates through the existing handoff pipeline. FR-007 — solver always receives the summary in parallel so they can override the cooperative path. US3 — non-cooperative classifications on later rounds escalate normally; the invitation does not lock the session into "cooperative limbo". |
| IV | Operator Notification Is a Core Responsibility | FR-007 — the existing solver-facing summary still fires unchanged, with `suggested_next_step = "self_resolution_offered_to_parties"` as the only delta. No solver visibility is lost compared to today's cooperative-summary path. |
| V | Assistance Without Authority | FR-005 — invitations end with a single sentence offering human assistance; phrasing avoids any framing as a binding decision. The session lifecycle (FR-013) and dispute closure remain untouched by Serbero — Mostro continues to own those. |
| VI | Auditability by Design | FR-009 — every dispatch writes an auditable event with session id, classification confidence, prompt-bundle version. FR-120 / TC-103 carryover: the rationale text is stored once in `reasoning_rationales` and referenced by id; the audit row never inlines it. |
| VII | Graceful Degradation | FR-011 — `[mediation].self_resolution_enabled = false` disables the entire branch without removing templates or modifying code; SC-007 — externally observable behaviour with the kill-switch off is byte-for-byte identical to the pre-feature pipeline. |
| VIII | Privacy by Default | The party-facing invitation reuses the existing per-party gift-wrap envelope (already minimum-disclosure). The solver-facing summary is the same one shipped today; no new audience. The audit row references rationale id, not text. |
| IX | Nostr-Native Coordination | Invitations are dispatched through the existing `chat::outbound` gift-wrap path. No new transport, no new relay subscription, no new event kind. |
| X | Portable Reasoning Backends | The new `human_requested: bool` classifier-output field is a plain JSON boolean; both the OpenAI-compatible adapter and the Anthropic adapter add it as an additive deserialiser field with the same default (`false` when absent), so each provider remains independently shippable. |
| XI | Incremental Scope and Clear Boundaries | Strictly additive: one new policy branch, one new event kind, one new escalation trigger, two config keys, one new prompt-bundle file. Zero changes to Phase 1/2/4 surfaces. The "Out of Scope" section of the spec explicitly forbids touching any non-cooperative classification's flow. |
| XII | Honest System Behavior | The invitation phrasing does not assert any factual claim about the dispute (it describes a *typical* coordination pattern, not a verdict). When confidence is below threshold, the path is silent (FR-012); when classification shifts non-cooperative, the standard escalation fires (US3). |
| XIII | Mostro Compatibility and Separation of Concerns | Serbero never invokes admin-settle / admin-cancel / TakeDispute as part of this feature. The opt-in escalation routes to a *human* solver who runs TakeDispute on their own Mostro instance. The fund-state authority remains entirely in Mostro. |

**Decision**: GATE PASSES. No Complexity Tracking entries needed.

**Post-design re-evaluation (after Phase 1 artifacts)**: data-model.md
(zero schema changes, three new Rust enum variants, two new config
keys, plus one additive state-machine edge `SummaryDelivered →
EscalationRecommended` to support the human-assistance opt-in
firing after a session has reached `SummaryDelivered`),
contracts/template-bundle.md (template-only authoring,
keyword-check obligation, language extensibility rules),
contracts/audit-events.md (`self_resolution_offered` payload pinned
to rationale-id reference), contracts/config.md (kill-switch +
threshold defaults), contracts/classifier-output.md (additive
`human_requested` field), and quickstart.md (SC-007 byte-for-byte
regression check on kill-switch-off path) collectively introduce
no new constitutional violations. The new state-machine edge is
purely additive and does not weaken any existing invariant; the
escalation pipeline reached from `SummaryDelivered` runs through
the existing `escalation::recommend(...)` helper unchanged. Gate
still PASSES.

## Project Structure

### Documentation (this feature)

```text
specs/005-cooperative-self-resolution/
├── plan.md              # This file (/speckit.plan command output)
├── research.md          # Phase 0 output (/speckit.plan command)
├── data-model.md        # Phase 1 output (/speckit.plan command)
├── quickstart.md        # Phase 1 output (/speckit.plan command)
├── contracts/           # Phase 1 output (/speckit.plan command)
│   ├── template-bundle.md     # Static template bundle shape, language extensibility, keyword-check obligation
│   ├── audit-events.md        # `self_resolution_offered` payload + `superseded_by_human` interaction
│   ├── config.md              # `[mediation].self_resolution_*` TOML keys + defaults
│   └── classifier-output.md   # Additive `human_requested` field on the existing classification JSON
├── checklists/
│   └── requirements.md  # Spec quality checklist (/speckit.specify output)
└── tasks.md             # Phase 2 output (/speckit.tasks command - NOT created here)
```

### Source Code (repository root)

This feature is a Phase 3 extension. It adds one new prompt-bundle
file under `prompts/`, a handful of additive model/policy enum
variants, and one new dispatch arm inside the existing
`src/mediation/follow_up.rs`. There is **no new top-level module**
because the surface is too small to justify one — the scope is "one
extra branch on the existing classify→summarize path".

```text
prompts/
├── phase3-system.md                    # MODIFIED: amend the "Allowed" list to include `self_resolution_offered` as an allowed output type, restate the unchanged fund-action prohibition. No structural rewrite.
├── phase3-self-resolution.md           # NEW: static template bundle. Sections per supported language ([en], [es], [pt] initially) with `template` + `human_assistance_optin` strings.
├── phase3-classification.md            # MODIFIED: classifier prompt gains `human_requested: bool` in the JSON-output schema; round-N+1 instructions describe when to set it.
├── phase3-escalation-policy.md         # MODIFIED: document the new `party_requested_human` trigger.
└── (other files)                       # (unchanged)

src/
├── chat/
│   └── outbound.rs                     # (unchanged — existing `build_wrap_with_audience` already supports per-party `m-aud` tag)
├── db/
│   ├── mediation_events.rs             # MODIFIED: extend `MediationEventKind` enum with `SelfResolutionOffered`. NO migration (kind is TEXT).
│   └── (other files)                   # (unchanged)
├── mediation/
│   ├── policy.rs                       # MODIFIED: add `PolicyDecision::SuggestSelfResolutionWithSummary { confidence }`. Extend `evaluate(...)` so that on `(CoordinationFailureResolvable, Summarize)` with confidence ≥ threshold AND no prior `self_resolution_offered` audit row AND `self_resolution_enabled=true`, the new variant is returned. Otherwise fall through to the existing `Summarize` branch.
│   ├── policy.rs (cont.)               # MODIFIED: add the human-requested short-circuit — when `classification.human_requested == true`, emit `Escalate(EscalationTrigger::PartyRequestedHuman)` regardless of label.
│   ├── follow_up.rs                    # MODIFIED: add the dispatch arm for the new `PolicyDecision::SuggestSelfResolutionWithSummary` variant. The arm renders the templates per detected party language, calls the existing `chat::outbound::send_chat_message_with_audience` path, then delegates to the existing `deliver_summary` with `suggested_next_step = "self_resolution_offered_to_parties"`.
│   ├── self_resolution.rs              # NEW (small file): the template-renderer + per-party language picker. Pure-function module so the keyword-audit test can exercise it without spinning up a session.
│   └── (other files)                   # (unchanged — session lifecycle stays at `summary_delivered`)
├── models/
│   ├── config.rs                       # MODIFIED: extend `MediationConfig` with `self_resolution_threshold: f32` (serde default 0.75) and `self_resolution_enabled: bool` (serde default true). The existing config-loading pipeline picks them up automatically.
│   ├── escalation.rs                   # MODIFIED: extend `EscalationTrigger` enum with `PartyRequestedHuman`.
│   ├── reasoning.rs                    # MODIFIED: add `human_requested: bool` (serde default false) on the classification-response struct so both reasoning adapters parse the field uniformly.
│   └── (other files)                   # (unchanged)
├── prompts/
│   └── (loader; reads the new bundle file)   # MODIFIED: add a field to `PromptBundle` carrying the parsed self-resolution templates; bundle hash updated automatically by the existing sha256 over all files in the bundle.
├── reasoning/
│   ├── openai.rs                       # MODIFIED: classifier prompt template emits the additional `human_requested` JSON instruction (round N+1 only); response parser reads the new field. Uses the existing `extract_json_object` + `parse_classification` plumbing — no new branches in the retry loop.
│   ├── anthropic.rs                    # MODIFIED: same JSON-output addition for the Anthropic adapter to keep portability (Principle X).
│   └── (other files)                   # (unchanged)
├── lib.rs                              # MODIFIED: `pub mod mediation::self_resolution;` (or re-export through `mediation::mod`)
└── (other files)                       # (unchanged)

tests/
├── common/mod.rs                       # (unchanged — existing harness covers everything)
├── phase3_self_resolution_happy_path.rs       # NEW: US1 — cooperative classification with high confidence dispatches both party invitations + solver summary; session ends at `summary_delivered`.
├── phase3_self_resolution_opt_in.rs           # NEW: US2 — after invitation, classifier returns `human_requested = true`, session escalates with trigger `party_requested_human`.
├── phase3_self_resolution_no_lock_in.rs       # NEW: US3 — after invitation, next round returns `ConflictingClaims`; session escalates under the standard `conflicting_claims` trigger.
├── phase3_self_resolution_one_shot.rs         # NEW: edge case — repeated cooperative classifications never re-fire the invitation (FR-006 / SC-006).
├── phase3_self_resolution_kill_switch.rs      # NEW: SC-007 — with `self_resolution_enabled = false`, the path is invisible; behaviour matches the pre-feature pipeline byte-for-byte (party-message count, audit-row count, solver-summary count).
└── phase3_self_resolution_template_audit.rs   # NEW: unit test (no DB, no relay) — load the template bundle, walk every (language, section) cell, assert no banned fund-action keyword occurs in any rendered string. Backs FR-004 / SC-003.
```

**Structure Decision**: The existing Phase 1/2/3/4 layout is a
single-project Rust daemon with feature-scoped module trees under
`src/`. This feature does **not** introduce a new top-level module —
the scope is one extra policy branch and one extra dispatch arm,
which fits naturally inside `src/mediation/`. The only new file
under `src/mediation/` is `self_resolution.rs`, a small pure-function
module that owns template rendering and per-party language picking
so the keyword-audit unit test can exercise it without spinning up
a full session. Tests live as top-level integration files under
`tests/` following the established `phase3_*.rs` naming pattern,
one per user-story slice plus the edge-case binaries listed above.

## Complexity Tracking

> **Fill ONLY if Constitution Check has violations that must be justified**

No constitution violations. Table intentionally left empty.
