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
    CveTarget, ForkPlatform, IssueInput, LifecycleError, ModeConfig, Modes, Platform, RepoRun,
    ReviewRound, run_issue, run_review,
};
use aegis_pr::{PrError, REPLY_MARKER, TokenSource, TokenUse};
use aegis_provider::{Observed, Provider};
use aegis_secrets::{Secret, Secrets};
use aegis_store::{Claim, Job, JobSource, ModeName, PgLedger, RepoId, RunId, Store};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

use crate::metrics::JobObserver;
use crate::runner::{Attempt, JobRunner};
use crate::sonar::sonar_issue;
use crate::tracker::{enrich_with_apm, ticket_issue};

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
        self.pg.append_in_run(issue, event, run).await?;
        match event {
            LedgerEvent::Triaged { triage } => crate::metrics::metrics().triaged(triage.category),
            LedgerEvent::SecretsBlocked { place, count } => {
                crate::metrics::metrics().secrets_blocked(place, *count);
            }
            LedgerEvent::Diagnosed { category, .. } => {
                crate::metrics::metrics().diagnosed(*category);
            }
            _ => {}
        }
        Ok(())
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
    /// The provider refuses what `name`'s residency does not allow (#592),
    /// on top of the check of its models when the mode was loaded.
    async fn new(name: &ModeName, mode: &ModeConfig, secrets: &Secrets) -> Result<Self, String> {
        let policy = mode.check(name).map_err(|e| e.to_string())?;
        let prices = PriceTable::load(&mode.run.pricing).map_err(|e| e.to_string())?;
        for model in [&mode.models.light, &mode.models.core, &mode.models.expert] {
            prices.price(&model.id).map_err(|e| e.to_string())?;
        }
        let provider = Provider::from_config_with_secrets(&mode.provider, secrets)
            .await
            .map_err(|e| e.to_string())?
            .with_policy(policy);
        Ok(Self { provider, prices })
    }
}

pub struct LifecycleRunner {
    configs: RepoConfigs,
    store: Store,
    ledger: RunLedger,
    profiles: Profiles,
    secrets: Secrets,
    /// Each configured mode's, by [`Modes::config`]'s name.
    runtimes: Vec<(ModeName, ModeRuntime)>,
    /// The instance's GitHub App (ADR 0017), for repos on the app path.
    github_app: Option<Arc<crate::github_app::AppHandle>>,
}

impl LifecycleRunner {
    /// The provider and prices of each configured mode, checked now: a
    /// mode that cannot run (its API key missing from `secrets`) fails the
    /// start, not its jobs.
    pub async fn new(store: Store, modes: Modes, secrets: Secrets) -> Result<Self, String> {
        let mut runtimes = Vec::new();
        for (name, mode) in modes.iter() {
            let runtime = ModeRuntime::new(name, mode, &secrets)
                .await
                .map_err(|e| format!("mode {name}: {e}"))?;
            runtimes.push((name.clone(), runtime));
        }
        Ok(Self {
            configs: RepoConfigs::new(modes, store.clone()),
            ledger: RunLedger::new(store.ledger()),
            store,
            profiles: Profiles::builtin().map_err(|e| e.to_string())?,
            secrets,
            runtimes,
            github_app: None,
        })
    }

    /// With the instance's GitHub App (ADR 0017): its repos' jobs take
    /// their tokens from it.
    pub fn with_github_app(self, app: Option<Arc<crate::github_app::AppHandle>>) -> Self {
        Self {
            github_app: app,
            ..self
        }
    }

    /// Where `config`'s job takes its GitHub tokens: the fork's secret on
    /// the token path (none when it opens no PR, as before), the app on
    /// the app path, whose API root the repo then uses.
    async fn token_source(
        &self,
        slug: &str,
        tenant: aegis_store::TenantId,
        config: &mut aegis_lifecycle::RunConfig,
    ) -> Result<TokenSource, Attempt> {
        let Some(connection) = &config.repo.app else {
            if !config.run.open_prs {
                return Ok(TokenSource::Static(String::new()));
            }
            let name = &config.repo.fork.token_env;
            let token = self.secret(slug, "the fork's token", name).await?;
            return Ok(TokenSource::Static(token.expose().to_owned()));
        };
        let handle = self.github_app.as_ref().ok_or_else(|| {
            Attempt::Rejected(format!(
                "repo `{slug}` is on the GitHub App path: the instance has no `[github_app]`"
            ))
        })?;
        match &connection.api {
            Some(api) if api.trim_end_matches('/') != handle.api().trim_end_matches('/') => {
                return Err(Attempt::Rejected(format!(
                    "repo `{slug}`: its app API `{api}` is not the instance app's `{}`",
                    handle.api()
                )));
            }
            _ => config.repo.fork.api = handle.api().to_owned(),
        }
        // An installation suspended or removed: no job, with the reason
        // its connection page gives too (ADR 0017 §5).
        let (owner, name) = (&config.repo.fork.owner, &config.repo.fork.name);
        let state = self
            .store
            .app_on_repo(owner, name)
            .await
            .map_err(|e| Attempt::Infra(e.to_string()))?;
        if let Some(why) = state.and_then(|s| s.refusal(&format!("{owner}/{name}"))) {
            return Err(Attempt::Rejected(format!("repo `{slug}`: {why}")));
        }
        let app = handle.app().await.map_err(Attempt::Rejected)?;
        Ok(TokenSource::App {
            app,
            owner: config.repo.fork.owner.clone(),
            repo: config.repo.fork.name.clone(),
            use_: TokenUse::Publish,
            // The app's gate gives tokens only for the tenant its
            // installation is bound to (ADR 0017 §6).
            holder: Some(tenant.0),
        })
    }

    /// The runtime of `mode`, by the name [`Modes::config`] finds it
    /// under (`_` and `-` alike).
    fn runtime(&self, mode: &ModeName) -> Result<&ModeRuntime, Attempt> {
        let key = mode.key();
        self.runtimes
            .iter()
            .find(|(name, _)| name.key() == key)
            .map(|(_, runtime)| runtime)
            .ok_or_else(|| Attempt::Rejected(format!("no configuration for mode `{mode}`")))
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
        let runtime = self.runtime(&repo.mode)?;
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
        if config.budget.cost_spent(spent) {
            return Ok(Attempt::Ended {
                outcome: Outcome::StoppedBudget,
                reason: Some(format!(
                    "repo `{}`'s budget of ${budget:.2} is spent: ${spent:.2}",
                    repo.slug
                )),
                pr_url: None,
                patch: None,
                diagnosis: None,
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
                patch: None,
                diagnosis: None,
            });
        }
        let github = self
            .token_source(&repo.slug, repo.tenant_id, &mut config)
            .await?;
        // The main branch as it is now, not as the clone last saw it; a
        // worker on a new machine makes the clone (#234).
        let read_token = match &github {
            TokenSource::App { .. } => Some(use_token(&github, TokenUse::Read).await?),
            TokenSource::Static(t) if !t.is_empty() => Some(t.clone()),
            TokenSource::Static(_) => self
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
            read_token
                .as_deref()
                .map(|t| (git_user(&config.repo.fork), t)),
        )
        .await
        // The remote may be back for the next attempt.
        .map_err(|e| Attempt::Infra(format!("updating the clone of {}: {e}", repo.url)))?;
        if let Some(sonar) = &mut config.sonar {
            let name = &sonar.token_env;
            sonar.token = Some(self.secret(&repo.slug, "Sonar's token", name).await?);
        }
        let round = match job.source {
            JobSource::Review => {
                let token = use_token(&github, TokenUse::ReviewEdits).await?;
                Some(self.review_round(job, &config, &token).await?)
            }
            _ => None,
        };
        let mut issue = match (&round, job.source) {
            (Some((root, _)), _) => {
                // The issue the PR is for, read as its job read it.
                let fork = &config.repo.fork;
                let read = match root.source {
                    JobSource::Gitlab => {
                        let token = use_token(&github, TokenUse::Read).await?;
                        gitlab_issue(root, fork, &token).await?
                    }
                    JobSource::Ci => {
                        let token = use_token(&github, TokenUse::Ci).await?;
                        ci_issue(root, fork, &repo.default_branch, &token).await?
                    }
                    _ => {
                        let token = use_token(&github, TokenUse::Read).await?;
                        issue(root, &fork.api, &token).await?
                    }
                };
                IssueInput {
                    id: job.ledger_key.as_str().to_owned(),
                    ..read
                }
            }
            (
                None,
                JobSource::Jira | JobSource::Backlog | JobSource::Jsm | JobSource::Servicenow,
            ) => ticket_issue(job, &repo, &self.secrets).await?,
            (None, JobSource::Sonar) => sonar_issue(job, &repo, &self.store, &self.secrets).await?,
            (None, JobSource::Gitlab) => {
                let token = use_token(&github, TokenUse::Read).await?;
                gitlab_issue(job, &config.repo.fork, &token).await?
            }
            (None, JobSource::Ci) => {
                let token = use_token(&github, TokenUse::Ci).await?;
                ci_issue(job, &config.repo.fork, &repo.default_branch, &token).await?
            }
            (None, JobSource::Cve) => cve_issue(job)?,
            (None, _) => {
                let token = use_token(&github, TokenUse::Read).await?;
                issue(job, &config.repo.fork.api, &token).await?
            }
        };
        if round.is_none() && !matches!(job.source, JobSource::Cve | JobSource::Sonar) {
            enrich_with_apm(&repo, &self.secrets, &mut issue).await;
            crate::memory::enrich_with_memory(&self.store, repo.id, &mut issue).await;
        }
        // Preserve the PostgreSQL queue's canonical intake source (#511).
        if round.is_none() {
            issue.source = Some(job.source.as_str().to_owned());
        }
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
            github,
        });
        let _tag = self.ledger.tag(job.ledger_key.clone(), claim.run.id);
        let fork = platform.config.repo.fork.clone();
        let locale = platform.config.repo.locale;
        let token = platform.github.clone();
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
                "{REPLY_MARKER}\n{}\n\n<sub>aegis-code job {}</sub>",
                (locale.strings().pushed_nothing)(&aegis_pr::inert(reason)),
                job.id.0
            );
            match aegis_lifecycle::fork_forge_of(&fork, &token) {
                Ok(forge) => {
                    if let Err(e) = forge.comment_on_request(pull.number, &text).await {
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
            patch: report.patch,
            diagnosis: report.diagnosis,
        })
    }
}

/// A token of `source` for `use_`, asked for now: the fixed one on the
/// token path, a fresh installation token on the app path. GitHub out of
/// reach is the infrastructure's; a refusal (the app not installed, the
/// repo taken off it) is the job's.
async fn use_token(source: &TokenSource, use_: TokenUse) -> Result<String, Attempt> {
    source.for_use(use_).token().await.map_err(|e| match &e {
        PrError::Http(_) => Attempt::Infra(e.to_string()),
        PrError::GitHub { status, .. } if *status == 0 || *status == 429 || *status >= 500 => {
            Attempt::Infra(e.to_string())
        }
        _ => Attempt::Rejected(e.to_string()),
    })
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
        if !matches!(
            root.source,
            JobSource::Github | JobSource::Cli | JobSource::Gitlab | JobSource::Ci
        ) {
            return Err(Attempt::Rejected(format!(
                "review rounds are for GitHub and GitLab jobs; job {} is a `{}` job",
                root.id.0,
                root.source.as_str()
            )));
        }
        if token.is_empty() {
            return Err(Attempt::Rejected(
                "a review round pushes to the PR: `open_prs` is off".into(),
            ));
        }
        let fork = &config.repo.fork;
        let not_the_forks = || {
            Attempt::Rejected(format!(
                "`{}` is not a PR or MR of the fork {}",
                job.issue_ref,
                fork.path()
            ))
        };
        let read = match fork.platform {
            ForkPlatform::GitHub => {
                let (owner, name, number) =
                    parse_issue_ref(&job.issue_ref).map_err(Attempt::Rejected)?;
                if !(owner.eq_ignore_ascii_case(&fork.owner)
                    && name.eq_ignore_ascii_case(&fork.name))
                {
                    return Err(not_the_forks());
                }
                ReviewRound::from_github(&fork.api, &owner, &name, number, token).await
            }
            // `group/sub/project!iid` (#254).
            ForkPlatform::GitLab => {
                let (project, iid) = job.issue_ref.rsplit_once('!').ok_or_else(not_the_forks)?;
                let iid: u64 = iid.parse().map_err(|_| not_the_forks())?;
                if !project.eq_ignore_ascii_case(&fork.path()) {
                    return Err(not_the_forks());
                }
                ReviewRound::from_gitlab(&fork.api, project, iid, token).await
            }
        };
        let round = read.map_err(|e| match e {
            // Closed, merged, not the agent's: no attempt changes it.
            LifecycleError::Pr(
                aegis_pr::PrError::GitHub {
                    status: 0,
                    ref message,
                }
                | aegis_pr::PrError::GitLab {
                    status: 0,
                    ref message,
                },
            ) if message.starts_with("PR ") || message.starts_with("MR ") => {
                Attempt::Rejected(e.to_string())
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

/// A CVE job (#502): its target read back from `issue_ref` and checked
/// again, under the job's ledger key.
fn cve_issue(job: &Job) -> Result<IssueInput, Attempt> {
    let target = CveTarget::from_ref(&job.issue_ref).map_err(Attempt::Rejected)?;
    Ok(IssueInput {
        id: job.ledger_key.as_str().to_owned(),
        title: target.title(),
        body: format!(
            "Security advisory {}: raise `{}` to {}.",
            target.advisory, target.package, target.version
        ),
        url: target.url(),
        base: None,
        hidden: None,
        kind: None,
        cve: Some(target),
        source: Some("cve".into()),
        memory: String::new(),
    })
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
async fn ci_issue(
    job: &Job,
    fork: &aegis_lifecycle::ForkConfig,
    branch: &str,
    token: &str,
) -> Result<IssueInput, Attempt> {
    let read = match parse_pipeline_ref(&job.issue_ref) {
        // A GitLab pipeline (#253), read from the fork's GitLab.
        Some((project, id)) => {
            if fork.platform != ForkPlatform::GitLab {
                return Err(Attempt::Rejected(
                    "a GitLab pipeline needs the repo's fork on GitLab".into(),
                ));
            }
            IssueInput::from_gitlab_pipeline(&fork.api, project, id, branch, token).await
        }
        None => {
            let (owner, name, run_id) = parse_run_ref(&job.issue_ref).map_err(Attempt::Rejected)?;
            IssueInput::from_github_run(&fork.api, &owner, &name, run_id, branch, token).await
        }
    };
    let issue = read.map_err(|e| match e {
        LifecycleError::Issue(why) => Attempt::Rejected(why),
        LifecycleError::Pr(
            aegis_pr::PrError::GitHub { status: 404, .. }
            | aegis_pr::PrError::GitLab {
                status: 401 | 403 | 404,
                ..
            },
        ) => Attempt::Rejected(e.to_string()),
        other => Attempt::Infra(other.to_string()),
    })?;
    Ok(IssueInput {
        id: job.ledger_key.as_str().to_owned(),
        ..issue
    })
}

/// The GitLab issue of a `gitlab` job (`group/sub/project#iid`, #252),
/// under the job's ledger key, read with the fork's token from the fork's
/// GitLab: the repo's fork must be on GitLab.
async fn gitlab_issue(
    job: &Job,
    fork: &aegis_lifecycle::ForkConfig,
    token: &str,
) -> Result<IssueInput, Attempt> {
    if fork.platform != ForkPlatform::GitLab {
        return Err(Attempt::Rejected(
            "a GitLab issue needs the repo's fork on GitLab (`fork.platform = \"gitlab\"`)".into(),
        ));
    }
    let bad = || Attempt::Rejected(format!("`{}` is not group/project#iid", job.issue_ref));
    let (project, iid) = job.issue_ref.rsplit_once('#').ok_or_else(bad)?;
    let iid: u64 = iid.parse().map_err(|_| bad())?;
    let issue = IssueInput::from_gitlab(&fork.api, project, iid, token)
        .await
        .map_err(|e| match e {
            // No such issue, no access, or not a project path: no attempt
            // changes it. A GitLab that cannot be reached may come back.
            LifecycleError::Pr(aegis_pr::PrError::GitLab {
                status,
                ref message,
            }) if matches!(status, 401 | 403 | 404) || message.contains("not a project path") => {
                Attempt::Rejected(e.to_string())
            }
            other => Attempt::Infra(other.to_string()),
        })?;
    Ok(IssueInput {
        id: job.ledger_key.as_str().to_owned(),
        ..issue
    })
}

/// Who the fork's token is sent as to git: its platform's user (#252).
fn git_user(fork: &aegis_lifecycle::ForkConfig) -> &'static str {
    match fork.platform {
        ForkPlatform::GitHub => aegis_pr::GITHUB_USER,
        ForkPlatform::GitLab => aegis_pr::GITLAB_USER,
    }
}

/// `group/sub/project/-/pipelines/<id>`, as the webhook builds it for a
/// GitLab pipeline (#253).
pub(crate) fn parse_pipeline_ref(text: &str) -> Option<(&str, u64)> {
    let (project, id) = text.trim().split_once("/-/pipelines/")?;
    Some((project, id.parse().ok()?)).filter(|(p, _)| p.contains('/'))
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
    fn pipeline_refs_keep_their_subgroups() {
        assert_eq!(
            parse_pipeline_ref("acme/platform/app/-/pipelines/9"),
            Some(("acme/platform/app", 9))
        );
        for bad in [
            "acme/app/actions/runs/9",
            "app/-/pipelines/9",
            "acme/app/-/pipelines/x",
        ] {
            assert_eq!(parse_pipeline_ref(bad), None, "{bad}");
        }
    }

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
