//! Mediation engine (US1+ — not yet implemented).
//!
//! The engine spawned from `daemon::run` will, when implemented,
//! drive each open `mediation_sessions` row through classification,
//! clarifying-message drafting, summary delivery, and Phase 4
//! escalation-handoff preparation. See `plan.md` §Module
//! Architecture.
//!
//! This skeleton keeps the boundary clean so T017–T019 (daemon
//! wiring) can import the module and treat Phase 3 as cleanly
//! disabled until US1 lands.

pub mod auth_retry;
pub mod escalation;
pub mod policy;
pub mod router;
pub mod session;
pub mod summarizer;

// TODO(US1): implement engine task entry point run(ctx, shutdown).
