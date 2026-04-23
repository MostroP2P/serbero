//! Phase 4 router — applies the FR-202 recipient rules.
//!
//! Four mutually-exclusive outcomes:
//!
//! 1. `Targeted(pubkey)` — a non-NULL `assigned_solver` matches a
//!    configured local solver that carries `Write` permission. The
//!    dispatcher sends to exactly that pubkey.
//! 2. `Broadcast { pubkeys, via_fallback: false }` — no matching
//!    assignment OR the assigned solver lacks write permission.
//!    The dispatcher broadcasts to every configured Write solver.
//! 3. `Broadcast { pubkeys, via_fallback: true }` — zero Write
//!    solvers configured AND
//!    `[escalation].fallback_to_all_solvers = true`. Every
//!    configured solver receives the DM regardless of permission.
//! 4. `Unroutable` — zero Write solvers configured AND fallback is
//!    off. The dispatcher records an
//!    `escalation_dispatch_unroutable` audit event + ERROR log
//!    line and leaves the handoff unconsumed.
//!
//! The function is pure: no DB access, no logging, no IO. All
//! side effects live on the caller side so the rule is trivially
//! unit-testable.

use crate::models::{SolverConfig, SolverPermission};

/// Decision returned by [`resolve_recipients`]. Exactly one of
/// these four outcomes per handoff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recipients {
    /// The dispute's `assigned_solver` was set to a pubkey that is
    /// both configured locally AND carries write permission. Only
    /// that solver receives the DM.
    Targeted(String),
    /// Either no targeted assignment matched, or the assigned
    /// solver lacked write permission, or zero write solvers are
    /// configured but fallback is on. `via_fallback = true` marks
    /// the third case — the dispatcher uses it to set the
    /// `fallback_broadcast` column on the `escalation_dispatches`
    /// row and the matching audit-payload flag.
    Broadcast {
        pubkeys: Vec<String>,
        via_fallback: bool,
    },
    /// Zero write solvers configured AND `fallback_to_all_solvers
    /// = false`. The dispatcher records an unroutable audit event
    /// instead of sending.
    Unroutable,
}

/// Apply the FR-202 routing rules in order.
pub fn resolve_recipients(
    solvers: &[SolverConfig],
    assigned_solver: Option<&str>,
    fallback_to_all: bool,
) -> Recipients {
    // Rule 1: targeted write solver wins.
    if let Some(pk) = assigned_solver {
        if solvers
            .iter()
            .any(|s| s.pubkey == pk && s.permission == SolverPermission::Write)
        {
            return Recipients::Targeted(pk.to_string());
        }
    }

    // Rule 2: broadcast to every configured write solver.
    let write_pubkeys: Vec<String> = solvers
        .iter()
        .filter(|s| s.permission == SolverPermission::Write)
        .map(|s| s.pubkey.clone())
        .collect();
    if !write_pubkeys.is_empty() {
        return Recipients::Broadcast {
            pubkeys: write_pubkeys,
            via_fallback: false,
        };
    }

    // Rules 3/4: no write solvers configured.
    //
    // `fallback_to_all = true` only produces a real broadcast when
    // the operator actually configured at least one solver (of any
    // permission). "Fallback on + zero solvers" is structurally
    // identical to "no solver at all" — there is nothing to
    // broadcast to — and collapses to `Unroutable` so the
    // dispatcher runs exactly one code path for every "can't
    // route" shape. The alternative (returning `Broadcast { [],
    // via_fallback: true }`) would bypass the Unroutable audit
    // handler and re-log every cycle without ever marking the
    // handoff consumed.
    if fallback_to_all && !solvers.is_empty() {
        let all_pubkeys: Vec<String> = solvers.iter().map(|s| s.pubkey.clone()).collect();
        return Recipients::Broadcast {
            pubkeys: all_pubkeys,
            via_fallback: true,
        };
    }
    Recipients::Unroutable
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solver(pk: &str, perm: SolverPermission) -> SolverConfig {
        SolverConfig {
            pubkey: pk.to_string(),
            permission: perm,
        }
    }

    #[test]
    fn targeted_write_assigned_solver_wins() {
        let solvers = vec![
            solver("pk-w1", SolverPermission::Write),
            solver("pk-r1", SolverPermission::Read),
        ];
        let got = resolve_recipients(&solvers, Some("pk-w1"), false);
        assert_eq!(got, Recipients::Targeted("pk-w1".to_string()));
    }

    #[test]
    fn assigned_solver_not_in_configured_falls_back_to_write_broadcast() {
        let solvers = vec![
            solver("pk-w1", SolverPermission::Write),
            solver("pk-w2", SolverPermission::Write),
        ];
        let got = resolve_recipients(&solvers, Some("pk-unknown"), false);
        assert_eq!(
            got,
            Recipients::Broadcast {
                pubkeys: vec!["pk-w1".to_string(), "pk-w2".to_string()],
                via_fallback: false,
            }
        );
    }

    #[test]
    fn read_permission_assigned_is_ignored_broadcast_to_write_set() {
        // US1 acceptance scenario 4 — a Read-assigned solver MUST
        // NOT receive the handoff DM; the router broadcasts to the
        // Write set instead.
        let solvers = vec![
            solver("pk-r1", SolverPermission::Read),
            solver("pk-w1", SolverPermission::Write),
        ];
        let got = resolve_recipients(&solvers, Some("pk-r1"), false);
        assert_eq!(
            got,
            Recipients::Broadcast {
                pubkeys: vec!["pk-w1".to_string()],
                via_fallback: false,
            }
        );
    }

    #[test]
    fn no_assignment_broadcasts_to_write_set() {
        let solvers = vec![
            solver("pk-w1", SolverPermission::Write),
            solver("pk-w2", SolverPermission::Write),
            solver("pk-r1", SolverPermission::Read),
        ];
        let got = resolve_recipients(&solvers, None, false);
        assert_eq!(
            got,
            Recipients::Broadcast {
                pubkeys: vec!["pk-w1".to_string(), "pk-w2".to_string()],
                via_fallback: false,
            }
        );
    }

    #[test]
    fn zero_write_solvers_with_fallback_on_broadcasts_to_all() {
        let solvers = vec![
            solver("pk-r1", SolverPermission::Read),
            solver("pk-r2", SolverPermission::Read),
        ];
        let got = resolve_recipients(&solvers, None, true);
        assert_eq!(
            got,
            Recipients::Broadcast {
                pubkeys: vec!["pk-r1".to_string(), "pk-r2".to_string()],
                via_fallback: true,
            }
        );
    }

    #[test]
    fn zero_write_solvers_with_fallback_off_is_unroutable() {
        let solvers = vec![
            solver("pk-r1", SolverPermission::Read),
            solver("pk-r2", SolverPermission::Read),
        ];
        let got = resolve_recipients(&solvers, None, false);
        assert_eq!(got, Recipients::Unroutable);
    }

    #[test]
    fn zero_configured_solvers_with_fallback_off_is_unroutable() {
        let got = resolve_recipients(&[], None, false);
        assert_eq!(got, Recipients::Unroutable);
    }

    #[test]
    fn zero_configured_solvers_with_fallback_on_is_unroutable() {
        // Fallback-on with zero solvers is structurally identical
        // to "no one configured" — there is nothing to broadcast
        // to, so the router collapses it to `Unroutable` and the
        // dispatcher funnels it through the single can't-route
        // handler (ERROR log + future T023 audit-row writer).
        // The earlier implementation returned
        // `Broadcast { pubkeys: [], via_fallback: true }`, which
        // bypassed the Unroutable handler AND left the handoff
        // unconsumed: every cycle re-scanned and re-logged with
        // no operator-visible terminal state.
        let got = resolve_recipients(&[], None, true);
        assert_eq!(got, Recipients::Unroutable);
    }
}
