//! Policy-layer validation of reasoning output (US3/US4 — not yet
//! implemented).
//!
//! Owns the evaluator defined in
//! `contracts/reasoning-provider.md` §Policy-Layer Validation and
//! suppresses any suggestion that would cross the Phase 3 authority
//! boundary (fund actions, dispute closure). Those suppressed
//! outputs MUST escalate with trigger `AuthorityBoundaryAttempt`.

// TODO(US3/US4): evaluate(classification_or_summary) -> PolicyDecision.
