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
