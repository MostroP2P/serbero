# Contract: Audit-Event Surface

This feature lands one new `MediationEventKind` variant and reuses
two existing variants on a new code path. It does **not** introduce
any new event payload that inlines rationale text (carryover from
FR-120).

## New Event: `self_resolution_offered`

**Kind string**: `"self_resolution_offered"`
**Rust variant**: `MediationEventKind::SelfResolutionOffered`
**Emitted by**: `src/mediation/follow_up.rs`, inside the new
`SuggestSelfResolutionWithSummary` dispatch arm, **before** the
two outbound gift-wraps are published. The audit row is
committed in the same transaction as the two
`mediation_messages` outbound rows (transactional outbox: a
crash between commit and publish leaves both the audit row and
the outbound rows in place; the next tick's idempotency check
sees the audit row and skips re-firing the branch).

**Payload**: see `data-model.md` for the full JSON shape. Recap:

```json
{
  "session_id": "<uuid>",
  "classification_confidence": 0.85,
  "rationale_id": "<sha256-hex>",
  "languages": {
    "buyer": "es",
    "seller": "en"
  }
}
```

**Companion columns** (already on `mediation_events`):

| Column | Value |
|--------|-------|
| `session_id` | the session for which the invitation fires |
| `kind` | `'self_resolution_offered'` |
| `rationale_id` | NULL — the rationale-id reference is in the payload, NOT this column. (Existing convention: this column is reserved for rationales whose lifecycle is owned by the audit row itself; here the rationale is owned by the round-N classification call.) |
| `prompt_bundle_id` | the bundle pinned on the session at dispatch time |
| `policy_hash` | the policy hash pinned on the session at dispatch time |
| `occurred_at` | unix-secs at the moment the row is committed |

## Reused Event: `summary_generated`

**Kind string**: `"summary_generated"` (existing)
**Reused by**: the same dispatch arm, immediately after the
party-facing publishes succeed. The summary delivery flows
through the existing `deliver_summary` helper, which already
writes a `summary_generated` row. **No semantic change** to the
existing payload schema — the only delta is that the summary's
`suggested_next_step` field carries the literal value
`"self_resolution_offered_to_parties"` so a downstream consumer
(solver UI, dashboards, log search) can distinguish a
self-resolution-offered cooperative case from a vanilla one.

The `suggested_next_step` field is part of the existing summary
payload shape; this feature only widens its **value** vocabulary
by one literal.

## Reused Event: `escalation_recommended`

**Kind string**: `"escalation_recommended"` (existing)
**Reused by**: the human-assistance opt-in path (User Story 2)
when `policy::evaluate(...)` short-circuits to
`Escalate(EscalationTrigger::PartyRequestedHuman)`. The existing
`escalation::recommend(...)` helper writes the row; this feature
only widens the **trigger** vocabulary by one variant
(`party_requested_human`).

The escalation payload's `trigger` field is part of the existing
schema. No structural change.

## Audit-Row Sequence Invariant

For a session that takes the cooperative-invitation path, the
audit-row sequence MUST be:

```text
1. session_opened                (existing — emitted at session open)
2. classification_produced       (existing — emitted on round 0 / round 1)
3. self_resolution_offered       (NEW — this feature)
4. summary_generated             (existing — emitted from inside deliver_summary)
5. session_closed                (existing — emitted later by dispute_resolved)
```

Steps 3 and 4 land in the same transaction. Step 5 lands later
when Mostro genuinely resolves the underlying dispute (existing
`dispute_resolved` handler).

## Audit-Row Sequence on the Opt-In Path

If a party opts in to human assistance after the invitation, the
sequence becomes:

```text
1. session_opened
2. classification_produced       (round 0 / round 1)
3. self_resolution_offered
4. summary_generated
5. classification_produced       (round 2: human_requested = true)
6. escalation_recommended        (trigger = party_requested_human)
7. handoff_prepared              (existing — Phase 4 takes over)
8. session_closed                (later, via dispute_resolved)
```

Note that steps 3 and 4 still fire on round 1; the opt-in is
detected on round 2 and produces steps 5 and 6 in that round.

## State Machine Edge Added

This feature adds **one** legal transition to
`MediationSessionState::can_transition_to`:

```rust
| (SummaryDelivered, EscalationRecommended)
```

Required because the opt-in path observes the
`human_requested = true` flag on a round whose session may
already have transitioned to `SummaryDelivered` (the dispatch
arm for the cooperative invitation does walk the session through
`Classified → SummaryPending → SummaryDelivered`). When the
opt-in fires from `SummaryDelivered`, the existing
`escalation::recommend(...)` helper performs the transition; the
new edge makes that transition legal.

The reverse direction (`EscalationRecommended → SummaryDelivered`)
is NOT added — once escalated, the case stays escalated.

## What This Feature Does NOT Audit

- The rendered party-facing message text. The text is determined
  by `(prompt_bundle_id, language)` on the audit row, which a
  forensic process can replay against a frozen snapshot of the
  bundle. The text is NOT inlined in any audit row, in keeping
  with the FR-120 carryover from Phase 3.
- The classifier's full JSON output. The rationale-id reference
  in the payload is the canonical handle into
  `reasoning_rationales`, which is already the pattern for every
  other audit row in the system.
- Per-party delivery success. The existing `notifications` table
  already tracks per-recipient send outcomes for the solver
  summary; the party-facing publishes use the same outbound chain
  and inherit its retry / `Failed` row behaviour from Phase 3.
