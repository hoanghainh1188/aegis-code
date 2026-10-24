//! The runner of real jobs: the issue's lifecycle (#16) in its own
//! worktree and gate containers, with the job's ledger in PostgreSQL. Each
//! job runs with its repo's configuration from the `repos` table: the
//! mode's provider and models, the repo's own image, Sonar project,
//! allow-list and budget (#24, #162).
//!
//! Every attempt of a job writes to the same ledger (`jobs.ledger_key`),
//! so the router's budget counts what earlier attempts spent: the cap is
//! the job's, not the attempt's (ADR 0003 §3). Each event also records the
//! run that wrote it, so the cost of an interrupted attempt is measurable.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use aegis_lang::Profiles;
use aegis_ledger::{
    IssueId, IssueRecord, LedgerError, LedgerEvent, LedgerStore, Outcome, PriceTable,
};
use aegis_lifecycle::{
    IssueInput, LifecycleError, ModeConfig, Modes, Platform, RepoRun, run_issue,
};
use aegis_provider::Provider;
use aegis_store::{Claim, JobSource, PgLedger, RepoId, RepoMode, RunId, Store};
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

/// Where each job's configuration comes from: its repo in the `repos`
/// table and the configuration of the repo's mode (#24).
#[derive(Debug, Clone)]
pub struct RepoConfigs {
    modes: Modes,
    store: Store,
}

impl RepoConfigs {
    pub fn new(modes: Modes, store: Store) -> Self {
        Self { modes, store }
    }

    /// The repo and the configuration its jobs run with. `Rejected`, with
    /// the reason, when it cannot run: no such repo, no configuration for
    /// its mode, settings that do not build. `Infra` when the database
    /// failed.
    pub async fn resolve(&self, repo: RepoId) -> Result<RepoRun, Attempt> {
        self.modes
            .repo_run(&self.store, repo)
            .await
            .map_err(|e| match e {
                LifecycleError::Store(e) => Attempt::Infra(e.to_string()),
                other => Attempt::Rejected(other.to_string()),
            })
    }
}

/// What the jobs of one mode share: its provider and prices, checked at
/// start.
struct ModeRuntime {
    provider: Provider,
    prices: PriceTable,
}

impl ModeRuntime {
    async fn new(mode: &ModeConfig) -> Result<Self, String> {
        let prices = PriceTable::load(&mode.run.pricing).map_err(|e| e.to_string())?;
        for model in [&mode.models.light, &mode.models.core, &mode.models.expert] {
            prices.price(&model.id).map_err(|e| e.to_string())?;
        }
        let provider = Provider::from_config(&mode.provider)
            .await
            .map_err(|e| e.to_string())?;
        Ok(Self { provider, prices })
    }
}

pub struct LifecycleRunner {
    configs: RepoConfigs,
    store: Store,
    ledger: RunLedger,
    profiles: Profiles,
    poc: Option<ModeRuntime>,
    customer_jp: Option<ModeRuntime>,
}

impl LifecycleRunner {
    /// The provider and prices of each configured mode, checked now: a
    /// mode that cannot run fails the start, not its jobs.
    pub async fn new(store: Store, modes: Modes) -> Result<Self, String> {
        let runtime = |mode: Option<ModeConfig>| async move {
            match mode {
                Some(m) => ModeRuntime::new(&m).await.map(Some),
                None => Ok(None),
            }
        };
        let poc = runtime(modes.poc.clone())
            .await
            .map_err(|e| format!("mode poc: {e}"))?;
        let customer_jp = runtime(modes.customer_jp.clone())
            .await
            .map_err(|e| format!("mode customer-jp: {e}"))?;
        Ok(Self {
            configs: RepoConfigs::new(modes, store.clone()),
            ledger: RunLedger::new(store.ledger()),
            store,
            profiles: Profiles::builtin().map_err(|e| e.to_string())?,
            poc,
            customer_jp,
        })
    }

    fn runtime(&self, mode: RepoMode) -> Result<&ModeRuntime, Attempt> {
        match mode {
            RepoMode::Poc => self.poc.as_ref(),
            RepoMode::CustomerJp => self.customer_jp.as_ref(),
        }
        .ok_or_else(|| Attempt::Rejected(format!("no configuration for mode `{}`", mode.as_str())))
    }

    async fn attempt(&self, claim: &Claim, cancel: CancellationToken) -> Result<Attempt, Attempt> {
        let job = &claim.job;
        let RepoRun { repo, config } = self.configs.resolve(job.repo_id).await?;
        let runtime = self.runtime(repo.mode)?;
        config
            .check_aci_bin()
            .map_err(|e| Attempt::Rejected(e.to_string()))?;
        // The repo's own budget may be below the worker's cap.
        let spent = self
            .store
            .job_spent(&job.ledger_key)
            .await
            .map_err(|e| Attempt::Infra(e.to_string()))?;
        let budget = config.budget.max_cost_usd;
        if spent >= budget {
            return Ok(Attempt::Ended {
                outcome: Outcome::StoppedBudget,
                reason: Some(format!(
                    "repo `{}`'s budget of ${budget:.2} is spent: ${spent:.2}",
                    repo.slug
                )),
                pr_url: None,
            });
        }
        // An earlier attempt may have closed the ledger before its worker
        // died: its outcome is the job's.
        let events = self
            .ledger
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
        let github_token = if config.run.open_prs {
            let env = &config.repo.fork.token_env;
            std::env::var(env).map_err(|_| {
                Attempt::Rejected(format!(
                    "repo `{}`: set {env} to a token for its fork",
                    repo.slug
                ))
            })?
        } else {
            String::new()
        };
        let issue = issue(claim, &config.repo.fork.api, &github_token).await?;
        let platform = Arc::new(Platform {
            config,
            provider: runtime.provider.clone(),
            store: self.ledger.clone(),
            prices: runtime.prices.clone(),
            profiles: self.profiles.clone(),
            github_token,
        });
        let _tag = self.ledger.tag(job.ledger_key.clone(), claim.run.id);
        let mut task = AbortOnDrop(tokio::spawn(run_issue(platform, issue, cancel)));
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

/// The issue of a GitHub or CLI job, under the job's ledger key.
async fn issue(claim: &Claim, api: &str, token: &str) -> Result<IssueInput, Attempt> {
    let job = &claim.job;
    if !matches!(job.source, JobSource::Github | JobSource::Cli) {
        return Err(Attempt::Rejected(format!(
            "no connector for `{}` jobs yet",
            job.source.as_str()
        )));
    }
    let (owner, name, number) = parse_issue_ref(&job.issue_ref).map_err(Attempt::Rejected)?;
    let token = Some(token).filter(|t| !t.is_empty());
    let issue = IssueInput::from_github(api, &owner, &name, number, token)
        .await
        .map_err(|e| Attempt::Infra(e.to_string()))?;
    Ok(IssueInput {
        id: job.ledger_key.as_str().to_owned(),
        ..issue
    })
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
