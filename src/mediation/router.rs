//! Solver-facing routing (US3 / T058).
//!
//! One function, one rule (spec.md §Solver-Facing Routing): when the
//! underlying `disputes.assigned_solver` is known and matches a
//! configured solver, route the notification to that pubkey;
//! otherwise broadcast to every configured solver. This is the only
//! module that reifies the rule — every Phase 3 notification path
//! (summary delivery today, escalation handoff tomorrow) MUST go
//! through [`resolve_recipients`] so the operator-facing contract
//! has a single source of truth.
//!
//! FR-120 note: the returned [`Recipients`] carries pubkeys only —
//! no rationale or summary text is embedded, so passing the value
//! through logs / spans is safe.

use tracing::warn;

use crate::models::SolverConfig;

/// The routing decision for a summary or escalation notification.
///
/// Cloneable so `run_engine`-level call sites can fan out without
/// lifetime plumbing. Small enum, pubkeys are short hex strings, so
/// the clone cost is negligible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recipients {
    /// Route only to the named solver pubkey (hex).
    Targeted(String),
    /// Broadcast to every configured solver pubkey (hex).
    Broadcast(Vec<String>),
}

/// Resolve the notification recipients for a mediation event.
///
/// Rules (spec.md §Solver-Facing Routing):
///
/// 1. `assigned_solver = Some(pk)` AND `pk` is present in `solvers`
///    → [`Recipients::Targeted(pk)`].
/// 2. `assigned_solver = Some(pk)` but `pk` is NOT in `solvers` →
///    emit exactly one `warn!` naming the unknown pk, then fall
///    back to [`Recipients::Broadcast(all configured pubkeys)`].
///    This keeps the operator informed of the drift (a dispute was
///    taken by someone we are not configured to DM) without losing
///    the notification.
/// 3. `assigned_solver = None` → [`Recipients::Broadcast(all)`].
/// 4. `solvers` is empty → [`Recipients::Broadcast(vec![])`]. The
///    caller sees an empty recipient list; it is the caller's job
///    to log "no solver delivery possible" — the router must not
///    silently fail, and it must not hide the empty-config case
///    inside an `Option::None`.
pub fn resolve_recipients(solvers: &[SolverConfig], assigned_solver: Option<&str>) -> Recipients {
    let all: Vec<String> = solvers.iter().map(|s| s.pubkey.clone()).collect();

    match assigned_solver {
        Some(pk) if solvers.iter().any(|s| s.pubkey == pk) => Recipients::Targeted(pk.to_string()),
        Some(pk) => {
            // Rule 2: assigned but unconfigured. Fall back to broadcast
            // with a single warn — silence here would hide a real
            // config drift from the operator.
            warn!(
                assigned_solver = %pk,
                configured_count = solvers.len(),
                "assigned_solver is not in configured solvers list; falling back to broadcast"
            );
            Recipients::Broadcast(all)
        }
        None => Recipients::Broadcast(all),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::SolverPermission;

    fn solver(pk: &str) -> SolverConfig {
        SolverConfig {
            pubkey: pk.into(),
            permission: SolverPermission::Read,
        }
    }

    #[test]
    fn targeted_when_assigned_solver_is_configured() {
        let solvers = [solver("pk-a"), solver("pk-b")];
        let out = resolve_recipients(&solvers, Some("pk-a"));
        assert_eq!(out, Recipients::Targeted("pk-a".into()));
    }

    #[test]
    fn falls_back_to_broadcast_when_assigned_solver_unknown() {
        let solvers = [solver("pk-a"), solver("pk-b")];
        let out = resolve_recipients(&solvers, Some("pk-unknown"));
        assert_eq!(
            out,
            Recipients::Broadcast(vec!["pk-a".into(), "pk-b".into()])
        );
    }

    #[test]
    fn broadcast_when_no_assigned_solver() {
        let solvers = [solver("pk-a"), solver("pk-b")];
        let out = resolve_recipients(&solvers, None);
        assert_eq!(
            out,
            Recipients::Broadcast(vec!["pk-a".into(), "pk-b".into()])
        );
    }

    #[test]
    fn empty_solvers_with_no_assignment_returns_empty_broadcast() {
        let out = resolve_recipients(&[], None);
        assert_eq!(out, Recipients::Broadcast(vec![]));
    }

    #[test]
    fn empty_solvers_with_assignment_falls_back_to_empty_broadcast() {
        // Rule 2 + rule 4: assigned_solver is set but there are no
        // configured solvers at all, so the fallback broadcast is
        // an empty list. The caller has to log "no delivery possible"
        // — we must not lie about the unknown assigned pk.
        let out = resolve_recipients(&[], Some("pk-a"));
        assert_eq!(out, Recipients::Broadcast(vec![]));
    }
}
