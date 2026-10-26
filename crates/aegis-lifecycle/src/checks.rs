//! What may end a model loop before its next call (ADR 0003, design
//! §5.3–§5.4): a cancel from outside, the job's budget, edits rejected
//! often enough, or Core reading on without a patch, to escalate, or calls
//! made without a test run, to remind the agent (#144, #156). The loop only sees
//! a reason string; the lifecycle reads the structured [`Halt`] back.

use std::sync::Mutex;

use aegis_ledger::Tier;
use aegis_orchestrator::{StopCheck, TurnRecord};
use aegis_router::{BudgetCheck, BudgetStop, Decision, EscalationReason, Router, Signal};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, PartialEq)]
pub enum Halt {
    Cancelled,
    Budget(BudgetStop),
    Escalate(EscalationReason),
    /// Remind the agent to write and run its reproduction test, or to run
    /// it again after its test run in call `last_test`, then go on at the
    /// same tier.
    RemindTest {
        calls: u32,
        last_test: Option<u32>,
    },
}

/// What the agent reads when reminded (#144, #156).
pub fn test_reminder(calls: u32, last_test: Option<u32>) -> String {
    match last_test {
        None => format!(
            "You have made {calls} calls without running a test. Stop reading code now: \
             write the reproduction test for the issue in a test file and run it with \
             `run_tests`. It must fail for the reason the issue describes. Then use its \
             output to find and fix the code."
        ),
        Some(last) => format!(
            "You last ran a test {} calls ago. Stop reading code now: make the change you \
             think fixes the issue and run the reproduction test again with `run_tests`. \
             Let its output tell you what is still wrong.",
            calls.saturating_sub(last)
        ),
    }
}

/// The router after the edits and test runs of `turns`, and the first
/// decision that is not `Continue` (an escalation), if any. `prior_calls`:
/// the job's Core/Expert calls before `turns`, to number each test run.
pub fn fold_tools(router: Router, prior_calls: u32, turns: &[TurnRecord]) -> (Router, Decision) {
    let mut router = router;
    let numbered = (1..).map(|n: u32| prior_calls.saturating_add(n));
    for (calls, call) in numbered
        .zip(turns)
        .flat_map(|(calls, t)| t.tool_calls.iter().map(move |c| (calls, c)))
    {
        let signal = match (call.name.as_str(), call.is_error) {
            ("edit", true) => Signal::EditRejected,
            ("edit", false) => Signal::EditAccepted,
            ("run_tests", _) => Signal::TestRun { calls },
            _ => continue,
        };
        let (next, decision) = router.observe(signal);
        router = next;
        if decision != Decision::Continue {
            return (router, decision);
        }
    }
    (router, Decision::Continue)
}

#[derive(Debug)]
pub struct JobCheck {
    pub budget: BudgetCheck,
    /// The router as the loop started.
    pub router: Router,
    pub cancel: CancellationToken,
    halt: Mutex<Option<Halt>>,
}

impl JobCheck {
    pub fn new(budget: BudgetCheck, router: Router, cancel: CancellationToken) -> Self {
        Self {
            budget,
            router,
            cancel,
            halt: Mutex::new(None),
        }
    }

    /// Why the loop was stopped, after `run_loop` returned `Stopped`.
    pub fn halt(&self) -> Option<Halt> {
        self.halt.lock().ok().and_then(|h| h.clone())
    }

    fn decide(&self, turns: &[TurnRecord]) -> Option<Halt> {
        if self.cancel.is_cancelled() {
            return Some(Halt::Cancelled);
        }
        if let Some(stop) = self.budget.evaluate(turns) {
            return Some(Halt::Budget(stop));
        }
        let prior = self.budget.model_calls(&[]);
        let (router, decision) = fold_tools(self.router, prior, turns);
        if let Decision::Escalate(reason) = decision
            && self.router.tier() == Tier::Core
        {
            return Some(Halt::Escalate(reason));
        }
        let calls = self.budget.model_calls(turns);
        match router.observe(Signal::ModelCalls { calls }).1 {
            Decision::Escalate(reason) => Some(Halt::Escalate(reason)),
            Decision::RemindTest { calls, last_test } => {
                Some(Halt::RemindTest { calls, last_test })
            }
            Decision::Continue | Decision::OpenPr | Decision::Stop(_) => None,
        }
    }
}

impl StopCheck for JobCheck {
    fn check(&self, turns: &[TurnRecord]) -> Option<String> {
        let halt = self.decide(turns)?;
        let text = match &halt {
            Halt::Cancelled => "cancelled".to_owned(),
            Halt::Budget(stop) => stop.to_string(),
            Halt::Escalate(reason) => format!("escalating: {reason}"),
            Halt::RemindTest { calls, .. } => {
                format!("reminding to run a test after {calls} calls")
            }
        };
        if let Ok(mut slot) = self.halt.lock() {
            *slot = Some(halt);
        }
        Some(text)
    }
}
