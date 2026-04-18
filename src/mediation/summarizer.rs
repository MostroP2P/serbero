//! Cooperative summary pipeline (US3 — not yet implemented).
//!
//! Produces a `MediationSummary` for the assigned solver at the
//! `CoordinationFailureResolvable` path, persists it to
//! `mediation_summaries`, and records the full rationale in the
//! controlled audit store (`reasoning_rationales`). General logs
//! see only classification + confidence + `rationale_id` (FR-120).

// TODO(US3): summarize(ctx, session) -> Result<MediationSummary>.
