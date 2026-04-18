//! Solver-Facing Routing (US3 — not yet implemented).
//!
//! Implements the single routing rule defined in `spec.md`
//! §Solver-Facing Routing: targeted when the underlying
//! `disputes.assigned_solver` is set, broadcast via the Phase 1/2
//! notifier otherwise.

// TODO(US3): resolve_recipients(solvers, assigned_solver) -> Recipients.
