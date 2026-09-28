//! Whether Serbero may mediate (`docs/spec.md` §7.1). Every check is
//! deterministic and runs before the take; no judge call happens here.

use crate::store::disputes::Lifecycle;

/// What eligibility reads about a dispute and Serbero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Candidate {
    /// `[mediation].enabled`.
    pub enabled: bool,
    /// The judge passed its startup checks (`Readiness::Ready`).
    pub ready: bool,
    pub lifecycle: Lifecycle,
    /// The dispute ever had a session: mediation opens at most once, so a
    /// dispute that was handed off is never taken again.
    pub had_session: bool,
    /// A take of this dispute by Serbero is already in flight.
    pub taking: bool,
}

/// Why a dispute is not mediated; logged, never shown to anyone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ineligible {
    Disabled,
    JudgeNotReady,
    /// Not `notified`: every first DM failed, a human took it, or it is
    /// resolved.
    NotNotified(Lifecycle),
    HadSession,
    AlreadyTaking,
}

pub fn check(candidate: &Candidate) -> Result<(), Ineligible> {
    if !candidate.enabled {
        return Err(Ineligible::Disabled);
    }
    if !candidate.ready {
        return Err(Ineligible::JudgeNotReady);
    }
    if candidate.lifecycle != Lifecycle::Notified {
        return Err(Ineligible::NotNotified(candidate.lifecycle));
    }
    if candidate.had_session {
        return Err(Ineligible::HadSession);
    }
    if candidate.taking {
        return Err(Ineligible::AlreadyTaking);
    }
    Ok(())
}

/// The judge's startup state (`docs/spec.md` §7.1 and plan T5.7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Readiness {
    Ready,
    /// Mediation stays off; notification is unaffected.
    Off(String),
}

/// What the startup checks found about the judge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JudgeChecks<'a> {
    /// Thresholds exist for the configured provider and model.
    pub has_thresholds: bool,
    /// Problems `Capabilities::check` found with the question sets.
    pub capability_problems: &'a [String],
    /// The health check's error, if it failed.
    pub health_error: Option<&'a str>,
}

/// Mediation runs only with calibrated thresholds, question sets the
/// provider can express, and a healthy judge.
pub fn readiness(checks: &JudgeChecks<'_>) -> Readiness {
    if !checks.has_thresholds {
        return Readiness::Off("no thresholds for the configured judge".into());
    }
    if !checks.capability_problems.is_empty() {
        return Readiness::Off(format!(
            "the judge cannot express the question set: {}",
            checks.capability_problems.join("; ")
        ));
    }
    if let Some(error) = checks.health_error {
        return Readiness::Off(format!("judge health check failed: {error}"));
    }
    Readiness::Ready
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eligible() -> Candidate {
        Candidate {
            enabled: true,
            ready: true,
            lifecycle: Lifecycle::Notified,
            had_session: false,
            taking: false,
        }
    }

    #[test]
    fn a_notified_dispute_with_everything_ready_is_eligible() {
        assert_eq!(check(&eligible()), Ok(()));
    }

    #[test]
    fn each_condition_makes_a_dispute_ineligible() {
        let cases = [
            (
                Candidate {
                    enabled: false,
                    ..eligible()
                },
                Ineligible::Disabled,
            ),
            (
                Candidate {
                    ready: false,
                    ..eligible()
                },
                Ineligible::JudgeNotReady,
            ),
            (
                Candidate {
                    lifecycle: Lifecycle::New,
                    ..eligible()
                },
                Ineligible::NotNotified(Lifecycle::New),
            ),
            (
                Candidate {
                    lifecycle: Lifecycle::Taken,
                    ..eligible()
                },
                Ineligible::NotNotified(Lifecycle::Taken),
            ),
            (
                Candidate {
                    lifecycle: Lifecycle::Resolved,
                    ..eligible()
                },
                Ineligible::NotNotified(Lifecycle::Resolved),
            ),
            (
                Candidate {
                    had_session: true,
                    ..eligible()
                },
                Ineligible::HadSession,
            ),
            (
                Candidate {
                    taking: true,
                    ..eligible()
                },
                Ineligible::AlreadyTaking,
            ),
        ];

        for (candidate, reason) in cases {
            assert_eq!(check(&candidate), Err(reason), "{candidate:?}");
        }
    }

    #[test]
    fn the_judge_is_ready_only_when_every_check_passes() {
        let problems = ["qs: 300 options".to_owned()];
        let ok = JudgeChecks {
            has_thresholds: true,
            capability_problems: &[],
            health_error: None,
        };

        assert_eq!(readiness(&ok), Readiness::Ready);
        assert!(matches!(
            readiness(&JudgeChecks { has_thresholds: false, ..ok }),
            Readiness::Off(r) if r.contains("no thresholds")
        ));
        assert!(matches!(
            readiness(&JudgeChecks { capability_problems: &problems, ..ok }),
            Readiness::Off(r) if r.contains("300 options")
        ));
        assert!(matches!(
            readiness(&JudgeChecks { health_error: Some("401"), ..ok }),
            Readiness::Off(r) if r.contains("health check failed: 401")
        ));
    }
}
