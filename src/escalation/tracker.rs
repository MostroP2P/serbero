//! Phase 4 tracker — writes the `escalation_dispatches` row and
//! the matching `escalation_dispatched` / `escalation_superseded` /
//! `escalation_dispatch_unroutable` / `escalation_dispatch_parse_failed`
//! audit event in a single transaction (FR-211 invariant).
//!
//! Filled in by T015 / T020 / T023 / T029.
