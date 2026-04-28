# Phase 0 Research: Cooperative Self-Resolution Nudge

## Open Questions Resolved

The spec was authored without `[NEEDS CLARIFICATION]` markers (all
informed-default decisions documented in the requirements
checklist). Phase 0 research therefore focuses on **best-practice
investigation** for the dependencies and patterns this feature
touches, plus a deeper look at the policy-boundary question that
emerged in the original conversation that motivated this feature.

---

## Decision: Static repository templates over LLM-authored party text

**Decision**: The party-facing invitation comes from a static
`prompts/phase3-self-resolution.md` file checked into the repo,
parsed at startup into the existing `PromptBundle`. The LLM decides
**only** whether to fire the branch (via classification + confidence);
it never authors any user-visible text on this code path.

**Rationale**:

- **Constitution Principle II — Protocol-Enforced Security
  Boundaries**: a security-critical boundary (no fund-action
  language) MUST NOT depend on an LLM behaving correctly. Static
  templates make the boundary structural: a policy violation can
  only land if a human commits a bad template, and the keyword-audit
  test (FR-004 / SC-003) catches that on every change.
- **Auditability (Principle VI)**: identical template rendered each
  time means the audit row only needs to capture which template
  version (via prompt-bundle hash) and which language was rendered;
  it does not need to inline the rendered text.
- **Translation review**: a static bundle is reviewable by a human
  translator once per language before shipping, and updates land as
  reviewable diffs.
- **Reproducibility for tests**: integration tests can assert
  byte-equality against the rendered output rather than fuzzy-match
  LLM completions.

**Alternatives considered**:

- *LLM-authored invitation with a system-prompt restriction*:
  rejected — violates Principle II (the boundary would depend on
  the LLM honouring the system prompt under prompt-injection
  conditions). The existing Phase 3 `authority_boundary_attempt`
  trigger already escalates when an LLM emits fund-action wording in
  free-form output, but using that as the *only* line of defence
  for a feature whose entire purpose involves party-facing text
  was deemed too risky.
- *LLM-authored with a runtime keyword-strip / sanitiser pass*:
  rejected — keyword stripping is bypassable by paraphrase; doesn't
  meaningfully tighten the boundary.

---

## Decision: Bundle integration via the existing `PromptBundle` loader

**Decision**: Add `self_resolution: SelfResolutionTemplates` (or
similar) as a field on the existing `PromptBundle` struct. The new
`prompts/phase3-self-resolution.md` is parsed by the existing bundle
loader (which currently reads system / classification / escalation /
mediation-style / message-templates Markdown files). The bundle
hash that the engine pins on each session is recomputed
automatically over the new file too.

**Rationale**:

- **Single source of truth for prompt-policy versioning**: a bundle
  hash that already captures everything Serbero is allowed to say
  to anyone is the right scope. Sessions opened against bundle v1
  see v1 templates even after a v2 deployment, which matches the
  existing Phase 3 prompt-pinning contract.
- **Zero new operator surface**: operators don't need a new config
  knob to point at a templates file; the loader already knows the
  bundle path from `[prompts]`.
- **Reuse of the existing audit chain**: the audit row references
  `bundle_id` + `policy_hash`, which already exist; no new audit
  fields needed.

**Alternatives considered**:

- *Inline templates inside `phase3-message-templates.md`*: rejected
  — that file currently scopes to clarification-question templates,
  which is a different policy class (model-authored, parameterised
  text). Self-resolution templates are model-untouched, so a
  separate file is cleaner.
- *Templates as Rust string constants in code*: rejected — defeats
  the "translator can review without a Rust toolchain" goal and
  makes adding a language a code-change rather than a prompts-change.

---

## Decision: Confidence threshold global (not per-classification)

**Decision**: One f32 config key `self_resolution_threshold`
(default 0.75) gates the new branch. The threshold is not
per-classification.

**Rationale**: the new branch only fires for one classification label
(`coordination_failure_resolvable`); per-label tuning is moot until
a future feature opens additional cooperative-style branches for
other labels. When that happens, the threshold can be promoted to a
map keyed by classification label — the migration is purely additive
on the config struct.

**Alternatives considered**:

- *Per-classification threshold map*: rejected — premature
  generalisation.
- *No threshold (always fire on cooperative)*: rejected — even a
  single false-positive cooperative classification on a fraud-
  adjacent case is operationally expensive. The threshold is a
  cheap safety budget.

---

## Decision: One-shot guard via prior `self_resolution_offered` audit row

**Decision**: The guard against re-firing the invitation on a later
round is a SQL existence check against `mediation_events` for a row
with `kind = 'self_resolution_offered'` and matching `session_id`.
The check runs inside `policy::evaluate(...)` (or its caller in
`mediation::follow_up`) before the new branch is selected.

**Rationale**:

- **Crash-safety**: the audit row is committed in the same
  transaction as the outbound rows. A crash between commit and
  publish leaves both the audit row and the outbound rows in place;
  on the next tick the guard sees the audit row and skips the
  branch correctly.
- **Reuse of existing dedup pattern**: the same shape is used
  today by Phase 3 for several once-per-session events (e.g.
  `session_opened`, `summary_generated`).

**Alternatives considered**:

- *In-memory flag on the session struct*: rejected — does not
  survive a daemon restart; would need a backstop check anyway.
- *New boolean column on `mediation_sessions`*: rejected — requires
  a migration for a derived fact already capturable from the audit
  trail.

---

## Decision: Human-assistance opt-in via classifier-output extension

**Decision**: The classifier prompt for round N+1 (after a
`self_resolution_offered` event) gains an additive `human_requested:
bool` JSON field. The classification deserialiser carries it as a
struct field with `serde(default)` so older provider responses (or
providers that don't yet emit the field) still parse cleanly with
the field defaulting to `false`. `policy::evaluate(...)`
short-circuits to `Escalate(EscalationTrigger::PartyRequestedHuman)`
when the field is `true`, regardless of the surrounding
classification label.

**Rationale**:

- **Reuses the existing classification round-trip**: no new model
  call, no new latency.
- **Multilingual coverage for free**: the classifier already runs in
  the party's language; detecting "necesito un humano" / "I want a
  human" / "preciso de ajuda humana" works without per-language
  keyword lists in Rust.
- **Provider portability (Principle X)**: an additive JSON field is
  the lowest-risk change for both the OpenAI-compatible adapter and
  the Anthropic adapter. Each adapter adds the parse + the prompt
  instruction independently.

**Alternatives considered**:

- *Rust-side keyword regex on inbound messages*: rejected —
  multilingual keyword lists are brittle (synonyms, slang, formal
  vs informal registers) and miss the point that the classifier is
  already doing language-aware semantic analysis on every round.
- *A separate model call dedicated to opt-in detection*: rejected —
  adds latency and provider cost for a binary classification that
  the existing call already covers.

---

## Decision: Solver summary fires immediately, in parallel with party invitations

**Decision**: When the new branch fires, the existing
`mediation_summary` notification to the assigned solver fires on
the same tick, with `suggested_next_step =
"self_resolution_offered_to_parties"`. There is no delay, no
"wait-and-see-if-parties-resolve" gate.

**Rationale**:

- **Audit completeness > solver inbox volume**: a solver who filters
  on the new `suggested_next_step` value can mute the cooperative-
  case feed if they want to; a solver who needs visibility (because
  they're paged for the dispute) gets it without a delay.
- **No state sprawl**: there is no "scheduled-summary" intermediate
  state to track.
- **Honest System Behavior (Principle XII)**: the solver receives
  the same factual summary they'd receive on any other cooperative
  case; the only delta is the operational hint that parties have
  been invited to self-resolve, which is true at the moment the
  summary is sent.

**Alternatives considered**:

- *Delay the summary by 1h (bet on self-resolution before paging
  the solver)*: rejected — operationally fragile (timer state to
  cancel on cooperative closure, tick-based scheduling, edge cases
  on daemon restart) for marginal benefit.
- *Skip the summary when invitation fires*: rejected — kills the
  solver's visibility into cooperative cases that don't actually
  resolve cooperatively. SC-004 explicitly tracks the silence rate
  as a feature-health metric; the solver-side summary is the
  fallback path that catches stalled cooperative cases.

---

## Decision: Threshold inclusivity (`≥`, not `>`)

**Decision**: The branch fires when `confidence >=
self_resolution_threshold`. A confidence value exactly equal to
the threshold triggers the invitation.

**Rationale**: matches the rest of the policy code (which already
uses `>=` on the existing low-confidence escalation gate) and
avoids the surprise of a threshold value that "never fires" because
operators set it to a round number that the model happens to emit
exactly.

---

## Decision: Multilingual support: English / Spanish / Portuguese initial set, extensible by template append

**Decision**: The initial bundle ships sections for `[en]`, `[es]`,
`[pt]`. New languages are added by appending a `[xx]` section to
the bundle file; no Rust changes required. The keyword-audit unit
test walks every section, so a new section ships only after the
audit passes for it.

**Rationale**:

- The existing transcript pipeline already supports independent
  per-party language detection across these three; reusing that
  detection is essentially free.
- Append-only language support keeps the operator workflow simple:
  PR adding `[de]` template + audit-test pass = one file changed.

**Alternatives considered**:

- *Single English template, translate at delivery time via the LLM*:
  rejected — re-introduces LLM-authored party text, which violates
  Principle II.
- *Ship many languages on day one*: rejected — out-of-scope per the
  spec. Adding a language post-launch is documented and trivial.

---

## Open Risks Acknowledged at Plan Time (not blockers)

- **R-001 — Translation review process**: SC-003 covers literal
  banned-keyword leakage but not subtler issues like a phrase that
  is grammatically correct but culturally implies a fund-action.
  Mitigation deferred: relies on human translator review at the
  PR that adds each language section. Documented in the bundle
  contract (`contracts/template-bundle.md`).
- **R-002 — Pre-feature baseline metrics for SC-001 / SC-002**: the
  spec's success criteria reference a 7-day baseline for
  "cooperative cases that resolved without solver intervention" and
  "median time from cooperative-detection to dispute closure". The
  daemon doesn't currently emit a structured metric for either.
  Mitigation: instrument those two counters as part of the
  implementation tasks (Phase 2) so the baseline can be captured
  from the feature-on-but-kill-switch-off period. SC-007 (kill-
  switch off = byte-for-byte legacy) makes the kill-switch-off
  baseline trivially capturable.
- **R-003 — Provider drift on `human_requested`**: an out-of-date
  provider that does not return the new field still parses cleanly
  (`serde(default)` makes it `false`), but the opt-in path will
  silently fail to fire for that provider. Mitigation: at startup,
  the reasoning health-check round trip can include a
  `human_requested: true` example in the system prompt and assert
  the parsed response carries the field; if not, log a warning at
  `info!` level. Captured as a Phase 2 task, not a hard gate.

---

## Best-Practice References Consulted

- Existing Phase 3 spec (`specs/003-guided-mediation/spec.md`) for
  the prompt-bundle pinning pattern and the "model decides whether,
  templates author what" split that this feature mirrors.
- Existing `mediation_events` schema (no migration needed; `kind`
  is unconstrained TEXT) and the audit-row payload conventions
  established by `summary_generated` and `escalation_recommended`.
- The `ReasoningProvider` trait surface in `src/reasoning/mod.rs`
  for the additive-JSON-field portability pattern (already used by
  several existing classifier-output fields).
- The 2026-04-27 production transcript on dispute
  `096fb2e4-7eca-4f59-96e8-ae49f69d1328` as the canonical
  motivating case; the seller's "ya recibí el fiat" reply is the
  reference shape for "cooperative case, high confidence" in
  user-story 1.

---

**Phase 0 status**: Complete. No `NEEDS CLARIFICATION` markers
remain. Ready to advance to Phase 1.
