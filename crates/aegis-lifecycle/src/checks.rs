//! What may end a model loop before its next call (ADR 0003, design
//! §5.3–§5.4): a cancel from outside, the job's budget, or edits rejected
//! often enough, or Core reading on without a patch, to escalate. The loop only sees a reason string; the
//! lifecycle reads the structured [`Halt`] back.

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
}

/// The router after the edits of `turns`, and the first decision that is
/// not `Continue` (an escalation), if any.
pub fn fold_edits(router: Router, turns: &[TurnRecord]) -> (Router, Decision) {
    let mut router = router;
    for call in turns
        .iter()
        .flat_map(|t| &t.tool_calls)
        .filter(|c| c.name == "edit")
    {
        let signal = if call.is_error {
            Signal::EditRejected
        } else {
            Signal::EditAccepted
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
        if self.router.tier() == Tier::Core {
            let (router, decision) = fold_edits(self.router, turns);
            if let Decision::Escalate(reason) = decision {
                return Some(Halt::Escalate(reason));
            }
            let calls = self.budget.model_calls(turns);
            if let (_, Decision::Escalate(reason)) = router.observe(Signal::ModelCalls { calls }) {
                return Some(Halt::Escalate(reason));
            }
        }
        None
    }
}

impl StopCheck for JobCheck {
    fn check(&self, turns: &[TurnRecord]) -> Option<String> {
        let halt = self.decide(turns)?;
        let text = match &halt {
            Halt::Cancelled => "cancelled".to_owned(),
            Halt::Budget(stop) => stop.to_string(),
            Halt::Escalate(reason) => format!("escalating: {reason}"),
        };
        if let Ok(mut slot) = self.halt.lock() {
            *slot = Some(halt);
        }
        Some(text)
    }
}
