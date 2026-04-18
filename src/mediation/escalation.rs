//! Phase 4 handoff assembly (US4 — not yet implemented).
//!
//! Transitions the session to `escalation_recommended`, records the
//! `EscalationTrigger` and evidence refs in `mediation_events`, and
//! assembles the Phase 4 handoff package. Phase 3 does NOT execute
//! the escalation itself.

// TODO(US4): recommend(ctx, session, trigger, evidence_refs).
