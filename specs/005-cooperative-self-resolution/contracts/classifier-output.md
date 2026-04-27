# Contract: Classifier-Output Surface (Additive `human_requested` Field)

## Scope

This feature extends the existing classifier-call surface with one
additive field, `human_requested: bool`. The change is **additive
on the wire, additive on the Rust struct, and provider-portable**.
Both reasoning adapters in the codebase (OpenAI-compatible and
Anthropic) MUST update their prompt + parser pair to honour this
contract before the feature can ship for that provider.

## Wire-Level JSON Schema (Round N+1, after a `self_resolution_offered` event)

```json
{
  "classification": "coordination_failure_resolvable",
  "confidence": 0.83,
  "suggested_action": "summarize",
  "buyer_clarification": "<existing field>",
  "seller_clarification": "<existing field>",
  "rationale": "<existing field>",
  "human_requested": false
}
```

The `human_requested` field is a plain JSON boolean. It is
**only requested by the prompt** on rounds following a
`self_resolution_offered` audit event for the session. Other
rounds may or may not include the field; if they do, its value
is ignored by `policy::evaluate(...)` (the short-circuit
documented below only fires after a `self_resolution_offered`
event has been recorded for the session).

## Prompt-Side Instruction

The classifier prompt for round N+1 (after the cooperative
invitation) gains, near the existing classification-instruction
block, an additional sentence and one example list:

```
human_requested (boolean): Set to true if and only if the latest
party reply contains an explicit, unambiguous request for a human
solver / mediator / arbitrator. Examples:
  - "I want a human"
  - "necesito un humano"
  - "please escalate to a person"
  - "que un humano lo revise"
  - "preciso de um humano"
Vague phrasings like "this is taking too long" or "I'm frustrated"
do NOT count. When in doubt, set to false.
```

The "when in doubt, set to false" clause is intentional: a
false negative defers escalation by one round (the user typically
re-states); a false positive escalates to human prematurely on a
case the parties might have resolved themselves.

## Rust-Side Parsing

`ClassificationResponse` (`src/models/reasoning.rs`) gains:

```rust
pub struct ClassificationResponse {
    // ... existing fields ...

    #[serde(default)]
    pub human_requested: bool,
}
```

`serde(default)` covers two scenarios:

1. A provider that hasn't yet been updated to emit the field.
   The struct deserialises with `human_requested = false`; the
   opt-in path silently never fires for that provider until the
   provider's prompt + parser are updated. A startup-time health-
   check (R-003 in `research.md`) logs a warning when the
   provider doesn't echo a probe.
2. Round 0 / round 1 responses where the prompt didn't request
   the field. Same default; no false escalation.

## Policy-Side Behaviour

`policy::evaluate(...)` adds **one** short-circuit before the
existing classification-label dispatch:

```rust
fn evaluate(...) -> PolicyDecision {
    if classification.human_requested && session_has_self_resolution_offered(session_id) {
        return PolicyDecision::Escalate(EscalationTrigger::PartyRequestedHuman);
    }
    // ... existing logic ...
}
```

The `session_has_self_resolution_offered(...)` predicate is a
cheap SQL-level existence check against `mediation_events` —
the same shape used by the new dispatch arm's one-shot guard.
Coupling the short-circuit to "an invitation has actually been
sent for this session" avoids two failure modes:

- An adversarial party trying to skip mediation by emitting
  human-assistance phrasing on round 0 before any invitation
  fires (covered by the existing `Escalate` paths anyway, but
  defence-in-depth here is cheap).
- A buggy provider that mistakenly emits `human_requested = true`
  on a round where the prompt did not request the field (the
  predicate guard means the field is only consulted when the
  prompt would have requested it).

## Provider Coverage

| Provider | Required updates | Status (at plan time) |
|----------|------------------|-----------------------|
| OpenAI-compatible (PPQ.ai, OpenAI direct) | classifier prompt template emits the field on round N+1; response parser accepts it. | Not yet implemented; lands in Phase 2 tasks. |
| Anthropic | classifier prompt template emits the field on round N+1; response parser accepts it. | Not yet implemented; lands in Phase 2 tasks. |
| Future providers | Same two changes. The portability invariant is a Phase 0 constitution-check item (Principle X). | Out of scope for this feature. |

Both adapters MUST be updated in the same PR set so a deploy
that runs Anthropic in production cannot accidentally lose the
opt-in path because the feature shipped only against the OpenAI
adapter.

## Backward Compatibility With Old Sessions

A session opened before this feature ships has no
`self_resolution_offered` audit row; the policy short-circuit
predicate returns false; the new field has no effect on the
session's classification rounds. The only externally observable
change for old sessions is that the `human_requested` field
appears on classifier-call traces at `debug!` level; this is
data-shape additive and does not break any existing parser.
