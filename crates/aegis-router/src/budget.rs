//! The $4 / 20-call cap, checked before every model call (design §5.3, §5.4).

use std::fmt;
use std::sync::Arc;

use aegis_ledger::{IssueRecord, LedgerError, Outcome, PriceTable, Tier, TokenCounts};
use aegis_orchestrator::{StopCheck, TurnRecord};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BudgetLimits {
    /// Total for the whole job, Light calls and regional surcharge included.
    pub max_cost_usd: f64,
    /// Core + Expert calls for the whole job.
    pub max_model_calls: u32,
}

impl Default for BudgetLimits {
    fn default() -> Self {
        Self {
            max_cost_usd: 4.0,
            max_model_calls: 20,
        }
    }
}

/// What earlier loops and attempts of the same job already used.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Spent {
    pub cost_usd: f64,
    /// Core + Expert calls only.
    pub model_calls: u32,
}

impl Spent {
    /// Reads the totals from the ledger, so a restarted job keeps counting.
    pub fn from_record(record: &IssueRecord) -> Self {
        Self {
            cost_usd: record.cost_usd,
            model_calls: record.model_calls,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum BudgetStop {
    Cost {
        spent_usd: f64,
        next_estimate_usd: f64,
        limit_usd: f64,
    },
    Calls {
        calls: u32,
        limit: u32,
    },
    /// A turn used a model missing from the price table; its cost is
    /// unknown, so the job stops instead of running unmetered.
    Unpriced {
        model: String,
    },
}

impl BudgetStop {
    pub fn outcome(&self) -> Outcome {
        match self {
            Self::Cost { .. } | Self::Unpriced { .. } => Outcome::StoppedBudget,
            Self::Calls { .. } => Outcome::StoppedSteps,
        }
    }
}

impl fmt::Display for BudgetStop {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cost {
                spent_usd,
                next_estimate_usd,
                limit_usd,
            } => write!(
                f,
                "spent ${spent_usd:.4} + next call ~${next_estimate_usd:.4} exceeds ${limit_usd:.2}"
            ),
            Self::Calls { calls, limit } => {
                write!(f, "reached {calls} Core/Expert calls (limit {limit})")
            }
            Self::Unpriced { model } => write!(f, "no price for model `{model}`"),
        }
    }
}

/// [`StopCheck`] for one loop at one tier. `Send + Sync`: it holds only an
/// `Arc` and plain values (ADR 0003).
#[derive(Debug, Clone)]
pub struct BudgetCheck {
    prices: Arc<PriceTable>,
    regional: bool,
    limits: BudgetLimits,
    tier: Tier,
    prior: Spent,
    first_call_estimate_usd: Option<f64>,
}

impl BudgetCheck {
    /// `regional` is true on the Bedrock configuration (surcharge).
    /// `prior` is what the job used before this loop.
    pub fn new(
        prices: Arc<PriceTable>,
        regional: bool,
        limits: BudgetLimits,
        tier: Tier,
        prior: Spent,
    ) -> Self {
        Self {
            prices,
            regional,
            limits,
            tier,
            prior,
            first_call_estimate_usd: None,
        }
    }

    /// Estimate for the first call of this loop, when the average of earlier
    /// calls is a poor guide: after escalation the model changes and the
    /// cache is cold, so the first Expert call is dearer than a Core call.
    pub fn with_first_call_estimate(self, usd: f64) -> Self {
        Self {
            first_call_estimate_usd: Some(usd),
            ..self
        }
    }

    /// Why the next call must not be made, if any. The next call is
    /// estimated at the cost of the last call of this loop; before the first
    /// one, at [`Self::with_first_call_estimate`] or else the average cost of
    /// earlier Core/Expert calls (which includes Light spend, so it errs high).
    pub fn evaluate(&self, turns: &[TurnRecord]) -> Option<BudgetStop> {
        let costs = match self.turn_costs(turns) {
            Ok(costs) => costs,
            Err(model) => return Some(BudgetStop::Unpriced { model }),
        };
        if self.tier.counts_as_model_call() {
            let this_loop = u32::try_from(turns.len()).unwrap_or(u32::MAX);
            let calls = self.prior.model_calls.saturating_add(this_loop);
            if calls >= self.limits.max_model_calls {
                return Some(BudgetStop::Calls {
                    calls,
                    limit: self.limits.max_model_calls,
                });
            }
        }
        let spent_usd = self.prior.cost_usd + costs.iter().sum::<f64>();
        let next_estimate_usd = costs
            .last()
            .copied()
            .or(self.first_call_estimate_usd)
            .unwrap_or_else(|| self.prior_average());
        let projected = spent_usd + next_estimate_usd;
        let over = !projected.is_finite() || projected > self.limits.max_cost_usd;
        over.then_some(BudgetStop::Cost {
            spent_usd,
            next_estimate_usd,
            limit_usd: self.limits.max_cost_usd,
        })
    }

    fn turn_costs(&self, turns: &[TurnRecord]) -> Result<Vec<f64>, String> {
        turns
            .iter()
            .map(|turn| {
                let tokens = TokenCounts::from_usage(&turn.usage);
                self.prices
                    .cost(&turn.model, &tokens, self.regional)
                    .map_err(|e| match e {
                        LedgerError::UnknownModel(model) => model,
                        other => other.to_string(),
                    })
            })
            .collect()
    }

    fn prior_average(&self) -> f64 {
        match self.prior.model_calls {
            0 => 0.0,
            n => self.prior.cost_usd / f64::from(n),
        }
    }
}

impl StopCheck for BudgetCheck {
    fn check(&self, turns: &[TurnRecord]) -> Option<String> {
        self.evaluate(turns).map(|stop| stop.to_string())
    }
}

#[cfg(test)]
mod tests {
    use aegis_ledger::IssueId;
    use aegis_provider::Usage;

    use super::*;

    const PRICES: &str = include_str!("../../../config/pricing.toml");

    fn prices() -> Arc<PriceTable> {
        Arc::new(PriceTable::from_toml(PRICES).unwrap())
    }

    fn check(tier: Tier, prior: Spent) -> BudgetCheck {
        BudgetCheck::new(prices(), false, BudgetLimits::default(), tier, prior)
    }

    fn spent(cost_usd: f64, model_calls: u32) -> Spent {
        Spent {
            cost_usd,
            model_calls,
        }
    }

    /// Sonnet, `input_tokens` uncached tokens at $2 / 1M.
    fn turn(model: &str, input_tokens: u64) -> TurnRecord {
        TurnRecord {
            model: model.into(),
            usage: Usage {
                input_tokens,
                ..Usage::default()
            },
            stop_reason: None,
            tool_calls: Vec::new(),
            attempts: 1,
        }
    }

    /// One Sonnet call costing `usd`.
    fn sonnet(usd: f64) -> TurnRecord {
        turn(
            "claude-sonnet-5-5",
            (usd / 2.0 * 1_000_000.0).round() as u64,
        )
    }

    #[test]
    fn is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync + 'static>() {}
        assert_send_sync::<BudgetCheck>();
    }

    #[test]
    fn fresh_job_may_call() {
        assert_eq!(check(Tier::Core, Spent::default()).evaluate(&[]), None);
    }

    // §5.3 row "any tier, > 20 calls": the 21st Core/Expert call is refused,
    // counting calls of earlier loops (Core before Expert).
    #[test]
    fn stops_at_twenty_calls_across_loops() {
        let expert = check(Tier::Expert, spent(0.0, 18));
        assert_eq!(expert.evaluate(&[sonnet(0.01)]), None);
        let stop = expert.evaluate(&[sonnet(0.01), sonnet(0.01)]).unwrap();
        assert_eq!(
            stop,
            BudgetStop::Calls {
                calls: 20,
                limit: 20
            }
        );
        assert_eq!(stop.outcome(), Outcome::StoppedSteps);
    }

    // §5.3: Light calls do not count towards the 20.
    #[test]
    fn light_calls_are_not_counted() {
        let light = check(Tier::Light, spent(0.0, 20));
        let turns: Vec<_> = (0..5).map(|_| turn("claude-haiku-5-5", 1_000)).collect();
        assert_eq!(light.evaluate(&turns), None);
    }

    // §5.3 row "any tier, > $4", with the next call estimated (§5.4).
    #[test]
    fn stops_when_spent_plus_next_estimate_exceeds_four_dollars() {
        // 2.0 + 0.6 spent, next ~0.6 → 3.2: continue.
        assert_eq!(
            check(Tier::Core, spent(2.0, 3)).evaluate(&[sonnet(0.6)]),
            None
        );
        // 3.0 + 0.6 spent, next ~0.6 → 4.2: stop before calling.
        let stop = check(Tier::Core, spent(3.0, 3))
            .evaluate(&[sonnet(0.6)])
            .unwrap();
        let BudgetStop::Cost {
            spent_usd,
            next_estimate_usd,
            limit_usd,
        } = stop
        else {
            panic!("expected a cost stop, got {stop:?}");
        };
        assert!((spent_usd - 3.6).abs() < 1e-9);
        assert!((next_estimate_usd - 0.6).abs() < 1e-9);
        assert_eq!(limit_usd, 4.0);
    }

    #[test]
    fn first_call_of_a_loop_is_estimated_from_earlier_calls() {
        // Expert starts after Core spent $3.5 in 5 calls: next ~$0.7 → $4.2.
        let stop = check(Tier::Expert, spent(3.5, 5)).evaluate(&[]).unwrap();
        assert_eq!(stop.outcome(), Outcome::StoppedBudget);
        assert_eq!(check(Tier::Expert, spent(3.0, 5)).evaluate(&[]), None);
    }

    #[test]
    fn first_call_estimate_overrides_the_average() {
        // Core averaged $0.40 over 8 calls; the first Expert call is cold.
        let expert = check(Tier::Expert, spent(3.2, 8));
        assert_eq!(expert.clone().evaluate(&[]), None);
        assert!(matches!(
            expert.clone().with_first_call_estimate(1.5).evaluate(&[]),
            Some(BudgetStop::Cost { .. })
        ));
        // Once this loop has a call, its cost is the estimate.
        assert_eq!(
            expert
                .with_first_call_estimate(1.5)
                .evaluate(&[sonnet(0.1)]),
            None
        );
    }

    #[test]
    fn non_finite_estimate_stops() {
        let check = check(Tier::Core, Spent::default()).with_first_call_estimate(f64::NAN);
        assert!(matches!(check.evaluate(&[]), Some(BudgetStop::Cost { .. })));
    }

    #[test]
    fn light_spending_counts_towards_the_cost_cap() {
        let light = check(Tier::Light, spent(3.99, 0));
        assert!(matches!(
            light.evaluate(&[turn("claude-haiku-5-5", 1_000_000)]),
            Some(BudgetStop::Cost { .. })
        ));
    }

    #[test]
    fn regional_surcharge_counts() {
        // Opus via `jp.`: 900k input = $3.6, +10% = $3.96; next ~$3.96 → stop.
        let regional = BudgetCheck::new(
            prices(),
            true,
            BudgetLimits::default(),
            Tier::Core,
            Spent::default(),
        );
        let stop = regional
            .evaluate(&[turn("jp.anthropic.claude-opus-5-5", 900_000)])
            .unwrap();
        let BudgetStop::Cost { spent_usd, .. } = stop else {
            panic!("expected a cost stop");
        };
        assert!((spent_usd - 3.96).abs() < 1e-9);
    }

    #[test]
    fn unpriced_model_stops_the_job() {
        let stop = check(Tier::Core, Spent::default())
            .evaluate(&[turn("mystery-model", 10)])
            .unwrap();
        assert_eq!(
            stop,
            BudgetStop::Unpriced {
                model: "mystery-model".into()
            }
        );
        assert_eq!(stop.outcome(), Outcome::StoppedBudget);
    }

    #[test]
    fn limits_are_values() {
        let tight = BudgetCheck::new(
            prices(),
            false,
            BudgetLimits {
                max_cost_usd: 8.0,
                max_model_calls: 2,
            },
            Tier::Core,
            Spent::default(),
        );
        assert!(matches!(
            tight.evaluate(&[sonnet(3.0), sonnet(0.1)]),
            Some(BudgetStop::Calls { calls: 2, limit: 2 })
        ));
    }

    #[test]
    fn stop_check_reports_a_readable_reason() {
        let reason = check(Tier::Core, spent(0.0, 20)).check(&[]).unwrap();
        assert_eq!(reason, "reached 20 Core/Expert calls (limit 20)");
    }

    #[test]
    fn spent_reads_the_ledger_record() {
        let mut record = IssueRecord::new(IssueId::parse("GH-1").unwrap());
        record.cost_usd = 1.25;
        record.model_calls = 7;
        assert_eq!(Spent::from_record(&record), spent(1.25, 7));
    }
}
