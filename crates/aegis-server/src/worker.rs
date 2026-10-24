//! The worker (ADR 0003 §3): claims jobs while it has room, runs each
//! attempt in its own task under a lease it renews, and turns how the
//! attempt went into the job's next state. Expired leases of dead workers
//! are reclaimed by whichever worker polls next.
//!
//! Shutdown: no new claims, the running attempts get the drain time to
//! end; those still running then are dropped, their runs `interrupted`
//! and their jobs queued again for the next worker.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aegis_ledger::Outcome;
use aegis_store::{AttemptEnd, Claim, ClaimLimits, JobId, Lease, RepoId, Run, Store};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::runner::{Attempt, JobRunner};

/// Infrastructure failures and expired leases a job survives (ADR 0003).
pub const MAX_RETRIES: u32 = 3;

/// Longest wait between two attempts of a job.
const MAX_BACKOFF: Duration = Duration::from_secs(3600);

#[derive(Debug, Clone, PartialEq)]
pub struct WorkerConfig {
    /// Holds the leases; unique per process.
    pub worker_id: String,
    /// Jobs this process runs at once.
    pub max_workers: u32,
    /// `max_concurrent_jobs` of a repo without its own value.
    pub max_concurrent_jobs: u32,
    /// `max_concurrent_jobs` by repo (#24 supplies these from the repos).
    pub repo_limits: Vec<(RepoId, u32)>,
    pub lease: Duration,
    /// Shorter than `lease`, so one missed renewal does not lose it.
    pub renew_every: Duration,
    /// How often to look for work when nothing has finished.
    pub poll_every: Duration,
    /// How long a shutdown waits for running attempts.
    pub drain: Duration,
    /// First wait after an infrastructure failure; doubles each retry.
    pub retry_base: Duration,
    /// The job's budget over all its attempts ($4, ADR 0003 §3).
    pub max_cost_usd: f64,
}

/// The wait before retry `retries + 1`.
pub fn backoff(base: Duration, retries: u32) -> Duration {
    base.saturating_mul(2u32.saturating_pow(retries))
        .min(MAX_BACKOFF)
}

/// The runs this worker holds, to release them if the drain runs out.
type Active = Arc<Mutex<HashMap<JobId, Run>>>;

/// Runs until `shutdown`, then drains. Errors of the database are logged
/// and retried at the next poll: a worker outlives a database restart.
pub async fn run_worker<R: JobRunner>(
    store: Store,
    runner: Arc<R>,
    config: WorkerConfig,
    shutdown: CancellationToken,
) {
    let config = Arc::new(config);
    let active: Active = Arc::default();
    let mut tasks = JoinSet::new();
    tracing::info!(worker = %config.worker_id, max_workers = config.max_workers, "worker started");
    loop {
        match store.reclaim_expired(MAX_RETRIES).await {
            Ok(ids) if !ids.is_empty() => {
                tracing::warn!(?ids, "reclaimed jobs with expired leases")
            }
            Ok(_) => {}
            Err(e) => tracing::error!(error = %e, "reclaiming expired leases"),
        }
        claim_while_room(&store, &runner, &config, &active, &mut tasks).await;
        tokio::select! {
            () = shutdown.cancelled() => break,
            () = tokio::time::sleep(config.poll_every) => {}
            Some(done) = tasks.join_next(), if !tasks.is_empty() => log_join(done),
        }
    }
    drain(&store, &config, &active, tasks).await;
    tracing::info!(worker = %config.worker_id, "worker stopped");
}

async fn claim_while_room<R: JobRunner>(
    store: &Store,
    runner: &Arc<R>,
    config: &Arc<WorkerConfig>,
    active: &Active,
    tasks: &mut JoinSet<()>,
) {
    let limits = ClaimLimits {
        max_workers: config.max_workers,
        per_repo_default: config.max_concurrent_jobs,
        per_repo: &config.repo_limits,
    };
    while tasks.len() < config.max_workers as usize {
        let claim = match store
            .claim_job(&config.worker_id, config.lease, limits)
            .await
        {
            Ok(Some(claim)) => claim,
            Ok(None) => return,
            Err(e) => {
                tracing::error!(error = %e, "claiming a job");
                return;
            }
        };
        tracing::info!(job = claim.job.id.0, attempt = claim.run.attempt, "claimed");
        lock(active).insert(claim.job.id, claim.run.clone());
        tasks.spawn(attempt(
            store.clone(),
            Arc::clone(runner),
            Arc::clone(config),
            Arc::clone(active),
            claim,
        ));
    }
}

fn lock(active: &Active) -> std::sync::MutexGuard<'_, HashMap<JobId, Run>> {
    // A panic while holding it leaves the map as it was: still usable.
    active
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn log_join(done: Result<(), tokio::task::JoinError>) {
    if let Err(e) = done
        && e.is_panic()
    {
        tracing::error!(error = %e, "an attempt's task panicked");
    }
}

async fn attempt<R: JobRunner>(
    store: Store,
    runner: Arc<R>,
    config: Arc<WorkerConfig>,
    active: Active,
    claim: Claim,
) {
    let job = claim.job.id.0;
    match run_attempt(&store, &*runner, &config, &claim).await {
        Some(end) => match store
            .finish_attempt(&claim.run, &config.worker_id, &end)
            .await
        {
            Ok(j) => {
                tracing::info!(job, status = j.status.as_str(), reason = ?j.reason, "attempt ended")
            }
            Err(e) => tracing::error!(job, error = %e, "recording the attempt's end"),
        },
        None => tracing::warn!(job, "lease lost; the attempt was dropped"),
    }
    lock(&active).remove(&claim.job.id);
}

/// How the attempt ended; `None` when the lease was lost and the job
/// belongs to someone else now.
async fn run_attempt<R: JobRunner>(
    store: &Store,
    runner: &R,
    config: &WorkerConfig,
    claim: &Claim,
) -> Option<AttemptEnd> {
    let spent = match store.job_spent(&claim.job.ledger_key).await {
        Ok(spent) => spent,
        Err(e) => return Some(infra(config, claim, e.to_string())),
    };
    if spent >= config.max_cost_usd {
        return Some(AttemptEnd::Done {
            outcome: Outcome::StoppedBudget,
            reason: Some(format!(
                "the job's budget of ${:.2} is spent: ${spent:.2} over {} attempts",
                config.max_cost_usd,
                claim.run.attempt - 1
            )),
            pr_url: None,
        });
    }
    let cancel = CancellationToken::new();
    tokio::select! {
        attempt = runner.run(claim, cancel.clone()) => {
            Some(end_of(config, claim, attempt, cancel.is_cancelled()))
        }
        () = keep_lease(store, config, claim.job.id, &cancel) => None,
    }
}

/// Renews the lease until it is lost; fires `cancel` when asked to.
async fn keep_lease(store: &Store, config: &WorkerConfig, job: JobId, cancel: &CancellationToken) {
    loop {
        tokio::time::sleep(config.renew_every).await;
        match store
            .renew_lease(job, &config.worker_id, config.lease)
            .await
        {
            Ok(Lease::Held { cancel_requested }) => {
                if cancel_requested && !cancel.is_cancelled() {
                    tracing::info!(job = job.0, "cancel asked for; stopping the attempt");
                    cancel.cancel();
                }
            }
            Ok(Lease::Lost) => return,
            // Try again next time; if the lease runs out meanwhile, the
            // next renewal says it is lost.
            Err(e) => tracing::error!(job = job.0, error = %e, "renewing the lease"),
        }
    }
}

fn end_of(config: &WorkerConfig, claim: &Claim, attempt: Attempt, cancelled: bool) -> AttemptEnd {
    if cancelled && !matches!(attempt, Attempt::Rejected(_)) {
        return AttemptEnd::Cancelled {
            reason: "cancelled by request".into(),
        };
    }
    match attempt {
        Attempt::Ended {
            outcome,
            reason,
            pr_url,
        } => AttemptEnd::Done {
            outcome,
            reason,
            pr_url,
        },
        Attempt::Infra(error) => infra(config, claim, error),
        Attempt::Rejected(error) => AttemptEnd::Failed { error },
    }
}

fn infra(config: &WorkerConfig, claim: &Claim, error: String) -> AttemptEnd {
    let retries = claim.job.retries;
    if retries < MAX_RETRIES {
        AttemptEnd::Retry {
            error,
            after: backoff(config.retry_base, retries),
        }
    } else {
        AttemptEnd::Failed {
            error: format!("{error} (no retry left after {retries})"),
        }
    }
}

async fn drain(store: &Store, config: &WorkerConfig, active: &Active, mut tasks: JoinSet<()>) {
    tracing::info!(running = tasks.len(), drain = ?config.drain, "draining");
    let waited = tokio::time::timeout(config.drain, async {
        while let Some(done) = tasks.join_next().await {
            log_join(done);
        }
    })
    .await;
    if waited.is_ok() {
        return;
    }
    tasks.shutdown().await;
    let left: Vec<Run> = lock(active).drain().map(|(_, run)| run).collect();
    for run in left {
        match store
            .finish_attempt(&run, &config.worker_id, &AttemptEnd::Released)
            .await
        {
            Ok(_) => tracing::warn!(job = run.job_id.0, "drain ran out; job released"),
            Err(e) => tracing::error!(job = run.job_id.0, error = %e, "releasing the job"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_and_is_capped() {
        let base = Duration::from_secs(30);
        assert_eq!(backoff(base, 0), Duration::from_secs(30));
        assert_eq!(backoff(base, 1), Duration::from_secs(60));
        assert_eq!(backoff(base, 2), Duration::from_secs(120));
        assert_eq!(backoff(base, 40), MAX_BACKOFF);
    }
}
