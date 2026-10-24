//! What may end a model loop before its next call (ADR 0003, design
//! §5.3–§5.4): a cancel from outside, the job's budget, edits rejected
//! often enough, or Core reading on without a patch, to escalate, or calls
//! made without a test run, to remind the agent (#144). The loop only sees
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
    /// Remind the agent to write and run its reproduction test, then go on
    /// at the same tier.
    RemindTest {
        calls: u32,
    },
}

/// What the agent reads when reminded (#144).
pub fn test_reminder(calls: u32) -> String {
    format!(
        "You have made {calls} calls without running a test. Stop reading code now: write \
         the reproduction test for the issue in a test file and run it with `run_tests`. \
         It must fail for the reason the issue describes. Then use its output to find \
         and fix the code."
    )
}

/// The router after the edits and test runs of `turns`, and the first
/// decision that is not `Continue` (an escalation), if any.
pub fn fold_tools(router: Router, turns: &[TurnRecord]) -> (Router, Decision) {
    let mut router = router;
    for call in turns.iter().flat_map(|t| &t.tool_calls) {
        let signal = match (call.name.as_str(), call.is_error) {
            ("edit", true) => Signal::EditRejected,
            ("edit", false) => Signal::EditAccepted,
            ("run_tests", _) => Signal::TestRun,
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
        let (router, decision) = fold_tools(self.router, turns);
        if let Decision::Escalate(reason) = decision
            && self.router.tier() == Tier::Core
        {
            return Some(Halt::Escalate(reason));
        }
        let calls = self.budget.model_calls(turns);
        match router.observe(Signal::ModelCalls { calls }).1 {
            Decision::Escalate(reason) => Some(Halt::Escalate(reason)),
            Decision::RemindTest { calls } => Some(Halt::RemindTest { calls }),
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
            Halt::RemindTest { calls } => format!("reminding to run a test after {calls} calls"),
        };
        if let Ok(mut slot) = self.halt.lock() {
            *slot = Some(halt);
        }
        Some(text)
    }
}
