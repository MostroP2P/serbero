# Quickstart: Cooperative Self-Resolution Nudge

This document is the operator / reviewer's "what should I expect to
see when this feature is on" guide. It complements `spec.md` (the
business-facing description) and `plan.md` (the implementation
shape) with a hands-on walkthrough that mirrors each user story
end-to-end.

## Prerequisites

Before this feature can be exercised:

- **`main` carries the `summary_delivered` lifecycle fix** that
  defers `summary_delivered → closed` to the `dispute_resolved`
  handler. Without it, the engine reopens duplicate sessions
  mid-coordination and the cooperative invitation becomes
  ineffective. (Already shipped in PR #47.)
- **A reasoning provider that emits the `human_requested` field
  on round N+1**. As of this feature's plan date neither
  adapter (OpenAI-compatible, Anthropic) emits it; a Phase 2
  task in this feature ships that update.
- **An updated `prompts/phase3-self-resolution.md` bundle file**
  with at minimum `[en]` populated. Templates land in the same PR
  as the code; the keyword-audit unit test refuses to merge a
  bundle with banned substrings.

## Configuration (operator-side)

Two new keys under `[mediation]` in `config.toml`:

```toml
[mediation]
self_resolution_threshold = 0.75   # (default)
self_resolution_enabled = true     # (default; set false to kill-switch the feature)
```

See `contracts/config.md` for the full operator-facing notes.

## User Story 1 — Cooperative resolves without solver action

### What you'd see in the daemon log

```
INFO  serbero::mediation::policy: classify_for_round: rationale persisted
      session_id=… classification=coordination_failure_resolvable
      confidence=0.85 rationale_id=…
DEBUG serbero::mediation::policy: evaluate: SuggestSelfResolutionWithSummary
      session_id=… confidence=0.85
INFO  serbero::mediation::self_resolution: invitation dispatched to both parties
      session_id=… buyer_lang=es seller_lang=en
INFO  serbero::mediation: solver_summary_delivered
      session_id=… suggested_next_step=self_resolution_offered_to_parties
INFO  serbero::mediation::session: session reached summary_delivered
      session_id=…
```

### What the parties see

Each party receives **one** gift-wrap message in their detected
language. The buyer sees the `[en]` (or detected language) template;
the seller sees the `[es]` (or detected language) template; both
end with the human-assistance opt-in sentence. No `Buyer:` or
`Round N.` prefixes — the chat-scaffolding cleanup already shipped
in `main` ensures that.

### What the assigned solver sees

Exactly **one** `mediation_summary` notification, identical in
structure to today's cooperative-summary DM, with one delta:

```
suggested_next_step: self_resolution_offered_to_parties
```

A solver UI / dashboard / log search filtering on this literal can
distinguish a self-resolution-offered case from a vanilla
cooperative summary at a glance.

### What the audit table records

```sql
SELECT kind, occurred_at FROM mediation_events
WHERE session_id = '<uuid>' ORDER BY occurred_at;
```

Should show, in order:

```
session_opened
classification_produced
self_resolution_offered     <-- NEW
summary_generated
session_closed              (later, when dispute_resolved fires)
```

### Verifying SC-006 (one-shot)

Run the engine for a session that produces a second
`coordination_failure_resolvable` classification with high
confidence on a later round. The audit table MUST contain
**exactly one** `self_resolution_offered` row. The new branch
short-circuits via the existence check.

## User Story 2 — Party opts in to human assistance

### Setup

Continue the User Story 1 session. After the invitation lands,
seed a buyer reply such as `"necesito un humano que revise esto"`
and let the engine ingest it.

### What you'd see in the daemon log

```
DEBUG serbero::reasoning::openai: openai reasoning call response
      attempt=0 model=… content_len=… content_sha256_prefix=…
INFO  serbero::mediation::policy: evaluate: human_requested=true
      session_id=… → Escalate(party_requested_human)
INFO  serbero::mediation::escalation: recommend: party_requested_human
      session_id=… handoff_event_id=…
INFO  serbero::escalation::dispatcher: dispatched escalation
      session_id=… solver=…
```

### What the audit table records (additionally)

```
classification_produced (round N+1)
escalation_recommended (trigger=party_requested_human)
handoff_prepared
```

The `handoff_prepared` row is consumed by the existing Phase 4
dispatcher; a write-permission solver receives the handoff DM.

### Verifying SC-005 (one-engine-cycle latency)

Time from "buyer reply ingested" to "escalation_recommended row
committed" should be ≤ one engine tick. The cooperative invitation
adds zero latency to this path because it routes through the
existing escalation pipeline.

## User Story 3 — No lock-in on cooperative branch

### Setup

Continue the User Story 1 session. After the invitation lands,
seed a buyer reply such as `"the seller never released, they
lied"` and let the engine ingest it.

### What you'd see in the daemon log

```
INFO  serbero::mediation::policy: classify_for_round
      session_id=… classification=conflicting_claims confidence=0.92
INFO  serbero::mediation::policy: evaluate
      session_id=… → Escalate(conflicting_claims)
INFO  serbero::mediation::escalation: recommend: conflicting_claims
```

The session escalates under the standard `conflicting_claims`
trigger, **not** under `party_requested_human`. The previous
`self_resolution_offered` event does not bias the new round's
classification.

## SC-007 Verification: Kill-switch off, byte-for-byte legacy

To confirm the kill-switch is honoured:

```toml
[mediation]
self_resolution_enabled = false
```

Restart the daemon. Run two side-by-side sessions:

1. A session with the feature on (`self_resolution_enabled = true`)
   that triggers a high-confidence cooperative classification.
2. A session with the feature off (`self_resolution_enabled =
   false`) that triggers the same classification.

Diff the audit-event rows for the two sessions:

```
session 1: session_opened, classification_produced,
           self_resolution_offered, summary_generated, …
session 2: session_opened, classification_produced,
           summary_generated, …
```

The only difference MUST be the absence of
`self_resolution_offered` in session 2. The
`summary_generated` payload's `suggested_next_step` field for
session 2 must carry the legacy value (whatever today's
cooperative-summary path uses), not
`"self_resolution_offered_to_parties"`.

## Running the keyword-audit test

```
cargo test --test phase3_self_resolution_template_audit
```

Should pass on every PR. If it fails, the bundle file contains a
banned fund-action keyword in some language section; fix the
template before merging.

## Dry-run for a new language section

When adding `[de]` (or any new language):

1. Append the section to `prompts/phase3-self-resolution.md`.
2. Extend the banned-substring map in
   `tests/phase3_self_resolution_template_audit.rs` with the
   German equivalents (`freigeben`, `bezahlen`, `abbrechen`,
   `überweisen`, `erstatten`, `auszahlen`, …).
3. Run `cargo test --test phase3_self_resolution_template_audit`
   locally.
4. Open the PR with both files in the same change set so a
   reviewer can confirm the audit covers the new translation.

## Forensic Replay

To reconstruct what a party saw on a past
`self_resolution_offered` event:

1. Look up the event row:

   ```sql
   SELECT prompt_bundle_id, policy_hash, payload_json
   FROM mediation_events
   WHERE kind = 'self_resolution_offered' AND session_id = '<uuid>';
   ```

2. The `prompt_bundle_id` plus `policy_hash` pin the exact bundle
   version. Check out the corresponding commit and re-render the
   message for the language code in `payload_json.languages.<party>`.
3. The rendered string is exactly what the party received (no
   per-session interpolation).

This replay path is the same one used by the existing Phase 3
forensic process for `summary_generated` and other audit rows;
no new tooling required.
