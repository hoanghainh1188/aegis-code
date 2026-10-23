//! Ledger handle for one issue, used by the issue lifecycle (#16).

use aegis_provider::Usage;

use crate::{
    GateRun, IssueId, IssueKind, IssueRecord, LedgerError, LedgerEvent, LedgerStore, Outcome,
    PriceTable, Review, Tier, TokenCounts,
};

/// Prices each model call, appends every event to the store before
/// updating the in-memory record, and saves the summary record when the
/// issue finishes or is reviewed.
#[derive(Debug)]
pub struct IssueLedger<'a, S> {
    store: &'a S,
    prices: &'a PriceTable,
    regional: bool,
    record: IssueRecord,
}

impl<'a, S: LedgerStore> IssueLedger<'a, S> {
    /// Opens the ledger of `issue`, replaying events already stored (after a
    /// restart, the cost of the interrupted run is kept). `regional` is true
    /// on the Bedrock configuration and adds the regional surcharge.
    pub async fn open(
        store: &'a S,
        prices: &'a PriceTable,
        regional: bool,
        issue: IssueId,
    ) -> Result<Self, LedgerError> {
        let events = store.events(&issue).await?;
        Ok(Self {
            store,
            prices,
            regional,
            record: IssueRecord::from_events(issue, &events),
        })
    }

    pub fn record(&self) -> &IssueRecord {
        &self.record
    }

    pub async fn classified(&mut self, kind: IssueKind) -> Result<(), LedgerError> {
        self.push(LedgerEvent::Classified { kind }).await
    }

    /// Records one model call from its `usage` and returns its cost in $.
    ///
    /// A model missing from the price table is an error and nothing is
    /// recorded, so callers should check every configured model with
    /// [`PriceTable::price`] at startup.
    pub async fn model_call(
        &mut self,
        tier: Tier,
        model: &str,
        usage: &Usage,
    ) -> Result<f64, LedgerError> {
        let tokens = TokenCounts::from_usage(usage);
        let cost_usd = self.prices.cost(model, &tokens, self.regional)?;
        self.push(LedgerEvent::ModelCall {
            tier,
            model: model.to_owned(),
            tokens,
            cost_usd,
        })
        .await?;
        Ok(cost_usd)
    }

    /// Secret values hidden from the model; nothing is recorded for zero.
    pub async fn secrets_redacted(&mut self, count: u32) -> Result<(), LedgerError> {
        if count == 0 {
            return Ok(());
        }
        self.push(LedgerEvent::SecretsRedacted { count }).await
    }

    pub async fn hidden_tests(&mut self, passed: bool, duration_s: f64) -> Result<(), LedgerError> {
        self.push(LedgerEvent::HiddenTests { passed, duration_s })
            .await
    }

    pub async fn gate_run(&mut self, run: GateRun) -> Result<(), LedgerError> {
        self.push(LedgerEvent::GateRun(run)).await
    }

    /// Adds the cost-bearing events of a run done for this issue under
    /// another id (a Sonar group's session, #17): model calls, gate runs
    /// and redactions. Prices stay as they were recorded.
    pub async fn absorb(&mut self, events: &[LedgerEvent]) -> Result<(), LedgerError> {
        for event in events {
            if matches!(
                event,
                LedgerEvent::ModelCall { .. }
                    | LedgerEvent::GateRun(_)
                    | LedgerEvent::SecretsRedacted { .. }
            ) {
                self.push(event.clone()).await?;
            }
        }
        Ok(())
    }

    /// The first outcome is final. Calling again (for example to retry
    /// after `save_record` failed) only saves the record.
    pub async fn finished(
        &mut self,
        outcome: Outcome,
        wall_time_s: f64,
    ) -> Result<(), LedgerError> {
        if self.record.outcome.is_none() {
            self.push(LedgerEvent::Finished {
                outcome,
                wall_time_s,
            })
            .await?;
        }
        self.store.save_record(&self.record).await
    }

    pub async fn reviewed(&mut self, review: Review) -> Result<(), LedgerError> {
        self.push(LedgerEvent::Reviewed(review)).await?;
        self.store.save_record(&self.record).await
    }

    async fn push(&mut self, event: LedgerEvent) -> Result<(), LedgerError> {
        self.store.append(&self.record.issue_id, &event).await?;
        let placeholder = IssueRecord::new(self.record.issue_id.clone());
        let record = std::mem::replace(&mut self.record, placeholder);
        self.record = record.apply(&event);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use aegis_provider::CacheCreation;

    use super::*;
    use crate::{MemoryLedger, ReviewVerdict};

    const PRICES: &str = include_str!("../../../config/pricing.toml");

    fn issue() -> IssueId {
        IssueId::parse("GH-9").unwrap()
    }

    fn usage(input: u64, output: u64, write_5m: u64, read: u64) -> Usage {
        Usage {
            input_tokens: input,
            output_tokens: output,
            cache_creation_input_tokens: write_5m,
            cache_read_input_tokens: read,
            cache_creation: CacheCreation {
                ephemeral_5m_input_tokens: write_5m,
                ephemeral_1h_input_tokens: 0,
            },
        }
    }

    fn assert_usd(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 1e-12,
            "expected ${expected}, got ${actual}"
        );
    }

    // Hand-computed issue on the Claude API configuration:
    //   Light (Haiku):  2 000 in, 200 out
    //                   0.1·2 000/1e6 + 0.5·200/1e6                      = 0.0003
    //   Core #1 (Sonnet): 30 000 in, 1 000 out, 20 000 written (5m)
    //                   2·(20 000·1.25 + 30 000)/1e6 + 10·1 000/1e6      = 0.12
    //   Core #2 (Sonnet): 2 000 in, 500 out, 50 000 read
    //                   2·(50 000·0.05 + 2 000)/1e6 + 10·500/1e6          = 0.014
    //   total                                                             = 0.1343
    #[tokio::test]
    async fn prices_a_whole_issue_and_saves_the_record() {
        let store = MemoryLedger::new();
        let prices = PriceTable::from_toml(PRICES).unwrap();
        let mut ledger = IssueLedger::open(&store, &prices, false, issue())
            .await
            .unwrap();

        ledger.classified(IssueKind::Bug).await.unwrap();
        let light = ledger
            .model_call(Tier::Light, "claude-haiku-5-5", &usage(2_000, 200, 0, 0))
            .await
            .unwrap();
        let core1 = ledger
            .model_call(
                Tier::Core,
                "claude-sonnet-5-5",
                &usage(30_000, 1_000, 20_000, 0),
            )
            .await
            .unwrap();
        ledger
            .gate_run(GateRun {
                passed: false,
                failed_step: Some("test".into()),
                duration_s: 100.0,
            })
            .await
            .unwrap();
        let core2 = ledger
            .model_call(
                Tier::Core,
                "claude-sonnet-5-5",
                &usage(2_000, 500, 0, 50_000),
            )
            .await
            .unwrap();
        ledger
            .gate_run(GateRun {
                passed: true,
                failed_step: None,
                duration_s: 95.0,
            })
            .await
            .unwrap();
        assert_eq!(
            store.record(&issue()),
            None,
            "record saved only when finished"
        );
        ledger.finished(Outcome::PrOpened, 420.0).await.unwrap();

        assert_usd(light, 0.0003);
        assert_usd(core1, 0.12);
        assert_usd(core2, 0.014);
        let saved = store.record(&issue()).unwrap();
        assert_eq!(&saved, ledger.record());
        assert_usd(saved.cost_usd, 0.1343);
        assert_eq!(saved.model_calls, 2);
        assert_eq!(saved.tiers_used, vec![Tier::Light, Tier::Core]);
        assert_eq!(saved.tokens["claude-sonnet-5-5"].cache_write_5m, 20_000);
        assert_eq!(saved.tokens["claude-sonnet-5-5"].cache_read, 50_000);
        assert_eq!(saved.gate_runs.len(), 2);
        assert_eq!(saved.outcome, Some(Outcome::PrOpened));
        assert_eq!(store.events(&issue()).await.unwrap().len(), 7);
    }

    #[tokio::test]
    async fn reopening_keeps_cost_of_the_interrupted_run() {
        let store = MemoryLedger::new();
        let prices = PriceTable::from_toml(PRICES).unwrap();
        {
            let mut ledger = IssueLedger::open(&store, &prices, false, issue())
                .await
                .unwrap();
            ledger
                .model_call(Tier::Core, "claude-sonnet-5-5", &usage(1_000_000, 0, 0, 0))
                .await
                .unwrap();
            // Process dies here: no `finished`.
        }
        let mut ledger = IssueLedger::open(&store, &prices, false, issue())
            .await
            .unwrap();
        assert_usd(ledger.record().cost_usd, 2.0);
        assert_eq!(ledger.record().outcome, None);

        ledger
            .model_call(Tier::Core, "claude-sonnet-5-5", &usage(1_000_000, 0, 0, 0))
            .await
            .unwrap();
        assert_usd(ledger.record().cost_usd, 4.0);
        assert_eq!(ledger.record().model_calls, 2);
    }

    // Bedrock: Opus via `jp.` profile, 4·10 000/1e6 = 0.04, +10% = 0.044.
    #[tokio::test]
    async fn regional_configuration_adds_surcharge() {
        let store = MemoryLedger::new();
        let prices = PriceTable::from_toml(PRICES).unwrap();
        let mut ledger = IssueLedger::open(&store, &prices, true, issue())
            .await
            .unwrap();
        let cost = ledger
            .model_call(
                Tier::Core,
                "jp.anthropic.claude-opus-5-5",
                &usage(10_000, 0, 0, 0),
            )
            .await
            .unwrap();
        assert_usd(cost, 0.044);
        assert!(
            ledger
                .record()
                .tokens
                .contains_key("jp.anthropic.claude-opus-5-5")
        );
    }

    #[tokio::test]
    async fn unknown_model_is_not_recorded() {
        let store = MemoryLedger::new();
        let prices = PriceTable::from_toml(PRICES).unwrap();
        let mut ledger = IssueLedger::open(&store, &prices, false, issue())
            .await
            .unwrap();
        let err = ledger
            .model_call(Tier::Core, "gpt-x", &usage(1, 1, 0, 0))
            .await
            .unwrap_err();
        assert!(matches!(err, LedgerError::UnknownModel(_)));
        assert!(store.events(&issue()).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn absorbs_the_cost_of_a_run_under_another_id() {
        let store = MemoryLedger::new();
        let prices = PriceTable::from_toml(PRICES).unwrap();
        let group = IssueId::parse("GH-9-g1").unwrap();
        let mut sub = IssueLedger::open(&store, &prices, false, group.clone())
            .await
            .unwrap();
        sub.classified(IssueKind::Sonar).await.unwrap();
        sub.model_call(Tier::Core, "claude-sonnet-5-5", &usage(1_000_000, 0, 0, 0))
            .await
            .unwrap();
        sub.gate_run(GateRun {
            passed: true,
            failed_step: None,
            duration_s: 3.0,
        })
        .await
        .unwrap();

        let mut ledger = IssueLedger::open(&store, &prices, false, issue())
            .await
            .unwrap();
        ledger.classified(IssueKind::Sonar).await.unwrap();
        ledger
            .absorb(&store.events(&group).await.unwrap())
            .await
            .unwrap();
        let record = ledger.record();
        assert_usd(record.cost_usd, 2.0);
        assert_eq!(record.model_calls, 1);
        assert_eq!(record.gate_runs.len(), 1);
        assert_eq!(record.kind, Some(IssueKind::Sonar));
        assert_eq!(store.events(&issue()).await.unwrap().len(), 3);
    }

    #[tokio::test]
    async fn finishing_twice_keeps_the_first_outcome() {
        let store = MemoryLedger::new();
        let prices = PriceTable::from_toml(PRICES).unwrap();
        let mut ledger = IssueLedger::open(&store, &prices, false, issue())
            .await
            .unwrap();
        ledger.finished(Outcome::StoppedBudget, 10.0).await.unwrap();
        ledger.finished(Outcome::PrOpened, 20.0).await.unwrap();

        let saved = store.record(&issue()).unwrap();
        assert_eq!(saved.outcome, Some(Outcome::StoppedBudget));
        assert_eq!(saved.wall_time_s, Some(10.0));
        assert_eq!(store.events(&issue()).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn review_after_finish_updates_saved_record() {
        let store = MemoryLedger::new();
        let prices = PriceTable::from_toml(PRICES).unwrap();
        let mut ledger = IssueLedger::open(&store, &prices, false, issue())
            .await
            .unwrap();
        ledger.finished(Outcome::PrOpened, 1.0).await.unwrap();

        let mut later = IssueLedger::open(&store, &prices, false, issue())
            .await
            .unwrap();
        let review = Review {
            verdict: ReviewVerdict::Accepted,
            lines_edited: 0,
            review_minutes: 5.0,
            broke_tests: false,
        };
        later.reviewed(review).await.unwrap();

        let saved = store.record(&issue()).unwrap();
        assert_eq!(saved.review, Some(review));
        assert_eq!(saved.outcome, Some(Outcome::PrOpened));
    }
}
