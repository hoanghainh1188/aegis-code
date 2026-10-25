//! New Sonar warnings, polled (#27). A repo whose settings have `sonar`
//! (its SonarQube Server or Cloud project) and `sonar_poll` (its schedule
//! and severity) is polled every `sonar_poll.every_s`: the open warnings
//! of the project's main branch at or above the severity, newest first.
//! Those no batch took before become one batch: a `sonar` job written
//! with the warnings it took in one transaction, so a warning that stays
//! open is never taken twice, even if two polls meet. Each repo's schedule
//! is a row of its own in the database (`sonar:<repo id>`); the `api` role
//! checks every `sonar.check_s` which are due, one process per period
//! (ADR 0003 §4).
//!
//! When the job starts, [`sonar_issue`] reads the batch's warnings again
//! by key, the open ones only, and gives the lifecycle an issue of kind
//! Sonar whose body holds them: the batching step (#17) groups them, runs
//! the groups and opens one PR. The token is the secret the repo's
//! `sonar.token_env` names, read from the server's store (#30).

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aegis_ledger::{IssueId, IssueKind};
use aegis_lifecycle::{IssueInput, SonarConfig};
use aegis_secrets::Secrets;
use aegis_sonar_batch::{BatchError, PollConfig, SonarServer, issue_body};
use aegis_store::{Delivered, Job, JobSource, NewJob, Page, Repo, Store, StoreError};
use tokio_util::sync::CancellationToken;

use crate::runner::Attempt;
use crate::tracker::Unusable;

/// Repos read per page while polling.
const REPO_PAGE: u32 = 100;
/// Characters of the repo's slug in a batch's ledger key, which names its
/// integration branch (at most 48 characters); the repo's id keeps keys
/// of repos alike in name apart.
const SLUG_IN_KEY: usize = 16;

impl From<BatchError> for Unusable {
    fn from(e: BatchError) -> Self {
        match e {
            BatchError::Sonar {
                status: 401 | 403 | 404,
                ..
            } => Self::Config(e.to_string()),
            BatchError::Sonar { .. } => Self::Unreached(e.to_string()),
            other => Self::Config(other.to_string()),
        }
    }
}

/// `repo`'s Sonar project and schedule, `None` when it is not polled.
pub fn poll_config(repo: &Repo) -> Result<Option<(SonarConfig, PollConfig)>, Unusable> {
    let setting = |name: &str| match repo.config.get(name) {
        None | Some(serde_json::Value::Null) => None,
        Some(value) => Some(value.clone()),
    };
    let Some(poll) = setting("sonar_poll") else {
        return Ok(None);
    };
    let bad = |e: String| Unusable::Config(format!("repo `{}`: {e}", repo.slug));
    let poll: PollConfig =
        serde_json::from_value(poll).map_err(|e| bad(format!("sonar_poll: {e}")))?;
    poll.check().map_err(|e| bad(e.to_string()))?;
    let sonar = setting("sonar").ok_or_else(|| bad("sonar_poll needs `sonar`".into()))?;
    let sonar: SonarConfig =
        serde_json::from_value(sonar).map_err(|e| bad(format!("sonar: {e}")))?;
    Ok(Some((sonar, poll)))
}

/// The server of `sonar`, with its token from `secrets`.
async fn connect(
    repo: &Repo,
    sonar: &SonarConfig,
    secrets: &Secrets,
) -> Result<SonarServer, Unusable> {
    let name = &sonar.token_env;
    let token = match secrets.get_or_load(name).await {
        Ok(Some(token)) => token,
        Ok(None) => {
            return Err(Unusable::Config(format!(
                "repo `{}`: no secret {name} (Sonar's token) in {}",
                repo.slug,
                secrets.source()
            )));
        }
        Err(e) => return Err(Unusable::Unreached(e.to_string())),
    };
    SonarServer::new(&sonar.url, token.expose(), sonar.organization.as_deref())
        .map_err(|e| Unusable::Config(format!("repo `{}`: {e}", repo.slug)))
}

/// The poll's name in the schedule.
fn schedule(repo: &Repo) -> String {
    format!("sonar:{}", repo.id.0)
}

fn millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis())
}

/// A new batch of `repo`: its name (the job's `issue_ref`) and ledger key.
fn new_batch(repo: &Repo) -> Option<(String, IssueId)> {
    let ms = millis();
    let slug: String = repo
        .slug
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .take(SLUG_IN_KEY)
        .collect();
    let key = format!("{}-{}-sonar-{ms}", slug.trim_matches('-'), repo.id.0);
    let key = IssueId::parse(key).ok()?;
    Some((format!("sonar-{ms}"), key))
}

/// One poll of `repo`'s project: its new warnings, if any, as a batch.
/// The job refused for the repo's budget is returned; a repo whose Sonar
/// fails is logged and skipped.
pub async fn poll_repo(
    store: &Store,
    secrets: &Secrets,
    repo: &Repo,
) -> Result<Option<Job>, StoreError> {
    let (sonar, poll) = match poll_config(repo) {
        Ok(Some(found)) => found,
        Ok(None) => return Ok(None),
        Err(e) => {
            tracing::warn!(repo = %repo.slug, error = %e, "Sonar not polled");
            return Ok(None);
        }
    };
    let found = match connect(repo, &sonar, secrets).await {
        Ok(server) => server
            .new_warnings(&sonar.project_key, &poll)
            .await
            .map_err(Unusable::from),
        Err(e) => Err(e),
    };
    let found = match found {
        Ok(found) => found,
        Err(e) => {
            tracing::warn!(repo = %repo.slug, error = %e, "polling Sonar");
            return Ok(None);
        }
    };
    if found.is_empty() {
        return Ok(None);
    }
    let Some((batch, ledger_key)) = new_batch(repo) else {
        return Ok(None);
    };
    let keys: Vec<String> = found.into_iter().map(|w| w.key).collect();
    let job = NewJob {
        repo_id: repo.id,
        issue_ref: batch.clone(),
        ledger_key,
        source: JobSource::Sonar,
    };
    match store.deliver_sonar_batch(&job, &keys).await? {
        None => Ok(None),
        Some((Delivered::Queued(job), taken)) => {
            tracing::info!(job = job.id.0, repo = %repo.slug, batch, warnings = taken.len(), "Sonar batch queued");
            Ok(None)
        }
        Some((Delivered::Refused(job), taken)) => {
            tracing::info!(job = job.id.0, repo = %repo.slug, batch, warnings = taken.len(), "Sonar batch refused: the repo's budget is spent");
            Ok(Some(*job))
        }
        Some((Delivered::Duplicate | Delivered::AlreadyLive(_), _)) => Ok(None),
    }
}

/// Polls every repo whose schedule is due and this process claims. The
/// jobs refused for a repo's budget are returned.
pub async fn poll_due(store: &Store, secrets: &Secrets) -> Result<Vec<Job>, StoreError> {
    let mut refused = Vec::new();
    let mut page = Page::first(REPO_PAGE);
    loop {
        let repos = store.list_repos(page).await?;
        for repo in &repos.items {
            let poll = match poll_config(repo) {
                Ok(Some((_, poll))) => poll,
                Ok(None) => continue,
                Err(e) => {
                    tracing::warn!(repo = %repo.slug, error = %e, "Sonar not polled");
                    continue;
                }
            };
            if store.claim_poll(&schedule(repo), poll.every()).await? {
                refused.extend(poll_repo(store, secrets, repo).await?);
            }
        }
        match repos.next_page() {
            Some(next) => page = next,
            None => return Ok(refused),
        }
    }
}

/// Checks the schedules every `check` until `shutdown`. Database errors
/// are logged and tried again.
pub async fn run_poller(
    store: Store,
    secrets: Secrets,
    check: Duration,
    shutdown: CancellationToken,
) {
    tracing::info!(check = ?check, "Sonar poller started");
    loop {
        match poll_due(&store, &secrets).await {
            Ok(refused) => {
                for job in refused {
                    tracing::warn!(job = job.id.0, reason = ?job.reason, "Sonar batch not run");
                }
            }
            Err(e) => tracing::error!(error = %e, "polling Sonar"),
        }
        tokio::select! {
            () = shutdown.cancelled() => break,
            () = tokio::time::sleep(check) => {}
        }
    }
    tracing::info!("Sonar poller stopped");
}

/// The issue of a `sonar` job of `repo`, under its ledger key: the
/// batch's warnings that are still open, read again from the server.
pub async fn sonar_issue(
    job: &Job,
    repo: &Repo,
    store: &Store,
    secrets: &Secrets,
) -> Result<IssueInput, Attempt> {
    let sonar = repo
        .config
        .get("sonar")
        .filter(|v| !v.is_null())
        .ok_or_else(|| {
            Attempt::Rejected(format!(
                "repo `{}` has no `sonar` for its Sonar batch",
                repo.slug
            ))
        })?;
    let sonar: SonarConfig = serde_json::from_value(sonar.clone())
        .map_err(|e| Attempt::Rejected(format!("repo `{}`: sonar: {e}", repo.slug)))?;
    let keys = store
        .sonar_batch_warnings(repo.id, &job.issue_ref)
        .await
        .map_err(|e| Attempt::Infra(e.to_string()))?;
    if keys.is_empty() {
        return Err(Attempt::Rejected(format!(
            "repo `{}` has no Sonar batch `{}`",
            repo.slug, job.issue_ref
        )));
    }
    let server = connect(repo, &sonar, secrets).await?;
    let warnings = server
        .warnings(&sonar.project_key, &keys)
        .await
        .map_err(|e| Attempt::from(Unusable::from(e)))?;
    let url = format!(
        "{}/project/issues?id={}&resolved=false",
        sonar.url.trim_end_matches('/'),
        sonar.project_key
    );
    Ok(IssueInput {
        id: job.ledger_key.as_str().to_owned(),
        title: format!(
            "Sonar: {} new warnings in {}",
            warnings.len(),
            sonar.project_key
        ),
        body: issue_body(&sonar.project_key, &warnings),
        url,
        base: None,
        hidden: None,
        kind: Some(IssueKind::Sonar),
    })
}
