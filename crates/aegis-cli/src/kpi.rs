//! `aegis review` and `aegis report` (#99): the reviewer's verdict on a PR
//! into the ledger, and the Proposal §8 KPIs out of it. Neither needs the
//! model provider.

use std::path::Path;

use aegis_ledger::{
    IssueId, IssueLedger, IssueRecord, JsonFileLedger, Kpi, LedgerStore, Outcome, PriceTable,
    Review, ReviewVerdict,
};
use aegis_lifecycle::{RunConfig, load_set};
use aegis_pr::GitHub;
use aegis_provider::ProviderConfig;

/// What `aegis review` was given.
#[derive(Debug, Default)]
pub struct ReviewArgs {
    pub id: Option<String>,
    pub verdict: Option<String>,
    pub lines: Option<String>,
    pub minutes: Option<String>,
    pub from_pr: Option<String>,
    pub broke_tests: bool,
    pub replace: bool,
}

pub fn store(config: &RunConfig) -> JsonFileLedger {
    JsonFileLedger::new(config.run.state_dir.join("ledger"))
}

fn verdict(text: &str) -> Result<ReviewVerdict, String> {
    match text {
        "accepted" => Ok(ReviewVerdict::Accepted),
        "accepted_with_edits" => Ok(ReviewVerdict::AcceptedWithEdits),
        "rejected" => Ok(ReviewVerdict::Rejected),
        _ => Err(format!(
            "--verdict is accepted, accepted_with_edits or rejected, not `{text}`"
        )),
    }
}

/// The PR number at the end of a PR URL, or a bare number.
fn pr_number(text: &str) -> Result<u64, String> {
    text.trim_end_matches('/')
        .rsplit('/')
        .next()
        .and_then(|n| n.parse().ok())
        .ok_or_else(|| format!("`{text}` is not a PR URL or number"))
}

/// Lines the reviewer changed, from the PR's commits after the agent's.
async fn lines_from_pr(config: &RunConfig, pr: &str) -> Result<u32, String> {
    let fork = &config.repo.fork;
    let token = std::env::var(&fork.token_env)
        .map_err(|_| format!("set {} to read the PR", fork.token_env))?;
    let github =
        GitHub::new(&fork.api, &fork.owner, &fork.name, &token).map_err(|e| e.to_string())?;
    github
        .review_edits(pr_number(pr)?)
        .await
        .map_err(|e| e.to_string())
}

pub async fn review(config: &RunConfig, args: &ReviewArgs) -> Result<String, String> {
    let id = args.id.as_deref().ok_or("--id is required")?;
    let verdict = verdict(args.verdict.as_deref().ok_or("--verdict is required")?)?;
    let minutes: f64 = args
        .minutes
        .as_deref()
        .ok_or("--minutes is required")?
        .parse()
        .map_err(|_| "--minutes must be a number")?;
    if !minutes.is_finite() || minutes < 0.0 {
        return Err("--minutes must be zero or more".into());
    }
    let lines = match (&args.lines, &args.from_pr) {
        (Some(lines), None) => lines
            .parse::<u32>()
            .map_err(|_| "--lines must be a whole number")?,
        (None, Some(pr)) => lines_from_pr(config, pr).await?,
        _ => return Err("give --lines or --from-pr".into()),
    };
    let store = store(config);
    let prices = PriceTable::load(&config.run.pricing).map_err(|e| e.to_string())?;
    let regional = matches!(config.provider, ProviderConfig::Bedrock { .. });
    let issue = IssueId::parse(id).map_err(|e| e.to_string())?;
    if store
        .events(&issue)
        .await
        .map_err(|e| e.to_string())?
        .is_empty()
    {
        return Err(format!("no run `{id}` in the ledger"));
    }
    let mut ledger = IssueLedger::open(&store, &prices, regional, issue)
        .await
        .map_err(|e| e.to_string())?;
    let record = ledger.record();
    if record.outcome != Some(Outcome::PrOpened) {
        return Err(format!("`{id}` did not open a PR ({:?})", record.outcome));
    }
    if record.review.is_some() && !args.replace {
        return Err(format!(
            "`{id}` already has a review; pass --replace to change it"
        ));
    }
    let review = Review {
        verdict,
        lines_edited: lines,
        review_minutes: minutes,
        broke_tests: args.broke_tests,
    };
    ledger.reviewed(review).await.map_err(|e| e.to_string())?;
    Ok(format!("{id}\t{verdict:?}\t{lines} lines\t{minutes} min"))
}

/// Every run's record, rebuilt from its events.
async fn records<S: LedgerStore>(store: &S) -> Result<Vec<IssueRecord>, String> {
    let mut out = Vec::new();
    for issue in store.issues().await.map_err(|e| e.to_string())? {
        let events = store.events(&issue).await.map_err(|e| e.to_string())?;
        out.push(IssueRecord::from_events(issue, &events));
    }
    Ok(out)
}

/// The KPIs of all runs, and per language when `set` names the eval set
/// the runs came from (run ids are `<eval id>-r<n>`).
pub async fn report<S: LedgerStore>(store: &S, set: Option<&Path>) -> Result<String, String> {
    let records = records(store).await?;
    let mut out = format!("## All runs\n{}", Kpi::from_records(&records));
    if let Some(dir) = set {
        let issues = load_set(dir).map_err(|e| e.to_string())?;
        let mut languages: Vec<_> = issues.iter().map(|i| i.language).collect();
        languages.sort();
        languages.dedup();
        for language in languages {
            let prefixes: Vec<String> = issues
                .iter()
                .filter(|i| i.language == language)
                .map(|i| format!("{}-r", i.id.to_ascii_lowercase()))
                .collect();
            let of_language: Vec<&IssueRecord> = records
                .iter()
                .filter(|r| {
                    let id = r.issue_id.as_str().to_ascii_lowercase();
                    prefixes.iter().any(|p| id.starts_with(p.as_str()))
                })
                .collect();
            out.push_str(&format!(
                "\n## {language}\n{}",
                Kpi::from_records(of_language)
            ));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use aegis_ledger::{LedgerEvent, MemoryLedger, Tier, TokenCounts};

    use super::*;

    #[test]
    fn verdicts_and_pr_numbers() {
        assert_eq!(
            verdict("accepted_with_edits"),
            Ok(ReviewVerdict::AcceptedWithEdits)
        );
        assert!(verdict("ok").is_err());
        assert_eq!(pr_number("https://github.com/me/scrapy/pull/12"), Ok(12));
        assert_eq!(pr_number("12"), Ok(12));
        assert!(pr_number("https://github.com/me/scrapy/pull/").is_err());
    }

    fn config(state: &Path) -> RunConfig {
        let poc = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/poc.toml");
        let mut config = RunConfig::load(&poc).unwrap();
        config.run.state_dir = state.to_path_buf();
        config.run.pricing =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/pricing.toml");
        config
    }

    #[tokio::test]
    async fn a_review_is_recorded_once_unless_replaced() {
        let state = std::env::temp_dir().join(format!("aegis-review-{}", std::process::id()));
        let config = config(&state);
        let store = store(&config);
        for (id, outcome) in [
            ("scrapy-7604-r1", Outcome::PrOpened),
            ("scrapy-7604-r2", Outcome::StoppedSteps),
        ] {
            store
                .append(
                    &IssueId::parse(id).unwrap(),
                    &LedgerEvent::Finished {
                        outcome,
                        wall_time_s: 1.0,
                    },
                )
                .await
                .unwrap();
        }
        let args = |id: &str, replace| ReviewArgs {
            id: Some(id.into()),
            verdict: Some("accepted_with_edits".into()),
            lines: Some("3".into()),
            minutes: Some("7.5".into()),
            replace,
            ..ReviewArgs::default()
        };

        assert!(
            review(&config, &args("scrapy-7604-r1", false))
                .await
                .is_ok()
        );
        let again = review(&config, &args("scrapy-7604-r1", false))
            .await
            .unwrap_err();
        assert!(again.contains("already has a review"), "{again}");
        assert!(review(&config, &args("scrapy-7604-r1", true)).await.is_ok());
        let stopped = review(&config, &args("scrapy-7604-r2", false))
            .await
            .unwrap_err();
        assert!(stopped.contains("did not open a PR"), "{stopped}");
        let missing = review(&config, &args("scrapy-1-r9", false))
            .await
            .unwrap_err();
        assert!(missing.contains("no run"), "{missing}");
        let bad = ReviewArgs {
            minutes: Some("-1".into()),
            ..args("scrapy-7604-r1", true)
        };
        assert!(review(&config, &bad).await.is_err());

        let text = report(&store, None).await.unwrap();
        assert!(
            text.contains("| Accepted PRs (≤ 10 lines edited) | 50% (1/2) | ≥ 40% | ✔ |"),
            "{text}"
        );
        assert!(
            text.contains("| Review time per PR | 7.5 min | ≤ 15 min | ✔ |"),
            "{text}"
        );
        let _ = std::fs::remove_dir_all(&state);
    }

    #[tokio::test]
    async fn the_report_splits_runs_by_language() {
        let store = MemoryLedger::new();
        let call = |cost_usd| LedgerEvent::ModelCall {
            tier: Tier::Core,
            model: "core".into(),
            tokens: TokenCounts {
                input: 10,
                cache_read: 90,
                ..TokenCounts::default()
            },
            cost_usd,
        };
        for (id, cost, outcome) in [
            ("typst-7541-r1", 1.0, Outcome::PrOpened),
            ("scrapy-7604-r1", 3.0, Outcome::StoppedBudget),
        ] {
            let issue = IssueId::parse(id).unwrap();
            store.append(&issue, &call(cost)).await.unwrap();
            store
                .append(
                    &issue,
                    &LedgerEvent::Finished {
                        outcome,
                        wall_time_s: 1.0,
                    },
                )
                .await
                .unwrap();
        }
        let set = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../eval/issues");
        let text = report(&store, Some(&set)).await.unwrap();
        assert!(
            text.starts_with("## All runs\n2 runs, 1 PRs opened"),
            "{text}"
        );
        assert!(text.contains("## rust\n1 runs, 1 PRs opened"), "{text}");
        assert!(text.contains("## python\n1 runs, 0 PRs opened"), "{text}");
        assert!(
            text.contains("| Cache read ratio | 90% | ≥ 70% | ✔ |"),
            "{text}"
        );
    }
}
