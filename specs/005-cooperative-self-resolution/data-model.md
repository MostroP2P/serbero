# Phase 1 Data Model: Cooperative Self-Resolution Nudge

This feature is **strictly additive at the data layer**. There is
**no SQL migration**: every persistent surface this feature touches
already exists. The new shapes are pure-Rust enum variants and a
small parsed-prompt struct held in memory by the existing
`PromptBundle` loader.

The headings below mirror the layers of the existing codebase so a
reviewer can trace each new shape back to its current owner.

## SQL Schema

**No changes.**

The two persistent surfaces this feature uses both already exist:

| Surface | Existing? | What this feature writes |
|---------|-----------|--------------------------|
| `mediation_events` (audit) | Yes (Phase 3, migration v3) | One row per session with `kind = 'self_resolution_offered'`. `kind` is an unconstrained TEXT column, so this is value-level extension only. |
| `mediation_sessions.state` | Yes | The session reaches `summary_delivered` exactly the way it does on the existing cooperative-summary path; this feature does not introduce a new state value. |

**No new tables. No new columns. No new indexes. No migration v6.**

## Rust Enum Extensions

### `MediationEventKind` (in `src/db/mediation_events.rs`)

Add one variant to the existing enum:

```rust
pub enum MediationEventKind {
    // ... existing variants ...
    SelfResolutionOffered,
}
```

`Display` / `FromStr` on this enum already follow the
`snake_case` convention the rest of the codebase uses; the
serialised string is `"self_resolution_offered"`.

### `EscalationTrigger` (in `src/models/escalation.rs`)

Add one variant:

```rust
pub enum EscalationTrigger {
    // ... existing variants ...
    PartyRequestedHuman,
}
```

Serialised string: `"party_requested_human"`. The existing
`Display` / `FromStr` impls cover this automatically per the
codebase pattern.

### `PolicyDecision` (in `src/mediation/policy.rs`)

Add one variant carrying the gate decision:

```rust
pub enum PolicyDecision {
    // ... existing variants ...
    SuggestSelfResolutionWithSummary {
        confidence: f32,
    },
}
```

Sibling of the existing `Summarize { classification, confidence }`.
Carries `confidence` so the dispatch arm can pin it on the audit
row without re-deriving it from the classifier output.

## Rust Struct Extensions

### `MediationConfig` (in `src/models/config.rs`)

Add two fields with sensible defaults:

```rust
pub struct MediationConfig {
    // ... existing fields ...

    #[serde(default = "default_self_resolution_threshold")]
    pub self_resolution_threshold: f32,

    #[serde(default = "default_self_resolution_enabled")]
    pub self_resolution_enabled: bool,
}

fn default_self_resolution_threshold() -> f32 { 0.75 }
fn default_self_resolution_enabled() -> bool { true }
```

Defaults align with FR-010 (threshold) and FR-011 (kill-switch on
by default). `serde(default)` ensures pre-existing operator
config files keep loading without explicit opt-in.

### `ClassificationResponse` (in `src/models/reasoning.rs`)

Add one additive field:

```rust
pub struct ClassificationResponse {
    // ... existing fields ...

    #[serde(default)]
    pub human_requested: bool,
}
```

`serde(default)` covers two cases: an out-of-date provider that
hasn't yet been updated to emit the field, and a provider that
emits it but the round in question is round 0 (where it's
meaningless and the prompt does not request it).

### `PromptBundle` (in `src/prompts/bundle.rs`)

Add one parsed-template field:

```rust
pub struct PromptBundle {
    // ... existing fields ...

    pub self_resolution: SelfResolutionTemplates,
}

pub struct SelfResolutionTemplates {
    /// Keyed by ISO-639-1 language code (`en`, `es`, `pt`, …).
    pub by_language: HashMap<String, SelfResolutionLanguageEntry>,

    /// Default fallback when a party's detected language is not in
    /// `by_language` or confidence is below the language-detection
    /// threshold. MUST be present (typically `en`).
    pub fallback_language: String,
}

pub struct SelfResolutionLanguageEntry {
    pub template: String,            // The cooperative-coordination text.
    pub human_assistance_optin: String, // The opt-in sentence appended after a single space.
}
```

`SelfResolutionLanguageEntry` is the unit of human-translator
review and the unit of the keyword-audit test (FR-004 / SC-003).

The bundle hash that Phase 3 already pins on each session
(`prompt_bundle_id` + `policy_hash` columns on
`mediation_sessions`) extends automatically over the new file
because the loader sha256s the entire bundle directory.

## Audit-Event Payload

`mediation_events.payload_json` for the new
`self_resolution_offered` kind:

```json
{
  "session_id": "<uuid>",
  "classification_confidence": 0.85,
  "rationale_id": "<sha256-content-hash>",
  "languages": {
    "buyer": "es",
    "seller": "en"
  }
}
```

Field-by-field:

- **`session_id`**: redundant with `mediation_events.session_id`
  but kept in the payload so a payload-only export (e.g. a CSV
  dump for forensics) is self-describing.
- **`classification_confidence`**: pinned at the moment of
  dispatch; downstream debugging needs to know whether a borderline
  fire correlated with cooperative resolution outcomes.
- **`rationale_id`**: SHA-256 content-hash reference into
  `reasoning_rationales`. **The rationale text is NEVER inlined
  here** (FR-120 / TC-103 carryover from Phase 3).
- **`languages`**: the per-party detected language code for which
  template was rendered. Useful for SC-004 silence-rate analysis
  segmented by language.

## State Transitions

**No new session states.** The `mediation_sessions.state` lifecycle
graph is unchanged from the post-fix version of `main`:

```
Opening → AwaitingResponse → Classified → SummaryPending → SummaryDelivered
                                                                ↓
                                                              Closed
                                                              (via dispute_resolved)
```

This feature lands the new branch on the
`Classified → SummaryPending` transition (the same edge the
existing `Summarize` decision uses), with extra outbound side-
effects (the two party invitations) before the same
`SummaryPending → SummaryDelivered` edge fires.

The session never re-enters `AwaitingResponse` after the
invitation. The opt-in escalation path (US2) takes the legal
`SummaryDelivered → … → EscalationRecommended` route via the
existing `dispute_resolved` / escalation-recommend pipeline; this
matches the carryover from the recent fix in `main` that closes
`summary_delivered` sessions on dispute resolution.

> Wait — that path needs verifying. `SummaryDelivered →
> EscalationRecommended` is **NOT** in the existing legal
> transitions list (`SummaryDelivered → Closed` is the only edge
> out of `SummaryDelivered`). The opt-in escalation therefore
> applies to a session **before** it reaches `SummaryDelivered` —
> i.e. the round-N+1 classification arrives while the session is
> still in `AwaitingResponse`, and the escalation fires from there
> via the standard `AwaitingResponse → EscalationRecommended` edge
> that already exists.

The corrected transition for User Story 2:

```
AwaitingResponse  ─(self_resolution_offered fired in round N)→  AwaitingResponse
                  ─(round N+1 classification: human_requested = true)→
                  EscalationRecommended  →  Closed
```

This works because the new branch does **not** flip the session to
`SummaryDelivered` immediately — it transitions
`AwaitingResponse → Classified → SummaryPending → SummaryDelivered`
inside the dispatch arm, but the round N+1 path runs **before**
the round-N dispatch lands `SummaryDelivered`. (See
`contracts/audit-events.md` for the precise ordering invariants
the implementation must hold.)

> The Phase 1 design intentionally surfaces this invariant here so
> the implementing engineer doesn't accidentally land the
> `SummaryPending → SummaryDelivered` edge before checking the
> opt-in. The simplest correct implementation: the new dispatch
> arm uses the existing `deliver_summary` helper, which already
> transitions through `SummaryPending → SummaryDelivered` only
> *after* both parties' invitations have been published; the
> opt-in detection runs on the next tick's classification call,
> by which point the session has reached `SummaryDelivered` and
> the escalation must therefore *also* be reachable from there.

**Resolution**: extend `MediationSessionState::can_transition_to`
with one new edge: `SummaryDelivered → EscalationRecommended`. The
edge is only taken when `policy::evaluate(...)` emits
`Escalate(PartyRequestedHuman)` on a session that's already in
`SummaryDelivered`. Audit-trail-wise this is exactly the same
shape as the existing `EscalationRecommended → Closed` edge taken
later by `dispute_resolved`.

(See `contracts/audit-events.md` for the audit-row sequence the
state-machine extension produces.)

## Configuration File Format

`config.toml` additions, surfacing the new keys with their
defaults:

```toml
[mediation]
# ... existing keys ...

# Confidence floor at which Serbero invites parties to coordinate
# the resolution among themselves on a coordination_failure_resolvable
# classification. Range: 0.0..=1.0. Higher = more conservative
# (fewer invitations, fewer false positives). Lower = more
# permissive. Default: 0.75.
self_resolution_threshold = 0.75

# Master kill-switch. When false, the cooperative self-resolution
# branch is bypassed entirely and Serbero behaves byte-for-byte as
# it did before this feature shipped. Default: true.
self_resolution_enabled = true
```

See `contracts/config.md` for the operator-facing documentation
shape.

## Summary of Persistence-Layer Impact

| Layer | Change |
|-------|--------|
| SQLite migrations | None. Migration count stays at v5 (Phase 4). |
| `mediation_events.kind` values | +1 (`self_resolution_offered`). |
| Rust enums | +3 variants (one each on `MediationEventKind`, `EscalationTrigger`, `PolicyDecision`). |
| Rust structs | +2 fields on existing structs (`MediationConfig`, `ClassificationResponse`); +1 new field on `PromptBundle` carrying parsed templates. |
| State machine | +1 edge: `SummaryDelivered → EscalationRecommended`, taken only on the human-assistance opt-in path. |
| Audit-payload schemas | +1 (`self_resolution_offered` payload, no rationale text inlined). |
| Config file | +2 keys under `[mediation]`, both with sensible serde defaults. |
| Prompt bundle | +1 file (`phase3-self-resolution.md`); existing files referenced get amendments documented under `prompts/` in `plan.md`. |
