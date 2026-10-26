//! Escalation and stop rules, Core → Expert → PR / stop (design §5.3).
//!
//! The budget row ("any tier, > $4 or > 40 calls") is checked before every
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
    /// Core/Expert calls the job may use before Core's first patch; Core
    /// still reading code past this escalates (#127).
    pub calls_without_patch: u32,
    /// Failed Expert attempts before stopping.
    pub expert_failures: u32,
    /// Core/Expert calls between reminders to write and run the
    /// reproduction test, while `run_tests` has not been called (#144);
    /// once it has, calls since the last test run before one reminder to
    /// run it again (#156).
    pub calls_per_test_reminder: u32,
    /// Reminders before the first test run a job gets at most.
    pub test_reminders: u32,
    /// Reminders to run the test again a job gets at most (#156).
    pub retest_reminders: u32,
}

impl Default for EscalationRules {
    fn default() -> Self {
        Self {
            edit_rejections: 2,
            repro_failures: 2,
            spread_files: 3,
            calls_without_patch: 15,
            expert_failures: 2,
            calls_per_test_reminder: 8,
            test_reminders: 2,
            retest_reminders: 2,
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
    /// The job has used this many Core/Expert calls so far.
    ModelCalls {
        calls: u32,
    },
    /// The agent called `run_tests` in the job's `calls`-th Core/Expert
    /// call.
    TestRun {
        calls: u32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscalationReason {
    EditsRejected,
    ReproStillFails,
    FaultSpread { files: usize },
    NoPatch { calls: u32 },
}

impl fmt::Display for EscalationReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EditsRejected => f.write_str("edits rejected by the linter twice in a row"),
            Self::ReproStillFails => f.write_str("repro test still fails after two patches"),
            Self::FaultSpread { files } => write!(f, "fault spans {files} files"),
            Self::NoPatch { calls } => write!(f, "no patch after {calls} calls"),
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
    /// Tell the agent to write its reproduction test and run it with
    /// `run_tests` before reading more code (#144), or, when it last ran
    /// one in call `last_test`, to run it again (#156); the tier stays.
    RemindTest {
        calls: u32,
        last_test: Option<u32>,
    },
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
    /// A patch reached the repro test or the gate.
    patched: bool,
    /// The call in which the agent last ran `run_tests`.
    last_test: Option<u32>,
    /// Reminders before the first test run given so far.
    reminders: u32,
    /// Reminders to run the test again given so far.
    retests: u32,
    /// A reminder to run the test again came since `last_test`.
    retest_reminded: bool,
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
            patched: false,
            last_test: None,
            reminders: 0,
            retests: 0,
            retest_reminded: false,
        }
    }

    /// Core or Expert.
    pub fn tier(&self) -> Tier {
        self.tier
    }

    pub fn observe(self, signal: Signal) -> (Self, Decision) {
        let patched = self.patched
            || matches!(
                signal,
                Signal::PatchTested { .. } | Signal::GatePassed | Signal::GateFailed
            );
        let this = match signal {
            Signal::TestRun { calls } => Self {
                patched,
                last_test: Some(calls),
                retest_reminded: false,
                ..self
            },
            _ => Self { patched, ..self },
        };
        match this.tier {
            Tier::Expert => this.observe_expert(signal),
            Tier::Light | Tier::Core => this.observe_core(signal),
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
            Signal::ModelCalls { calls } if !self.patched && calls >= rules.calls_without_patch => {
                self.escalate(EscalationReason::NoPatch { calls })
            }
            Signal::ModelCalls { calls } => self.remind_test(calls),
            Signal::TestRun { .. } => (self, Decision::Continue),
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
            Signal::ModelCalls { calls } => self.remind_test(calls),
            Signal::EditRejected
            | Signal::EditAccepted
            | Signal::FaultSpread { .. }
            | Signal::TestRun { .. } => (self, Decision::Continue),
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

    /// A reminder every `calls_per_test_reminder` calls until the agent
    /// runs a test, at most `test_reminders` of them. After that, one
    /// reminder `calls_per_test_reminder` calls after the last test run,
    /// until the next run, at most `retest_reminders` of them: in #145 an
    /// agent ran its test once and read code to the end of its calls.
    fn remind_test(self, calls: u32) -> (Self, Decision) {
        let every = self.rules.calls_per_test_reminder;
        let (due, left) = match self.last_test {
            None => (
                every.saturating_mul(self.reminders.saturating_add(1)),
                self.reminders < self.rules.test_reminders,
            ),
            Some(at) => (
                at.saturating_add(every),
                !self.retest_reminded && self.retests < self.rules.retest_reminders,
            ),
        };
        if !left || calls < due {
            return (self, Decision::Continue);
        }
        let next = match self.last_test {
            None => Self {
                reminders: self.reminders + 1,
                ..self
            },
            Some(_) => Self {
                retests: self.retests + 1,
                retest_reminded: true,
                ..self
            },
        };
        (
            next,
            Decision::RemindTest {
                calls,
                last_test: self.last_test,
            },
        )
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

    // §5.3 row: Core, 15 calls without a patch → Expert (#127).
    #[test]
    fn core_without_a_patch_after_fifteen_calls_escalates() {
        // A test run keeps the reminder of #144 out of the way.
        let (router, d) = run(Router::default(), &[tested(14), calls(14)]);
        assert_eq!((router.tier(), d[1]), (Tier::Core, Decision::Continue));

        let (router, d) = run(Router::default(), &[tested(15), calls(15)]);
        assert_eq!(
            d[1],
            Decision::Escalate(EscalationReason::NoPatch { calls: 15 })
        );
        assert_eq!(router.tier(), Tier::Expert);
    }

    #[test]
    fn a_tested_patch_turns_the_call_rule_off() {
        for first in [
            Signal::GateFailed,
            Signal::PatchTested { repro_passes: true },
        ] {
            let (router, d) = run(Router::default(), &[tested(30), first, calls(30)]);
            assert_eq!((router.tier(), d[2]), (Tier::Core, Decision::Continue));
        }
    }

    #[test]
    fn expert_ignores_the_call_rule() {
        let (router, d) = run(expert(), &[tested(30), calls(30)]);
        assert_eq!((router.tier(), d[1]), (Tier::Expert, Decision::Continue));
    }

    fn calls(calls: u32) -> Signal {
        Signal::ModelCalls { calls }
    }

    fn tested(calls: u32) -> Signal {
        Signal::TestRun { calls }
    }

    fn reminded(calls: u32) -> Decision {
        Decision::RemindTest {
            calls,
            last_test: None,
        }
    }

    fn reminded_again(calls: u32, last_test: u32) -> Decision {
        Decision::RemindTest {
            calls,
            last_test: Some(last_test),
        }
    }

    // §5.3 row: any tier, 8 calls without `run_tests` → reminder, at most
    // twice (#144).
    #[test]
    fn eight_calls_without_a_test_run_bring_a_reminder() {
        let (router, d) = run(Router::default(), &[calls(7), calls(8), calls(9)]);
        assert_eq!(
            d,
            vec![Decision::Continue, reminded(8), Decision::Continue,]
        );
        assert_eq!(router.tier(), Tier::Core);
    }

    #[test]
    fn a_second_reminder_comes_eight_calls_later_and_no_third() {
        // A failed gate run (a patch, no test run) keeps the escalation
        // rule of #127 out of the way.
        let (_, d) = run(
            Router::default(),
            &[
                Signal::GateFailed,
                calls(8),
                calls(14),
                calls(16),
                calls(24),
                calls(32),
            ],
        );
        assert_eq!(
            d[1..],
            [
                reminded(8),
                Decision::Continue,
                reminded(16),
                Decision::Continue,
                Decision::Continue,
            ]
        );
    }

    #[test]
    fn a_test_run_turns_the_reminder_off() {
        let (_, d) = run(
            Router::default(),
            &[tested(5), Signal::GateFailed, calls(8), calls(12)],
        );
        assert!(d.iter().all(|d| *d == Decision::Continue), "{d:?}");
    }

    // #156: in #145 an agent ran its test once in call 11 and read code to
    // the end of its 40 calls.
    #[test]
    fn eight_calls_after_the_last_test_run_bring_one_reminder_to_run_it_again() {
        let (_, d) = run(
            Router::default(),
            &[
                Signal::GateFailed,
                tested(11),
                calls(18),
                calls(19),
                calls(27),
                calls(40),
            ],
        );
        assert_eq!(
            d[2..],
            [
                Decision::Continue,
                reminded_again(19, 11),
                Decision::Continue,
                Decision::Continue,
            ]
        );
    }

    #[test]
    fn a_new_test_run_restarts_the_count_and_the_reminders_stop_after_two() {
        let (_, d) = run(
            Router::default(),
            &[
                Signal::GateFailed,
                tested(3),
                calls(11),
                tested(12),
                calls(19),
                calls(20),
                tested(21),
                calls(29),
                calls(40),
            ],
        );
        assert_eq!(
            d[1..],
            [
                Decision::Continue,
                reminded_again(11, 3),
                Decision::Continue,
                Decision::Continue,
                reminded_again(20, 12),
                Decision::Continue,
                Decision::Continue,
                Decision::Continue,
            ]
        );
    }

    #[test]
    fn reminders_before_the_first_test_run_leave_the_reminders_to_run_it_again() {
        let (_, d) = run(
            Router::default(),
            &[
                Signal::GateFailed,
                calls(8),
                calls(16),
                tested(17),
                calls(25),
            ],
        );
        assert_eq!(
            d[1..],
            [
                reminded(8),
                reminded(16),
                Decision::Continue,
                reminded_again(25, 17),
            ]
        );
    }

    #[test]
    fn escalating_without_a_patch_comes_before_a_reminder() {
        // Reminded at 8, the 15th call without a patch escalates; the
        // second reminder then reaches Expert at 16.
        let (router, d) = run(Router::default(), &[calls(8), calls(15), calls(16)]);
        assert_eq!(
            d,
            vec![
                reminded(8),
                Decision::Escalate(EscalationReason::NoPatch { calls: 15 }),
                reminded(16),
            ]
        );
        assert_eq!(router.tier(), Tier::Expert);
    }

    #[test]
    fn expert_is_reminded_too() {
        let (router, d) = run(expert(), &[calls(8)]);
        assert_eq!(d[0], reminded(8));
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
