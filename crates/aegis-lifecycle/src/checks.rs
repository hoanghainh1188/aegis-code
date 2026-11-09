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
    /// Query Codebase-Memory graph impact after editing a source file (#267).
    CheckImpact,
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
    impact_checks: usize,
    halt: Mutex<Option<Halt>>,
}

impl JobCheck {
    pub fn new(budget: BudgetCheck, router: Router, cancel: CancellationToken) -> Self {
        Self {
            budget,
            router,
            cancel,
            impact_checks: 0,
            halt: Mutex::new(None),
        }
    }

    pub fn with_impact_checks(mut self, impact_checks: usize) -> Self {
        self.impact_checks = impact_checks;
        self
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
            Decision::Escalate(reason) => return Some(Halt::Escalate(reason)),
            Decision::RemindTest { calls, last_test } => {
                return Some(Halt::RemindTest { calls, last_test });
            }
            Decision::Continue | Decision::OpenPr | Decision::Stop(_) => {}
        }
        if self.impact_checks < 2 && has_source_edit(turns) {
            return Some(Halt::CheckImpact);
        }
        None
    }
}

fn has_source_edit(turns: &[TurnRecord]) -> bool {
    turns.iter().any(|t| {
        t.tool_calls.iter().any(|c| {
            c.name == "edit"
                && !c.is_error
                && c.target
                    .as_deref()
                    .and_then(|t| t.split_whitespace().next())
                    .and_then(|p| p.split(':').next())
                    .is_some_and(|path| !aegis_lang::is_test_path(path))
        })
    })
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
            Halt::CheckImpact => "checking code graph impact".to_owned(),
        };
        if let Ok(mut slot) = self.halt.lock() {
            *slot = Some(halt);
        }
        Some(text)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use aegis_orchestrator::ToolCallRecord;
    use aegis_provider::Usage;
    use aegis_router::{BudgetLimits, Spent};

    use super::*;

    fn turn_with_tool(name: &str, target: Option<&str>, is_error: bool) -> TurnRecord {
        TurnRecord {
            model: "core".into(),
            usage: Usage::default(),
            stop_reason: None,
            attempts: 1,
            tool_calls: vec![ToolCallRecord {
                name: name.into(),
                target: target.map(str::to_owned),
                is_error,
            }],
        }
    }

    const TEST_PRICES: &str = r#"
regional_surcharge = 0.1
[cache_write_multiplier]
5m = 1.0
1h = 1.0
[models.core]
input = 0.1
output = 0.5
cache_read_multiplier = 0.1
"#;

    fn check(impact_checks: usize) -> JobCheck {
        let budget = BudgetCheck::new(
            Arc::new(aegis_ledger::PriceTable::from_toml(TEST_PRICES).unwrap()),
            false,
            BudgetLimits {
                max_model_calls: 50,
                max_cost_usd: 100.0,
            },
            Tier::Core,
            Spent::default(),
        );
        let router = Router::new(Default::default());
        JobCheck::new(budget, router, CancellationToken::new()).with_impact_checks(impact_checks)
    }

    #[test]
    fn source_file_edit_triggers_check_impact() {
        let c = check(0);
        let turns = vec![turn_with_tool("edit", Some("src/lib.rs:10-20"), false)];
        assert_eq!(c.decide(&turns), Some(Halt::CheckImpact));
    }

    #[test]
    fn test_file_edit_does_not_trigger_check_impact() {
        let c = check(0);
        let turns = vec![turn_with_tool("edit", Some("tests/test_cart.rs:10"), false)];
        assert_eq!(c.decide(&turns), None);
    }

    #[test]
    fn failed_edit_does_not_trigger_check_impact() {
        let c = check(0);
        let turns = vec![turn_with_tool("edit", Some("src/lib.rs:10"), true)];
        assert_eq!(c.decide(&turns), None);
    }

    #[test]
    fn two_impact_checks_limit_prevents_further_checks() {
        let c = check(2);
        let turns = vec![turn_with_tool("edit", Some("src/lib.rs:10"), false)];
        assert_eq!(c.decide(&turns), None);
    }
}
