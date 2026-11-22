//! Tickets from Jira and Backlog (#26). A repo whose settings have a
//! `tracker` takes the tickets its filter selects. The `api` role polls
//! the filters every `trackers.poll_s`, on a schedule in the database, so
//! one process polls each period (ADR 0003 §4). Each ticket's job is
//! written with a delivery `<repo id>:<key>` in one transaction: a ticket
//! gets one job however long it keeps matching, even if two polls meet.
//! Running it again is a retry through the API.
//!
//! The runner reads the ticket (description, text logs) when the job
//! starts, as it reads a GitHub issue; when the job ends,
//! [`TrackerNotifier`] comments on the ticket and, with a PR, links it and
//! moves the ticket on if configured. Tokens are secrets named by the
//! tracker's `token_env`, read from the server's store (#30) when used.

use std::time::Duration;

use aegis_ledger::IssueId;
use aegis_lifecycle::IssueInput;
use aegis_secrets::Secrets;
use aegis_store::{
    Delivered, Job, JobSource, NewDelivery, NewJob, Page, Repo, SlaPriority, Store, StoreError,
    WebhookSource,
};
use aegis_tracker::{
    ApmClient, ApmConfig, Priority, Report, TelemetryQuery, Tracker, TrackerConfig, TrackerError,
};
use tokio_util::sync::CancellationToken;

use crate::api::default_ledger_key;
use crate::runner::Attempt;
use crate::webhook::{plain_comment_with, pr_url};
use crate::worker::JobNotifier;

/// Repos read per page while polling.
const REPO_PAGE: u32 = 100;
/// The poll's name in the schedule.
const POLL: &str = "trackers";

/// Why a repo's tracker cannot be used.
#[derive(Debug, Clone, PartialEq)]
pub enum Unusable {
    /// Its settings or secret are missing or wrong: nothing to retry.
    Config(String),
    /// The secret store or the tracker did not answer.
    Unreached(String),
}

impl From<Unusable> for Attempt {
    fn from(u: Unusable) -> Self {
        match u {
            Unusable::Config(e) => Attempt::Rejected(e),
            Unusable::Unreached(e) => Attempt::Infra(e),
        }
    }
}

impl std::fmt::Display for Unusable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Config(e) | Self::Unreached(e) => f.write_str(e),
        }
    }
}

impl From<TrackerError> for Unusable {
    fn from(e: TrackerError) -> Self {
        match e {
            TrackerError::Config(m) => Self::Config(m),
            // The ticket is gone, or was never there: no retry finds it.
            e @ TrackerError::Api {
                status: 404 | 410, ..
            } => Self::Config(e.to_string()),
            e @ TrackerError::Api { .. } => Self::Unreached(e.to_string()),
        }
    }
}

/// `repo`'s tracker from its settings, `None` when it has none.
pub fn tracker_config(repo: &Repo) -> Result<Option<TrackerConfig>, Unusable> {
    match repo.config.get("tracker") {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(value) => serde_json::from_value(value.clone())
            .map(Some)
            .map_err(|e| Unusable::Config(format!("repo `{}`: tracker: {e}", repo.slug))),
    }
}

/// `repo`'s APM connector from its settings, `None` when it has none.
pub fn apm_config(repo: &Repo) -> Result<Option<ApmConfig>, Unusable> {
    match repo.config.get("apm") {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(value) => serde_json::from_value(value.clone())
            .map(Some)
            .map_err(|e| Unusable::Config(format!("repo `{}`: apm: {e}", repo.slug))),
    }
}

/// Enriches `issue.body` with production telemetry from the repository's
/// configured APM provider (#452). Fails soft: a missing secret or transient
/// APM error is logged and leaves `issue` untouched.
pub async fn enrich_with_apm(repo: &Repo, secrets: &Secrets, issue: &mut IssueInput) {
    let config = match apm_config(repo) {
        Ok(Some(c)) => c,
        Ok(None) => return,
        Err(e) => {
            tracing::warn!(repo = %repo.slug, error = %e, "ignoring invalid apm config");
            return;
        }
    };
    let Ok(Some(token)) = secrets.get_or_load(config.token_env()).await else {
        tracing::warn!(repo = %repo.slug, secret = %config.token_env(), "missing apm token secret");
        return;
    };
    let app_key = match config.app_key_env() {
        Some(name) => match secrets.get_or_load(name).await {
            Ok(Some(k)) => Some(k),
            _ => {
                tracing::warn!(repo = %repo.slug, secret = %name, "missing apm app key secret");
                return;
            }
        },
        None => None,
    };
    let client = match ApmClient::new(
        &config,
        token.expose(),
        app_key.as_ref().map(|k| k.expose()),
    ) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(repo = %repo.slug, error = %e, "apm client config rejected");
            return;
        }
    };
    let query = TelemetryQuery::from_ticket(&issue.title, &issue.body);
    match client.fetch(&query).await {
        Ok(Some(telemetry)) => issue.body.push_str(&telemetry.to_markdown()),
        Ok(None) => {}
        Err(e) => {
            tracing::warn!(repo = %repo.slug, issue = %issue.id, error = %e, "apm telemetry fetch failed");
        }
    }
}

/// `repo`'s tracker, connected with its token from `secrets`.
pub async fn connect(repo: &Repo, secrets: &Secrets) -> Result<Option<Tracker>, Unusable> {
    let Some(config) = tracker_config(repo)? else {
        return Ok(None);
    };
    let name = config.token_env();
    let token = match secrets.get_or_load(name).await {
        Ok(Some(token)) => token,
        Ok(None) => {
            return Err(Unusable::Config(format!(
                "repo `{}`: no secret {name} (the tracker's token) in {}",
                repo.slug,
                secrets.source()
            )));
        }
        Err(e) => return Err(Unusable::Unreached(e.to_string())),
    };
    Tracker::new(&config, token.expose())
        .map(Some)
        .map_err(|e| Unusable::Config(format!("repo `{}`: tracker: {e}", repo.slug)))
}

fn sources(config: &TrackerConfig) -> (WebhookSource, JobSource) {
    match config {
        TrackerConfig::Jira(_) => (WebhookSource::Jira, JobSource::Jira),
        TrackerConfig::Backlog(_) => (WebhookSource::Backlog, JobSource::Backlog),
        TrackerConfig::Jsm(_) => (WebhookSource::Jsm, JobSource::Jsm),
        TrackerConfig::Servicenow(_) => (WebhookSource::Servicenow, JobSource::Servicenow),
    }
}

fn map_priority(p: Priority) -> SlaPriority {
    match p {
        Priority::P1 => SlaPriority::P1,
        Priority::P2 => SlaPriority::P2,
        Priority::P3 => SlaPriority::P3,
    }
}

/// The issue of a `jira` or `backlog` job of `repo`, under its ledger
/// key: the ticket's description followed by its text logs.
pub async fn ticket_issue(
    job: &Job,
    repo: &Repo,
    secrets: &Secrets,
) -> Result<IssueInput, Attempt> {
    let tracker = connect(repo, secrets).await?.ok_or_else(|| {
        Attempt::Rejected(format!(
            "repo `{}` has no tracker for its `{}` job",
            repo.slug,
            job.source.as_str()
        ))
    })?;
    let ticket = tracker
        .ticket(&job.issue_ref)
        .await
        .map_err(|e| Attempt::from(Unusable::from(e)))?;
    Ok(IssueInput {
        cve: None,
        id: job.ledger_key.as_str().to_owned(),
        body: ticket.body_with_logs(),
        title: ticket.title,
        url: ticket.url,
        base: None,
        hidden: None,
        kind: None,
        source: Some(job.source.as_str().to_owned()),
        memory: String::new(),
    })
}

/// One pass over every repo's filter: a job queued for each new ticket.
/// The jobs refused for a repo's budget are returned, to be told; a repo
/// whose tracker fails is logged and skipped.
pub async fn poll_once(store: &Store, secrets: &Secrets) -> Result<Vec<Job>, StoreError> {
    let mut refused = Vec::new();
    let mut page = Page::first(REPO_PAGE);
    loop {
        let repos = store.list_repos(page).await?;
        for repo in &repos.items {
            refused.extend(poll_repo(store, secrets, repo).await?);
        }
        match repos.next_page() {
            Some(next) => page = next,
            None => return Ok(refused),
        }
    }
}

async fn poll_repo(store: &Store, secrets: &Secrets, repo: &Repo) -> Result<Vec<Job>, StoreError> {
    let (config, tracker) = match (tracker_config(repo), connect(repo, secrets).await) {
        (Ok(Some(config)), Ok(Some(tracker))) => (config, tracker),
        (Ok(None), _) | (_, Ok(None)) => return Ok(Vec::new()),
        (Err(e), _) | (_, Err(e)) => {
            tracing::warn!(repo = %repo.slug, error = %e, "tracker not polled");
            return Ok(Vec::new());
        }
    };
    let tickets = match tracker.search_tickets().await {
        Ok(tickets) => tickets,
        Err(e) => {
            tracing::warn!(repo = %repo.slug, error = %e, "polling the tracker");
            return Ok(Vec::new());
        }
    };
    let (source, job_source) = sources(&config);
    let mut refused = Vec::new();
    for ticket in tickets {
        let key = ticket.key;
        let Ok(ledger_key) = IssueId::parse(default_ledger_key(&key)) else {
            continue;
        };
        let delivery = NewDelivery {
            source,
            delivery_id: format!("{}:{key}", repo.id.0),
            event: "poll".into(),
        };
        let job = NewJob {
            repo_id: repo.id,
            issue_ref: key.clone(),
            ledger_key,
            source: job_source,
            priority: ticket.priority.map(map_priority),
            sla_deadline: ticket.sla_deadline,
        };
        match store.deliver_job(&delivery, &job).await? {
            Delivered::Queued(job) => {
                tracing::info!(job = job.id.0, repo = %repo.slug, key, "job queued from the tracker")
            }
            Delivered::Refused(job) => {
                tracing::info!(job = job.id.0, repo = %repo.slug, key, "job refused: the repo's budget is spent");
                refused.push(*job);
            }
            Delivered::Duplicate | Delivered::AlreadyLive(_) => {}
        }
    }
    Ok(refused)
}

/// Polls when due, every `every`, until `shutdown`; refused jobs are told
/// to `notifier`. A process that finds the poll taken by another waits
/// for the next; database errors are logged and tried again.
pub async fn run_poller<N: JobNotifier>(
    store: Store,
    secrets: Secrets,
    notifier: N,
    every: Duration,
    shutdown: CancellationToken,
) {
    tracing::info!(every = ?every, "tracker poller started");
    // Checks more often than it polls, so a process takes over within a
    // fraction of the period when the one that polled stops.
    let check = (every / 4).max(Duration::from_secs(1));
    loop {
        match poll_if_due(&store, &secrets, every).await {
            Ok(refused) => {
                for job in refused {
                    notifier.job_ended(&job, None).await;
                }
            }
            Err(e) => tracing::error!(error = %e, "polling the trackers"),
        }
        tokio::select! {
            () = shutdown.cancelled() => break,
            () = tokio::time::sleep(check) => {}
        }
    }
    tracing::info!("tracker poller stopped");
}

/// [`poll_once`] if this process claims the poll that is due.
pub async fn poll_if_due(
    store: &Store,
    secrets: &Secrets,
    every: Duration,
) -> Result<Vec<Job>, StoreError> {
    if store.claim_poll(POLL, every).await? {
        poll_once(store, secrets).await
    } else {
        Ok(Vec::new())
    }
}

/// Tells the ticket of every `jira` and `backlog` job how it ended.
#[derive(Debug, Clone)]
pub struct TrackerNotifier {
    store: Store,
    secrets: Secrets,
    /// The language of each repo's comments (#589).
    locales: crate::locale::Locales,
}

impl TrackerNotifier {
    pub fn new(store: Store, secrets: Secrets) -> Self {
        Self {
            store,
            secrets,
            locales: crate::locale::Locales::default(),
        }
    }

    /// Each repo's comments in its language (#589); English otherwise.
    pub fn with_locales(self, locales: crate::locale::Locales) -> Self {
        Self { locales, ..self }
    }

    async fn report(&self, job: &Job, diagnosis: Option<&str>) -> Result<(), String> {
        let repo = self
            .store
            .repo(job.repo_id)
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("no repo {}", job.repo_id.0))?;
        let tracker = connect(&repo, &self.secrets)
            .await
            .map_err(|e| e.to_string())?
            .ok_or("the repo has no tracker now")?;
        // No secret leaves for the tracker (#509): the job's reason may
        // quote what the agent or a tool said.
        let locale = self.locales.of(job.repo_id).await;
        let comment = aegis_pr::masked(&plain_comment_with(job, diagnosis, locale));
        let report = Report {
            comment: &comment,
            pr_url: pr_url(job),
        };
        tracker
            .report(&job.issue_ref, &report)
            .await
            .map_err(|e| e.to_string())
    }
}

impl JobNotifier for TrackerNotifier {
    async fn job_ended(&self, job: &Job, diagnosis: Option<&str>) {
        if !matches!(
            job.source,
            JobSource::Jira | JobSource::Backlog | JobSource::Jsm | JobSource::Servicenow
        ) {
            return;
        }
        match self.report(job, diagnosis).await {
            Ok(()) => tracing::info!(job = job.id.0, "reported on the ticket"),
            Err(e) => tracing::warn!(job = job.id.0, error = %e, "reporting on the ticket"),
        }
    }
}
