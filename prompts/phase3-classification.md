# Phase 3 Classification Policy

## Scope

This document defines how each mediation turn is classified. The
classification drives the policy layer's decision.

## Labels

The model MUST emit the canonical snake_case token (left of the
parenthesis) — these are the only strings the OpenAI parser accepts.
The PascalCase name is the Rust enum variant, kept here for
cross-reference with the audit code paths.

- **`coordination_failure_resolvable`** (`CoordinationFailureResolvable`):
  Both parties appear to be acting in good faith but have a
  coordination problem (payment timing, communication gap, process
  misunderstanding). Cooperative path.
- **`conflicting_claims`** (`ConflictingClaims`): Mutually exclusive
  factual claims with no resolution path visible. Escalate immediately.
- **`suspected_fraud`** (`SuspectedFraud`): Evidence of deliberate bad
  faith (fake proofs, social engineering, known scam patterns).
  Escalate immediately.
- **`unclear`** (`Unclear`): Insufficient information to classify
  confidently. Ask a targeted clarifying question if rounds remain.
- **`not_suitable_for_mediation`** (`NotSuitableForMediation`): Dispute
  type or circumstances fall outside guided mediation scope. Escalate
  immediately.

## Confidence Score

- Range: 0.0 to 1.0.
- Below 0.5: policy layer escalates with LowConfidence regardless
  of label.
- Reflects how well evidence supports the label, not probability
  of resolution.

## Flags

Same convention as Labels: emit the canonical snake_case token; the
PascalCase name is the Rust enum variant.

- **`fraud_risk`** (`FraudRisk`): Indicator of deliberate bad faith.
  Triggers immediate escalation with `fraud_indicator`.
- **`conflicting_claims`** (`ConflictingClaims`): Mutually exclusive
  assertions. Triggers immediate escalation.
- **`low_info`** (`LowInfo`): Insufficient data. Informational only.
- **`unresponsive_party`** (`UnresponsiveParty`): One party has not
  replied. Informational; the timeout trigger handles escalation.
- **`authority_boundary_attempt`** (`AuthorityBoundaryAttempt`): Model
  output attempted to cross the authority boundary. Triggers
  immediate escalation.

## Rationale

Every classification MUST include a rationale explaining the chosen
label and confidence. Stored in the audit store only; referenced by
id in general logs (FR-120).

## Clarifying Questions (per-party)

When `suggested_action = ask_clarification`, emit TWO distinct
questions, one for each party, in fields `buyer_clarification` and
`seller_clarification`. Each question is delivered only to its
addressee — the buyer never sees the seller's question and vice
versa.

Write each question in the role's second person and ask for exactly
what that role could supply:

- `buyer_clarification`: addressed to the buyer. Ask about the
  buyer's actions and evidence (fiat payment sent? proof of transfer
  — method, timestamp, reference, redacted screenshot; or, if not
  sent, what blocked it).
- `seller_clarification`: addressed to the seller. Ask about the
  seller's observations (fiat payment received? if so when and by
  what method with a redacted statement for the expected window; if
  not, what proof the buyer has shared).

Hard rules:

- Do NOT prefix the text with role labels like "Buyer:" or
  "Seller:". The transport layer handles recipient routing; these
  prefixes only leak confusion into the other party's chat if they
  ever land on the wrong side.
- Both strings MUST be non-empty. If you cannot produce a useful
  question for one side, pick a different `suggested_action`
  (`summarize` or `escalate`) instead of emitting a half-populated
  clarification.
- Each question stands on its own — don't cross-reference the other
  party's text, since each party only ever sees theirs.

## Per-Party Language (Feature 005)

The classifier MUST also emit two top-level fields:

- `buyer_language` (string | null): ISO-639-1 code (e.g. `"en"`,
  `"es"`, `"pt"`) inferred from the buyer's most recent reply. Set
  to `null` when the latest message has no buyer content or is too
  short to disambiguate. Required on **every** round, not only on
  rounds following a `self_resolution_offered` event — the runtime
  uses the codes to drive the cooperative-self-resolution dispatch
  arm and keeps round-0/1 ready in case the cooperative branch
  fires later.
- `seller_language` (string | null): same shape, for the seller.

## Human-Assistance Opt-In (Feature 005, conditional)

On rounds following a `self_resolution_offered` audit event for the
session — the runtime appends the request to the prompt only on
those rounds — the classifier MUST also emit:

- `human_requested` (boolean): `true` if and only if the latest
  party reply contains an explicit, unambiguous request for a human
  solver / mediator / arbitrator. Examples: `"I want a human"`,
  `"necesito un humano"`, `"please escalate to a person"`, `"que un
  humano lo revise"`, `"preciso de um humano"`. Vague phrasings like
  `"this is taking too long"` or `"I'm frustrated"` do **NOT** count.
  When in doubt, set to `false` — a false negative defers escalation
  by one round (the user re-states); a false positive escalates to
  human prematurely on a case the parties might have resolved.
