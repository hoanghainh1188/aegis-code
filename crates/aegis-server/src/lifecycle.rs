//! The runner of real jobs: the issue's lifecycle (#16) in its own
//! worktree and gate containers, with the job's ledger in PostgreSQL. Each
//! job runs with its repo's configuration from the `repos` table: the
//! mode's provider and models, the repo's own image, Sonar project,
//! allow-list and budget (#24, #162). Its secrets (the mode's API key,
//! the fork's and Sonar's tokens) come from the server's store (#30), as
//! they are when the attempt starts.
//!
//! Every attempt of a job writes to the same ledger (`jobs.ledger_key`),
//! so the router's budget counts what earlier attempts spent: the cap is
//! the job's, not the attempt's (ADR 0003 §3). Each event also records the
//! run that wrote it, so the cost of an interrupted attempt is measurable.
//!
//! A `jira` or `backlog` job reads its ticket from its repo's tracker
//! (#26); a `sonar` job the warnings its batch took, read again from the
//! repo's Sonar (#27); a `github` or `cli` job its GitHub issue.
//!
//! The attempt adds its repo's slug and language to the worker's `job`
//! span, which the issue's task runs in, and its model calls are counted
//! under them ([`JobObserver`]).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use aegis_lang::Profiles;
use aegis_ledger::{
    IssueId, IssueRecord, LedgerError, LedgerEvent, LedgerStore, Outcome, PriceTable,
};
use aegis_lifecycle::{
    IssueInput, LifecycleError, ModeConfig, Modes, Platform, RepoRun, ReviewRound, run_issue,
    run_review,
};
use aegis_pr::{GitHub, REPLY_MARKER};
use aegis_provider::{Observed, Provider};
use aegis_secrets::{Secret, Secrets};
use aegis_store::{Claim, Job, JobSource, PgLedger, RepoId, RepoMode, RunId, Store};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

use crate::metrics::JobObserver;
use crate::runner::{Attempt, JobRunner};
use crate::sonar::sonar_issue;
use crate::tracker::ticket_issue;

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
    async fn new(mode: &ModeConfig, secrets: &Secrets) -> Result<Self, String> {
        let prices = PriceTable::load(&mode.run.pricing).map_err(|e| e.to_string())?;
        for model in [&mode.models.light, &mode.models.core, &mode.models.expert] {
            prices.price(&model.id).map_err(|e| e.to_string())?;
        }
        let provider = Provider::from_config_with_secrets(&mode.provider, secrets)
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
    secrets: Secrets,
    poc: Option<ModeRuntime>,
    customer_jp: Option<ModeRuntime>,
}

impl LifecycleRunner {
    /// The provider and prices of each configured mode, checked now: a
    /// mode that cannot run (its API key missing from `secrets`) fails the
    /// start, not its jobs.
    pub async fn new(store: Store, modes: Modes, secrets: Secrets) -> Result<Self, String> {
        let runtime = |mode: Option<ModeConfig>| async {
            match mode {
                Some(m) => ModeRuntime::new(&m, &secrets).await.map(Some),
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
            secrets,
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

    /// Secret `name` for repo `slug`'s job: `Rejected` when the store has
    /// none, `Infra` when it cannot be read.
    async fn secret(&self, slug: &str, what: &str, name: &str) -> Result<Secret, Attempt> {
        match self.secrets.get_or_load(name).await {
            Ok(Some(secret)) => Ok(secret),
            Ok(None) => Err(Attempt::Rejected(format!(
                "repo `{slug}`: no secret {name} ({what}) in {}",
                self.secrets.source()
            ))),
            Err(e) => Err(Attempt::Infra(e.to_string())),
        }
    }

    async fn attempt(&self, claim: &Claim, cancel: CancellationToken) -> Result<Attempt, Attempt> {
        let job = &claim.job;
        let RepoRun { repo, mut config } = self.configs.resolve(job.repo_id).await?;
        let span = tracing::Span::current();
        span.record("repo", repo.slug.as_str());
        span.record("language", repo.language.as_str());
        let runtime = self.runtime(repo.mode)?;
        config
            .check_aci_bin()
            .and_then(|()| config.check_codebase_memory())
            .map_err(|e| Attempt::Rejected(e.to_string()))?;
        // The repo's own budget may be below the worker's cap.
        let spent = self
            .store
            .job_spent(&job.ledger_key)
            .await
            .map_err(|e| Attempt::Infra(e.to_string()))?;
        let budget = config.budget.max_cost_usd;
        if config.budget.limits().is_spent(spent) {
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
            let name = &config.repo.fork.token_env;
            let token = self.secret(&repo.slug, "the fork's token", name).await?;
            token.expose().to_owned()
        } else {
            String::new()
        };
        // The main branch as it is now, not as the clone last saw it; a
        // worker on a new machine makes the clone (#234).
        let read_token = match &github_token {
            t if !t.is_empty() => Some(t.clone()),
            _ => self
                .secrets
                .get_or_load(&config.repo.fork.token_env)
                .await
                .map_err(|e| Attempt::Infra(e.to_string()))?
                .map(|t| t.expose().to_owned()),
        };
        aegis_lifecycle::clone::prepare_async(
            &config.repo.path,
            &repo.url,
            &config.repo.base,
            read_token.as_deref(),
        )
        .await
        // The remote may be back for the next attempt.
        .map_err(|e| Attempt::Infra(format!("updating the clone of {}: {e}", repo.url)))?;
        if let Some(sonar) = &mut config.sonar {
            let name = &sonar.token_env;
            sonar.token = Some(self.secret(&repo.slug, "Sonar's token", name).await?);
        }
        let round = match job.source {
            JobSource::Review => Some(self.review_round(job, &config, &github_token).await?),
            _ => None,
        };
        let issue = match (&round, job.source) {
            (Some((root, _)), _) => IssueInput {
                id: job.ledger_key.as_str().to_owned(),
                ..issue(root, &config.repo.fork.api, &github_token).await?
            },
            (None, JobSource::Jira | JobSource::Backlog) => {
                ticket_issue(job, &repo, &self.secrets).await?
            }
            (None, JobSource::Sonar) => sonar_issue(job, &repo, &self.store, &self.secrets).await?,
            (None, JobSource::Ci) => {
                ci_issue(
                    job,
                    &config.repo.fork.api,
                    &repo.default_branch,
                    &github_token,
                )
                .await?
            }
            (None, _) => issue(job, &config.repo.fork.api, &github_token).await?,
        };
        let tiers = [
            (&config.models.light, "light"),
            (&config.models.core, "core"),
            (&config.models.expert, "expert"),
        ]
        .map(|(model, tier)| (model.id.clone(), tier))
        .to_vec();
        let observer = JobObserver::new(&repo.slug, &repo.language, tiers);
        let platform = Arc::new(Platform {
            config,
            provider: Observed::new(runtime.provider.clone(), observer),
            store: self.ledger.clone(),
            prices: runtime.prices.clone(),
            profiles: self.profiles.clone(),
            github_token,
        });
        let _tag = self.ledger.tag(job.ledger_key.clone(), claim.run.id);
        let fork = platform.config.repo.fork.clone();
        let token = platform.github_token.clone();
        type Run = std::pin::Pin<
            Box<dyn Future<Output = Result<aegis_lifecycle::IssueReport, LifecycleError>> + Send>,
        >;
        let (future, pull): (Run, _) = match round {
            Some((_, round)) => {
                let pull = round.pull.clone();
                (
                    Box::pin(run_review(platform, issue, round, cancel)),
                    Some(pull),
                )
            }
            None => (Box::pin(run_issue(platform, issue, cancel)), None),
        };
        let mut task = AbortOnDrop(tokio::spawn(future.in_current_span()));
        let report = match (&mut task.0).await {
            Ok(Ok(report)) => report,
            Ok(Err(e)) => return Err(Attempt::Infra(e.to_string())),
            Err(e) => return Err(Attempt::Infra(format!("the job panicked: {e}"))),
        };
        if let Some(pull) = pull
            && report.outcome != Outcome::PrOpened
        {
            // The round pushed nothing: the reviewers are told why on the PR.
            let reason = report.reason.as_deref().unwrap_or("it stopped");
            let text = format!(
                "{REPLY_MARKER}\n**aegis-code** pushed nothing for this review: {}.\n\n\
                 <sub>aegis-code job {}</sub>",
                aegis_pr::inert(reason),
                job.id.0
            );
            match GitHub::new(&fork.api, &fork.owner, &fork.name, &token) {
                Ok(github) => {
                    if let Err(e) = github.comment_on_issue(pull.number, &text).await {
                        tracing::warn!(error = %e, "replying on the PR");
                    }
                }
                Err(e) => tracing::warn!(error = %e, "replying on the PR"),
            }
        }
        Ok(Attempt::Ended {
            outcome: report.outcome,
            reason: report.reason,
            pr_url: report.pr_url,
        })
    }
}

impl LifecycleRunner {
    /// A review round's PR (#243): the job that opened it, and the PR as
    /// it is now with its notes. Only a PR of the repo's fork, from an
    /// open `agent/*` branch of it; GitHub issues for now.
    async fn review_round(
        &self,
        job: &Job,
        config: &aegis_lifecycle::RunConfig,
        token: &str,
    ) -> Result<(Job, ReviewRound), Attempt> {
        let root = self
            .store
            .review_root(job)
            .await
            .map_err(|e| Attempt::Infra(e.to_string()))?
            .ok_or_else(|| Attempt::Rejected("a review round of no job".into()))?;
        if !matches!(root.source, JobSource::Github | JobSource::Cli) {
            return Err(Attempt::Rejected(format!(
                "review rounds are for GitHub issues; job {} is a `{}` job",
                root.id.0,
                root.source.as_str()
            )));
        }
        if token.is_empty() {
            return Err(Attempt::Rejected(
                "a review round pushes to the PR: `open_prs` is off".into(),
            ));
        }
        let (owner, name, number) = parse_issue_ref(&job.issue_ref).map_err(Attempt::Rejected)?;
        let fork = &config.repo.fork;
        if !(owner.eq_ignore_ascii_case(&fork.owner) && name.eq_ignore_ascii_case(&fork.name)) {
            return Err(Attempt::Rejected(format!(
                "`{}` is not a PR of the fork {}/{}",
                job.issue_ref, fork.owner, fork.name
            )));
        }
        let round = ReviewRound::from_github(&fork.api, &owner, &name, number, token)
            .await
            .map_err(|e| match e {
                // Closed, merged, not the agent's: no attempt changes it.
                LifecycleError::Pr(aegis_pr::PrError::GitHub { status: 0, message }) => {
                    Attempt::Rejected(message)
                }
                other => Attempt::Infra(other.to_string()),
            })?;
        Ok((root, round))
    }
}

impl JobRunner for LifecycleRunner {
    async fn run(&self, claim: &Claim, cancel: CancellationToken) -> Attempt {
        self.attempt(claim, cancel).await.unwrap_or_else(|a| a)
    }
}

/// The issue of a GitHub or CLI job, under the job's ledger key. Jira and
/// Backlog jobs: [`ticket_issue`]; Sonar batches: [`sonar_issue`].
async fn issue(job: &Job, api: &str, token: &str) -> Result<IssueInput, Attempt> {
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

/// The failed GitHub Actions run of a `ci` job (#250), under the job's
/// ledger key. A run that did not fail, or ran on another branch, is
/// `Rejected`: no attempt changes it.
async fn ci_issue(job: &Job, api: &str, branch: &str, token: &str) -> Result<IssueInput, Attempt> {
    let (owner, name, run_id) = parse_run_ref(&job.issue_ref).map_err(Attempt::Rejected)?;
    let issue = IssueInput::from_github_run(api, &owner, &name, run_id, branch, token)
        .await
        .map_err(|e| match e {
            LifecycleError::Issue(why) => Attempt::Rejected(why),
            LifecycleError::Pr(aegis_pr::PrError::GitHub {
                status: 404,
                message,
            }) => Attempt::Rejected(format!("run {run_id}: {message}")),
            other => Attempt::Infra(other.to_string()),
        })?;
    Ok(IssueInput {
        id: job.ledger_key.as_str().to_owned(),
        ..issue
    })
}

/// `owner/repo/actions/runs/<id>`, as the webhook builds it.
pub(crate) fn parse_run_ref(text: &str) -> Result<(String, String, u64), String> {
    let bad = || format!("`{text}` is not owner/repo/actions/runs/<id>");
    let parts: Vec<&str> = text.trim().split('/').collect();
    let [owner, name, "actions", "runs", id] = parts.as_slice() else {
        return Err(bad());
    };
    let id = id.parse().map_err(|_| bad())?;
    if owner.is_empty() || name.is_empty() {
        return Err(bad());
    }
    Ok(((*owner).to_owned(), (*name).to_owned(), id))
}

/// `owner/repo#number`.
pub(crate) fn parse_issue_ref(text: &str) -> Result<(String, String, u64), String> {
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
    fn run_refs_are_owner_repo_actions_runs_id() {
        assert_eq!(
            parse_run_ref("typst/typst/actions/runs/77").unwrap(),
            ("typst".into(), "typst".into(), 77)
        );
        for bad in [
            "typst/typst#77",
            "typst/typst/actions/runs/x",
            "/x/actions/runs/1",
            "a/b/actions/jobs/1",
        ] {
            assert!(parse_run_ref(bad).is_err(), "{bad}");
        }
    }

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
