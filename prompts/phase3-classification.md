# Phase 3 Classification Policy

## Scope

This document defines how each mediation turn is classified. The
classification drives the policy layer's decision.

## Labels

- **CoordinationFailureResolvable**: Both parties appear to be acting
  in good faith but have a coordination problem (payment timing,
  communication gap, process misunderstanding). Cooperative path.
- **ConflictingClaims**: Mutually exclusive factual claims with no
  resolution path visible. Escalate immediately.
- **SuspectedFraud**: Evidence of deliberate bad faith (fake proofs,
  social engineering, known scam patterns). Escalate immediately.
- **Unclear**: Insufficient information to classify confidently. Ask
  a targeted clarifying question if rounds remain.
- **NotSuitableForMediation**: Dispute type or circumstances fall
  outside guided mediation scope. Escalate immediately.

## Confidence Score

- Range: 0.0 to 1.0.
- Below 0.5: policy layer escalates with LowConfidence regardless
  of label.
- Reflects how well evidence supports the label, not probability
  of resolution.

## Flags

- **FraudRisk**: Indicator of deliberate bad faith. Triggers
  immediate escalation with FraudIndicator.
- **ConflictingClaims**: Mutually exclusive assertions. Triggers
  immediate escalation.
- **LowInfo**: Insufficient data. Informational only.
- **UnresponsiveParty**: One party has not replied. Informational;
  the timeout trigger handles escalation.
- **AuthorityBoundaryAttempt**: Model output attempted to cross the
  authority boundary. Triggers immediate escalation.

## Rationale

Every classification MUST include a rationale explaining the chosen
label and confidence. Stored in the audit store only; referenced by
id in general logs (FR-120).
