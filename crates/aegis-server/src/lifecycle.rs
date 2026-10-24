//! The runner of real jobs: the issue's lifecycle (#16) in its own
//! worktree and gate containers, with the job's ledger in PostgreSQL.
//!
//! Every attempt of a job writes to the same ledger (`jobs.ledger_key`),
//! so the router's budget counts what earlier attempts spent: the cap is
//! the job's, not the attempt's (ADR 0003 §3). Each event also records the
//! run that wrote it, so the cost of an interrupted attempt is measurable.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use aegis_lang::Profiles;
use aegis_ledger::{IssueId, IssueRecord, LedgerError, LedgerEvent, LedgerStore, PriceTable};
use aegis_lifecycle::{IssueInput, Platform, RunConfig, run_issue};
use aegis_provider::Provider;
use aegis_store::{Claim, JobSource, PgLedger, RunId, Store};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::runner::{Attempt, JobRunner};

/// The Postgres ledger, with each event tied to the run of its job that is
/// writing it at the moment.
#[derive(Debug, Clone)]
pub struct RunLedger {
    pg: PgLedger,
    runs: Arc<Mutex<HashMap<IssueId, RunId>>>,
}

impl RunLedger {
    pub fn new(pg: PgLedger) -> Self {
        Self {
            pg,
            runs: Arc::default(),
        }
    }

    fn runs(&self) -> std::sync::MutexGuard<'_, HashMap<IssueId, RunId>> {
        self.runs.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Tags the ledger's events with `run` until the guard is dropped.
    fn tag(&self, issue: IssueId, run: RunId) -> RunTag {
        self.runs().insert(issue.clone(), run);
        RunTag {
            ledger: self.clone(),
            issue,
        }
    }
}

struct RunTag {
    ledger: RunLedger,
    issue: IssueId,
}

impl Drop for RunTag {
    fn drop(&mut self) {
        self.ledger.runs().remove(&self.issue);
    }
}

impl LedgerStore for RunLedger {
    async fn append(&self, issue: &IssueId, event: &LedgerEvent) -> Result<(), LedgerError> {
        let run = self.runs().get(issue).copied();
        self.pg.append_in_run(issue, event, run).await
    }

    async fn events(&self, issue: &IssueId) -> Result<Vec<LedgerEvent>, LedgerError> {
        self.pg.events(issue).await
    }

    async fn save_record(&self, record: &IssueRecord) -> Result<(), LedgerError> {
        self.pg.save_record(record).await
    }

    async fn issues(&self) -> Result<Vec<IssueId>, LedgerError> {
        self.pg.issues().await
    }
}

/// Aborts the task when dropped: a dropped attempt (lease lost, drain
/// over) must not keep running the issue.
struct AbortOnDrop<T>(JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub struct LifecycleRunner {
    platform: Arc<Platform<Provider, RunLedger>>,
    store: Store,
}

impl LifecycleRunner {
    /// The platform of `config`, checked as `aegis run` checks it: the ACI
    /// binary, the prices of every model, the fork token when PRs open.
    pub async fn new(store: Store, config: RunConfig) -> Result<Self, String> {
        config.check_aci_bin().map_err(|e| e.to_string())?;
        let prices = PriceTable::load(&config.run.pricing).map_err(|e| e.to_string())?;
        for model in [
            &config.models.light,
            &config.models.core,
            &config.models.expert,
        ] {
            prices.price(&model.id).map_err(|e| e.to_string())?;
        }
        let github_token = if config.run.open_prs {
            let env = &config.repo.fork.token_env;
            std::env::var(env).map_err(|_| format!("set {env} to a token for the fork"))?
        } else {
            String::new()
        };
        let provider = Provider::from_config(&config.provider)
            .await
            .map_err(|e| e.to_string())?;
        let platform = Platform {
            store: RunLedger::new(store.ledger()),
            config,
            provider,
            prices,
            profiles: Profiles::builtin().map_err(|e| e.to_string())?,
            github_token,
        };
        Ok(Self {
            platform: Arc::new(platform),
            store,
        })
    }

    async fn issue(&self, claim: &Claim) -> Result<IssueInput, Attempt> {
        let job = &claim.job;
        if !matches!(job.source, JobSource::Github | JobSource::Cli) {
            return Err(Attempt::Rejected(format!(
                "no connector for `{}` jobs yet",
                job.source.as_str()
            )));
        }
        let (owner, name, number) = parse_issue_ref(&job.issue_ref).map_err(Attempt::Rejected)?;
        let token = Some(self.platform.github_token.as_str()).filter(|t| !t.is_empty());
        let api = &self.platform.config.repo.fork.api;
        let issue = IssueInput::from_github(api, &owner, &name, number, token)
            .await
            .map_err(|e| Attempt::Infra(e.to_string()))?;
        Ok(IssueInput {
            id: job.ledger_key.as_str().to_owned(),
            ..issue
        })
    }

    async fn attempt(&self, claim: &Claim, cancel: CancellationToken) -> Result<Attempt, Attempt> {
        let job = &claim.job;
        let key = &self.platform.config.repo.key;
        let repo = self
            .store
            .repo(job.repo_id)
            .await
            .map_err(|e| Attempt::Infra(e.to_string()))?
            .ok_or_else(|| Attempt::Rejected(format!("repo {} not found", job.repo_id.0)))?;
        if &repo.slug != key {
            return Err(Attempt::Rejected(format!(
                "no run configuration for repo `{}`: this server runs `{key}` (#24)",
                repo.slug
            )));
        }
        // An earlier attempt may have closed the ledger before its worker
        // died: its outcome is the job's.
        let events = self
            .platform
            .store
            .events(&job.ledger_key)
            .await
            .map_err(|e| Attempt::Infra(e.to_string()))?;
        if let Some(outcome) = IssueRecord::from_events(job.ledger_key.clone(), &events).outcome {
            return Ok(Attempt::Ended {
                outcome,
                reason: Some("ended by an earlier attempt".into()),
                pr_url: None,
            });
        }
        let issue = self.issue(claim).await?;
        let _tag = self
            .platform
            .store
            .tag(job.ledger_key.clone(), claim.run.id);
        let mut task = AbortOnDrop(tokio::spawn(run_issue(
            Arc::clone(&self.platform),
            issue,
            cancel,
        )));
        let report = match (&mut task.0).await {
            Ok(Ok(report)) => report,
            Ok(Err(e)) => return Err(Attempt::Infra(e.to_string())),
            Err(e) => return Err(Attempt::Infra(format!("the job panicked: {e}"))),
        };
        Ok(Attempt::Ended {
            outcome: report.outcome,
            reason: report.reason,
            pr_url: report.pr_url,
        })
    }
}

impl JobRunner for LifecycleRunner {
    async fn run(&self, claim: &Claim, cancel: CancellationToken) -> Attempt {
        self.attempt(claim, cancel).await.unwrap_or_else(|a| a)
    }
}

/// `owner/repo#number`.
fn parse_issue_ref(text: &str) -> Result<(String, String, u64), String> {
    let bad = || format!("`{text}` is not owner/repo#number");
    let (repo, number) = text.trim().split_once('#').ok_or_else(bad)?;
    let (owner, name) = repo.split_once('/').ok_or_else(bad)?;
    let number = number.parse().map_err(|_| bad())?;
    if owner.is_empty() || name.is_empty() {
        return Err(bad());
    }
    Ok((owner.to_owned(), name.to_owned(), number))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_refs_are_owner_repo_number() {
        assert_eq!(
            parse_issue_ref("typst/typst#12").unwrap(),
            ("typst".into(), "typst".into(), 12)
        );
        for bad in ["typst#12", "typst/typst", "/typst#1", "a/b#x"] {
            assert!(parse_issue_ref(bad).is_err(), "{bad}");
        }
    }
}
