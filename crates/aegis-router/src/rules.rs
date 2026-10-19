//! Escalation and stop rules, Core → Expert → PR / stop (design §5.3).
//!
//! The budget row ("any tier, > $4 or > 20 calls") is checked before every
//! model call by [`crate::BudgetCheck`]; this module covers the other rows.

use std::fmt;

use aegis_ledger::{Outcome, Tier};

/// Thresholds of §5.3. `Default` is the design value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EscalationRules {
    /// Consecutive edits rejected by the linter before escalating.
    pub edit_rejections: u32,
    /// Consecutive patches after which the repro test still fails.
    pub repro_failures: u32,
    /// Files the fault spans (from Codebase-Memory) that escalate.
    pub spread_files: usize,
    /// Failed Expert attempts before stopping.
    pub expert_failures: u32,
}

impl Default for EscalationRules {
    fn default() -> Self {
        Self {
            edit_rejections: 2,
            repro_failures: 2,
            spread_files: 3,
            expert_failures: 2,
        }
    }
}

/// What the issue lifecycle observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    /// The linter rejected an `edit`.
    EditRejected,
    /// An `edit` went through; resets the rejection streak.
    EditAccepted,
    /// A patch was applied and the reproduction test was run.
    PatchTested {
        repro_passes: bool,
    },
    /// Codebase-Memory located the fault across this many files.
    FaultSpread {
        files: usize,
    },
    GatePassed,
    GateFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscalationReason {
    EditsRejected,
    ReproStillFails,
    FaultSpread { files: usize },
}

impl fmt::Display for EscalationReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EditsRejected => f.write_str("edits rejected by the linter twice in a row"),
            Self::ReproStillFails => f.write_str("repro test still fails after two patches"),
            Self::FaultSpread { files } => write!(f, "fault spans {files} files"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    ExpertFailed { attempts: u32 },
}

impl StopReason {
    pub fn outcome(self) -> Outcome {
        match self {
            Self::ExpertFailed { .. } => Outcome::StoppedExpertFail,
        }
    }
}

impl fmt::Display for StopReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ExpertFailed { attempts } => write!(f, "Expert failed {attempts} times"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Continue,
    /// Switch to Expert, keeping the whole history.
    Escalate(EscalationReason),
    OpenPr,
    /// Hand over to a human.
    Stop(StopReason),
}

/// Router state for one issue. Starts at Core (Light only classifies).
/// After [`Decision::OpenPr`] or [`Decision::Stop`] the lifecycle stops
/// feeding signals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Router {
    rules: EscalationRules,
    tier: Tier,
    edit_rejections: u32,
    repro_failures: u32,
    expert_failures: u32,
    /// The current Expert attempt already counted as failed.
    attempt_failed: bool,
}

impl Router {
    pub fn new(rules: EscalationRules) -> Self {
        Self {
            rules,
            tier: Tier::Core,
            edit_rejections: 0,
            repro_failures: 0,
            expert_failures: 0,
            attempt_failed: false,
        }
    }

    /// Core or Expert.
    pub fn tier(&self) -> Tier {
        self.tier
    }

    pub fn observe(self, signal: Signal) -> (Self, Decision) {
        match self.tier {
            Tier::Expert => self.observe_expert(signal),
            Tier::Light | Tier::Core => self.observe_core(signal),
        }
    }

    fn observe_core(self, signal: Signal) -> (Self, Decision) {
        let rules = self.rules;
        match signal {
            Signal::GatePassed => (self, Decision::OpenPr),
            Signal::GateFailed => (self, Decision::Continue),
            Signal::EditAccepted => (
                Self {
                    edit_rejections: 0,
                    ..self
                },
                Decision::Continue,
            ),
            Signal::EditRejected => {
                let edit_rejections = self.edit_rejections.saturating_add(1);
                let next = Self {
                    edit_rejections,
                    ..self
                };
                if edit_rejections >= rules.edit_rejections {
                    next.escalate(EscalationReason::EditsRejected)
                } else {
                    (next, Decision::Continue)
                }
            }
            Signal::PatchTested { repro_passes: true } => (
                Self {
                    repro_failures: 0,
                    ..self
                },
                Decision::Continue,
            ),
            Signal::PatchTested {
                repro_passes: false,
            } => {
                let repro_failures = self.repro_failures.saturating_add(1);
                let next = Self {
                    repro_failures,
                    ..self
                };
                if repro_failures >= rules.repro_failures {
                    next.escalate(EscalationReason::ReproStillFails)
                } else {
                    (next, Decision::Continue)
                }
            }
            Signal::FaultSpread { files } if files >= rules.spread_files => {
                self.escalate(EscalationReason::FaultSpread { files })
            }
            Signal::FaultSpread { .. } => (self, Decision::Continue),
        }
    }

    /// An Expert attempt is a patch, its repro test, then the gate. It fails
    /// when the repro test still fails or the gate fails, and counts at most
    /// once: a failing repro followed by a failing gate is one failure. A
    /// gate run ends the attempt.
    fn observe_expert(self, signal: Signal) -> (Self, Decision) {
        match signal {
            Signal::GatePassed => (self, Decision::OpenPr),
            Signal::PatchTested {
                repro_passes: false,
            } => self.expert_failure(true),
            Signal::PatchTested { repro_passes: true } => (
                Self {
                    attempt_failed: false,
                    ..self
                },
                Decision::Continue,
            ),
            Signal::GateFailed if self.attempt_failed => (
                Self {
                    attempt_failed: false,
                    ..self
                },
                Decision::Continue,
            ),
            Signal::GateFailed => self.expert_failure(false),
            Signal::EditRejected | Signal::EditAccepted | Signal::FaultSpread { .. } => {
                (self, Decision::Continue)
            }
        }
    }

    /// `attempt_open`: the attempt goes on to the gate, which must not count
    /// the same failure again.
    fn expert_failure(self, attempt_open: bool) -> (Self, Decision) {
        let expert_failures = self.expert_failures.saturating_add(1);
        let next = Self {
            expert_failures,
            attempt_failed: attempt_open,
            ..self
        };
        if expert_failures >= self.rules.expert_failures {
            let stop = StopReason::ExpertFailed {
                attempts: expert_failures,
            };
            (next, Decision::Stop(stop))
        } else {
            (next, Decision::Continue)
        }
    }

    fn escalate(self, reason: EscalationReason) -> (Self, Decision) {
        (
            Self {
                tier: Tier::Expert,
                ..self
            },
            Decision::Escalate(reason),
        )
    }
}

impl Default for Router {
    fn default() -> Self {
        Self::new(EscalationRules::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feeds signals in order and returns the final state and the decisions.
    fn run(router: Router, signals: &[Signal]) -> (Router, Vec<Decision>) {
        signals
            .iter()
            .fold((router, Vec::new()), |(router, mut decisions), &signal| {
                let (next, decision) = router.observe(signal);
                decisions.push(decision);
                (next, decisions)
            })
    }

    fn expert() -> Router {
        let (router, _) = run(Router::default(), &[Signal::FaultSpread { files: 5 }]);
        assert_eq!(router.tier(), Tier::Expert);
        router
    }

    #[test]
    fn starts_at_core() {
        assert_eq!(Router::default().tier(), Tier::Core);
    }

    // §5.3 row: Core, gate passes → PR.
    #[test]
    fn core_gate_pass_opens_pr() {
        let (_, d) = run(Router::default(), &[Signal::GateFailed, Signal::GatePassed]);
        assert_eq!(d, vec![Decision::Continue, Decision::OpenPr]);
    }

    // §5.3 row: Core, `edit` rejected twice in a row → Expert.
    #[test]
    fn core_two_consecutive_edit_rejections_escalate() {
        let (router, d) = run(
            Router::default(),
            &[Signal::EditRejected, Signal::EditRejected],
        );
        assert_eq!(d[1], Decision::Escalate(EscalationReason::EditsRejected));
        assert_eq!(router.tier(), Tier::Expert);
    }

    #[test]
    fn accepted_edit_breaks_the_rejection_streak() {
        let (router, d) = run(
            Router::default(),
            &[
                Signal::EditRejected,
                Signal::EditAccepted,
                Signal::EditRejected,
            ],
        );
        assert!(d.iter().all(|d| *d == Decision::Continue));
        assert_eq!(router.tier(), Tier::Core);
    }

    // §5.3 row: Core, repro test still fails after 2 consecutive patches → Expert.
    #[test]
    fn core_repro_failing_after_two_patches_escalates() {
        let fail = Signal::PatchTested {
            repro_passes: false,
        };
        let (router, d) = run(Router::default(), &[fail, fail]);
        assert_eq!(
            d,
            vec![
                Decision::Continue,
                Decision::Escalate(EscalationReason::ReproStillFails)
            ]
        );
        assert_eq!(router.tier(), Tier::Expert);
    }

    #[test]
    fn passing_repro_resets_the_patch_streak() {
        let fail = Signal::PatchTested {
            repro_passes: false,
        };
        let pass = Signal::PatchTested { repro_passes: true };
        let (router, _) = run(Router::default(), &[fail, pass, fail, Signal::GateFailed]);
        assert_eq!(router.tier(), Tier::Core);
    }

    // §5.3 row: Core, fault spans ≥ 3 files → Expert.
    #[test]
    fn core_fault_spread_over_three_files_escalates() {
        let (router, d) = run(Router::default(), &[Signal::FaultSpread { files: 2 }]);
        assert_eq!((router.tier(), d[0]), (Tier::Core, Decision::Continue));

        let (router, d) = run(Router::default(), &[Signal::FaultSpread { files: 3 }]);
        assert_eq!(
            d[0],
            Decision::Escalate(EscalationReason::FaultSpread { files: 3 })
        );
        assert_eq!(router.tier(), Tier::Expert);
    }

    // §5.3 row: Expert, gate passes → PR.
    #[test]
    fn expert_gate_pass_opens_pr() {
        let (_, d) = run(expert(), &[Signal::GateFailed, Signal::GatePassed]);
        assert_eq!(d, vec![Decision::Continue, Decision::OpenPr]);
    }

    // §5.3 row: Expert fails twice → stop.
    #[test]
    fn expert_two_failed_attempts_stop() {
        let repro_fails = Signal::PatchTested {
            repro_passes: false,
        };
        let repro_passes = Signal::PatchTested { repro_passes: true };
        let (_, d) = run(
            expert(),
            &[
                // Attempt 1: repro still fails, and the gate run after it fails too.
                repro_fails,
                Signal::GateFailed,
                // Attempt 2: repro passes, gate fails.
                repro_passes,
                Signal::GateFailed,
            ],
        );
        let stop = StopReason::ExpertFailed { attempts: 2 };
        assert_eq!(
            d,
            vec![
                Decision::Continue,
                Decision::Continue,
                Decision::Continue,
                Decision::Stop(stop)
            ]
        );
        assert_eq!(stop.outcome(), Outcome::StoppedExpertFail);
    }

    #[test]
    fn expert_failures_count_per_attempt() {
        let repro_fails = Signal::PatchTested {
            repro_passes: false,
        };
        let stop = Decision::Stop(StopReason::ExpertFailed { attempts: 2 });
        // Two gate runs, no repro signal: two attempts.
        let (_, d) = run(expert(), &[Signal::GateFailed, Signal::GateFailed]);
        assert_eq!(d[1], stop);
        // Two patches whose repro fails, gate never run: two attempts.
        let (_, d) = run(expert(), &[repro_fails, repro_fails]);
        assert_eq!(d[1], stop);
    }

    #[test]
    fn expert_does_not_escalate_again() {
        let (router, d) = run(
            expert(),
            &[
                Signal::EditRejected,
                Signal::EditRejected,
                Signal::FaultSpread { files: 9 },
            ],
        );
        assert!(d.iter().all(|d| *d == Decision::Continue));
        assert_eq!(router.tier(), Tier::Expert);
    }

    #[test]
    fn thresholds_come_from_rules() {
        let strict = EscalationRules {
            edit_rejections: 1,
            ..EscalationRules::default()
        };
        let (_, d) = run(Router::new(strict), &[Signal::EditRejected]);
        assert_eq!(d[0], Decision::Escalate(EscalationReason::EditsRejected));
    }

    #[test]
    fn reasons_read_well() {
        assert_eq!(
            EscalationReason::FaultSpread { files: 4 }.to_string(),
            "fault spans 4 files"
        );
        assert_eq!(
            StopReason::ExpertFailed { attempts: 2 }.to_string(),
            "Expert failed 2 times"
        );
    }
}
