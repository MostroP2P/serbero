# Phase 3 Classification Policy (STUB — not live content)

Structural stub so the bundle hash is stable. Fill in the real
classification criteria before production use. See
`specs/003-guided-mediation/spec.md` §"AI Agent Behavior Boundaries"
and `contracts/reasoning-provider.md` for the label set.

## Scope

- Classify each mediation turn into one of:
  `CoordinationFailureResolvable`, `ConflictingClaims`,
  `SuspectedFraud`, `Unclear`, `NotSuitableForMediation`.
- Each classification carries a confidence score and a rationale.
  The rationale is stored in the audit store (`reasoning_rationales`),
  never in general logs.

## Rules / Guidance

- TODO: enumerate the distinguishing features of each label.
- TODO: state explicit thresholds for when classification yields
  `Escalate` regardless of label.

## Examples

- TODO: per-label illustrative cases.
