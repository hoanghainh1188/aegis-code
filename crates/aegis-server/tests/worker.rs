//! Workers against a real queue (#23), with a scripted runner in place of
//! the lifecycle: concurrency limits, retries, restarts, drain, cancel.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aegis_ledger::{IssueId, LedgerEvent, LedgerStore, Outcome};
use aegis_server::runner::{Attempt, JobRunner};
use aegis_server::worker::{WorkerConfig, run_worker};
use aegis_store::testing::test_db;
use aegis_store::{
    Claim, Job, JobFilter, JobSource, JobStatus, NewJob, NewRepo, Page, Patch, RepoId, RepoMode,
    RunStatus, Store,
};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone)]
enum Step {
    /// Takes this long, then ends so.
    After(Duration, Attempt),
    /// Runs until cancelled (or dropped).
    Hang,
}

fn pr() -> Attempt {
    Attempt::Ended {
        outcome: Outcome::PrOpened,
        reason: None,
        pr_url: Some("https://github.com/example/fork/pull/1".into()),
        patch: Some(Patch {
            diff: "diff --git a/src/lib.rs b/src/lib.rs\n-a\n+b\n".into(),
            truncated: false,
        }),
    }
}

/// Plays each job's steps in order, one per attempt (then `After(50ms,
/// PR)`), and counts what runs at once.
#[derive(Default)]
struct Scripted {
    steps: Mutex<HashMap<String, VecDeque<Step>>>,
    default: Option<Step>,
    by_repo: Mutex<HashMap<RepoId, (u32, u32)>>,
    calls: AtomicU32,
    dropped: AtomicU32,
}

impl Scripted {
    fn hanging() -> Self {
        Self {
            default: Some(Step::Hang),
            ..Self::default()
        }
    }

    fn plan(self, issue_ref: &str, steps: Vec<Step>) -> Self {
        self.steps
            .lock()
            .unwrap()
            .insert(issue_ref.into(), steps.into());
        self
    }

    fn most_at_once(&self, repo: RepoId) -> u32 {
        self.by_repo.lock().unwrap().get(&repo).map_or(0, |c| c.1)
    }
}

/// Counts the attempt out when it ends or is dropped.
struct Running<'a> {
    runner: &'a Scripted,
    repo: RepoId,
    ended: bool,
}

impl Drop for Running<'_> {
    fn drop(&mut self) {
        self.runner
            .by_repo
            .lock()
            .unwrap()
            .get_mut(&self.repo)
            .unwrap()
            .0 -= 1;
        if !self.ended {
            self.runner.dropped.fetch_add(1, Ordering::SeqCst);
        }
    }
}

impl JobRunner for Scripted {
    async fn run(&self, claim: &Claim, cancel: CancellationToken) -> Attempt {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let step = self
            .steps
            .lock()
            .unwrap()
            .get_mut(&claim.job.issue_ref)
            .and_then(VecDeque::pop_front)
            .or_else(|| self.default.clone())
            .unwrap_or(Step::After(Duration::from_millis(50), pr()));
        let repo = claim.job.repo_id;
        {
            let mut by_repo = self.by_repo.lock().unwrap();
            let c = by_repo.entry(repo).or_default();
            c.0 += 1;
            c.1 = c.1.max(c.0);
        }
        let mut running = Running {
            runner: self,
            repo,
            ended: false,
        };
        let attempt = match step {
            Step::After(d, attempt) => {
                tokio::select! {
                    () = tokio::time::sleep(d) => attempt,
                    () = cancel.cancelled() => stopped(),
                }
            }
            Step::Hang => {
                cancel.cancelled().await;
                stopped()
            }
        };
        running.ended = true;
        attempt
    }
}

/// What the lifecycle reports when cancelled.
fn stopped() -> Attempt {
    Attempt::Ended {
        outcome: Outcome::StoppedSteps,
        reason: Some("cancelled".into()),
        pr_url: None,
        patch: None,
    }
}

fn config(id: &str) -> WorkerConfig {
    WorkerConfig {
        worker_id: id.into(),
        max_workers: 4,
        lease: Duration::from_millis(600),
        renew_every: Duration::from_millis(100),
        poll_every: Duration::from_millis(50),
        drain: Duration::from_secs(5),
        retry_base: Duration::from_millis(20),
        max_cost_usd: 4.0,
    }
}

async fn repo(s: &Store, slug: &str) -> RepoId {
    limited(s, slug, 3).await
}

/// A repo that runs at most `max` of its jobs at once.
async fn limited(s: &Store, slug: &str, max: u32) -> RepoId {
    s.create_repo(&NewRepo {
        slug: slug.into(),
        url: format!("https://github.com/example/{slug}"),
        default_branch: "main".into(),
        language: "rust".into(),
        mode: RepoMode::Poc,
        max_concurrent_jobs: max,
        config: serde_json::json!({}),
    })
    .await
    .unwrap()
    .id
}

async fn job(s: &Store, repo: RepoId, issue_ref: &str) -> Job {
    let key = issue_ref.replace(['/', '#'], "-");
    s.create_job(&NewJob {
        repo_id: repo,
        issue_ref: issue_ref.into(),
        ledger_key: IssueId::parse(key).unwrap(),
        source: JobSource::Github,
    })
    .await
    .unwrap()
}

async fn all_jobs(s: &Store) -> Vec<Job> {
    s.list_jobs(&JobFilter::default(), Page::first(200))
        .await
        .unwrap()
        .items
}

fn ended(j: &Job) -> bool {
    matches!(
        j.status,
        JobStatus::Done | JobStatus::Failed | JobStatus::Cancelled
    )
}

/// Polls until `done` holds for every job, or fails after `within`.
async fn wait_for(s: &Store, within: Duration, done: impl Fn(&Job) -> bool) -> Vec<Job> {
    let start = Instant::now();
    loop {
        let jobs = all_jobs(s).await;
        if jobs.iter().all(&done) {
            return jobs;
        }
        assert!(start.elapsed() < within, "timed out: {jobs:#?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn spawn_worker(
    s: &Store,
    runner: &Arc<Scripted>,
    config: WorkerConfig,
) -> (CancellationToken, tokio::task::JoinHandle<()>) {
    let shutdown = CancellationToken::new();
    let task = tokio::spawn(run_worker(
        s.clone(),
        Arc::clone(runner),
        config,
        shutdown.clone(),
    ));
    (shutdown, task)
}

async fn runs_of(s: &Store, job: &Job) -> Vec<RunStatus> {
    s.list_runs(job.id, Page::first(20))
        .await
        .unwrap()
        .items
        .iter()
        .map(|r| r.status)
        .collect()
}

#[tokio::test]
async fn ten_jobs_at_once_all_end_with_a_reason_within_the_limits() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    // Repo a takes two jobs at a time, b three.
    let a = limited(s, "a", 2).await;
    let b = repo(s, "b").await;
    let infra = |e: &str| Step::After(Duration::from_millis(30), Attempt::Infra(e.into()));
    let runner = Arc::new(
        Scripted::default()
            .plan("x/a#0", vec![infra("provider unavailable")])
            .plan("x/a#1", vec![infra("container died"); 4])
            .plan(
                "x/a#2",
                vec![Step::After(
                    Duration::from_millis(80),
                    Attempt::Ended {
                        outcome: Outcome::StoppedExpertFail,
                        reason: Some("Expert could not pass the gate".into()),
                        pr_url: None,
                        patch: None,
                    },
                )],
            )
            .plan(
                "x/b#0",
                vec![Step::After(
                    Duration::ZERO,
                    Attempt::Rejected("no connector for `jira` jobs yet".into()),
                )],
            ),
    );
    for n in 0..6 {
        job(s, a, &format!("x/a#{n}")).await;
    }
    for n in 0..4 {
        job(s, b, &format!("x/b#{n}")).await;
    }
    // Two workers share the queue.
    let (stop1, w1) = spawn_worker(s, &runner, config("w1"));
    let (stop2, w2) = spawn_worker(s, &runner, config("w2"));
    let jobs = wait_for(s, Duration::from_secs(30), ended).await;
    stop1.cancel();
    stop2.cancel();
    w1.await.unwrap();
    w2.await.unwrap();

    let by_ref: HashMap<_, _> = jobs.iter().map(|j| (j.issue_ref.as_str(), j)).collect();
    for j in &jobs {
        match j.status {
            JobStatus::Done => assert!(j.outcome.is_some()),
            _ => assert!(j.reason.is_some(), "{j:?} ended without a reason"),
        }
        assert_eq!(j.locked_by, None);
    }
    let a0 = by_ref["x/a#0"];
    assert_eq!(
        (a0.status, a0.outcome, a0.retries),
        (JobStatus::Done, Some(Outcome::PrOpened), 1)
    );
    assert_eq!(
        runs_of(s, a0).await,
        [RunStatus::Failed, RunStatus::Finished]
    );
    // The PR's patch is kept with the job; a stop keeps none (#312).
    let kept = s.patch(a0.id).await.unwrap().unwrap();
    assert!(kept.diff.starts_with("diff --git a/src/lib.rs"), "{kept:?}");
    assert_eq!(s.patch(by_ref["x/a#2"].id).await.unwrap(), None);
    let a1 = by_ref["x/a#1"];
    assert_eq!(
        (a1.status, a1.retries, a1.attempts),
        (JobStatus::Failed, 3, 4)
    );
    assert!(a1.reason.as_deref().unwrap().contains("container died"));
    let a2 = by_ref["x/a#2"];
    assert_eq!(a2.outcome, Some(Outcome::StoppedExpertFail));
    assert_eq!(
        (a2.attempts, a2.retries),
        (1, 0),
        "an agent's stop is not retried"
    );
    let b0 = by_ref["x/b#0"];
    assert_eq!(
        (b0.status, b0.attempts),
        (JobStatus::Failed, 1),
        "rejected: no retry"
    );

    assert!(
        runner.most_at_once(a) <= 2,
        "repo a: {}",
        runner.most_at_once(a)
    );
    assert!(
        runner.most_at_once(b) <= 3,
        "repo b: {}",
        runner.most_at_once(b)
    );
    assert!(runner.most_at_once(a) >= 1 && runner.most_at_once(b) >= 1);
    db.drop().await;
}

#[tokio::test]
async fn a_worker_that_dies_loses_no_job() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let a = limited(s, "a", 5).await;
    for n in 0..5 {
        job(s, a, &format!("x/a#{n}")).await;
    }
    let hanging = Arc::new(Scripted::hanging());
    let dying = WorkerConfig {
        max_workers: 5,
        lease: Duration::from_millis(300),
        ..config("dying")
    };
    let (_never, task) = spawn_worker(s, &hanging, dying);
    wait_for(s, Duration::from_secs(10), |j| {
        j.status == JobStatus::Running
    })
    .await;
    // Killed: no drain, no release; the leases are left to expire.
    task.abort();
    let _ = task.await;
    assert!(
        all_jobs(s)
            .await
            .iter()
            .all(|j| j.locked_by.as_deref() == Some("dying"))
    );

    let runner = Arc::new(Scripted::default());
    let (stop, task) = spawn_worker(s, &runner, config("next"));
    let jobs = wait_for(s, Duration::from_secs(20), ended).await;
    stop.cancel();
    task.await.unwrap();
    for j in &jobs {
        assert_eq!(
            (j.status, j.outcome),
            (JobStatus::Done, Some(Outcome::PrOpened))
        );
        assert_eq!((j.attempts, j.retries), (2, 1));
        assert_eq!(
            runs_of(s, j).await,
            [RunStatus::Interrupted, RunStatus::Finished]
        );
    }
    db.drop().await;
}

#[tokio::test]
async fn a_shutdown_drains_running_jobs_then_releases_what_is_left() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let a = repo(s, "a").await;
    let quick = job(s, a, "x/a#0").await;
    let slow = job(s, a, "x/a#1").await;
    let runner = Arc::new(
        Scripted::default()
            .plan("x/a#0", vec![Step::After(Duration::from_millis(300), pr())])
            .plan("x/a#1", vec![Step::Hang]),
    );
    let short_drain = WorkerConfig {
        drain: Duration::from_millis(800),
        ..config("w1")
    };
    let (stop, task) = spawn_worker(s, &runner, short_drain);
    wait_for(s, Duration::from_secs(10), |j| {
        j.status == JobStatus::Running
    })
    .await;
    stop.cancel();
    task.await.unwrap();

    let quick = s.job(quick.id).await.unwrap().unwrap();
    assert_eq!(quick.status, JobStatus::Done, "finished within the drain");
    let slow = s.job(slow.id).await.unwrap().unwrap();
    assert_eq!(
        (slow.status, slow.retries, &slow.locked_by),
        (JobStatus::Queued, 0, &None)
    );
    assert_eq!(runs_of(s, &slow).await, [RunStatus::Interrupted]);
    assert_eq!(runner.dropped.load(Ordering::SeqCst), 1);
    db.drop().await;
}

#[tokio::test]
async fn a_cancel_stops_the_running_attempt() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let a = repo(s, "a").await;
    let j = job(s, a, "x/a#0").await;
    let runner = Arc::new(Scripted::hanging());
    let (stop, task) = spawn_worker(s, &runner, config("w1"));
    wait_for(s, Duration::from_secs(10), |j| {
        j.status == JobStatus::Running
    })
    .await;
    s.request_cancel(j.id).await.unwrap();
    let jobs = wait_for(s, Duration::from_secs(10), ended).await;
    stop.cancel();
    task.await.unwrap();
    assert_eq!(jobs[0].status, JobStatus::Cancelled);
    assert_eq!(jobs[0].reason.as_deref(), Some("cancelled by request"));
    assert_eq!(runs_of(s, &jobs[0]).await, [RunStatus::Finished]);
    db.drop().await;
}

#[tokio::test]
async fn a_job_whose_budget_is_spent_is_not_run_again() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let a = repo(s, "a").await;
    let j = job(s, a, "x/a#0").await;
    // Earlier attempts spent $4.50 in the job's ledger.
    let call = LedgerEvent::ModelCall {
        tier: aegis_ledger::Tier::Expert,
        model: "m".into(),
        tokens: aegis_ledger::TokenCounts::default(),
        cost_usd: 4.5,
    };
    s.ledger().append(&j.ledger_key, &call).await.unwrap();
    let runner = Arc::new(Scripted::default());
    let (stop, task) = spawn_worker(s, &runner, config("w1"));
    let jobs = wait_for(s, Duration::from_secs(10), ended).await;
    stop.cancel();
    task.await.unwrap();
    assert_eq!(jobs[0].outcome, Some(Outcome::StoppedBudget));
    assert!(
        jobs[0].reason.as_deref().unwrap().contains("$4.50"),
        "{:?}",
        jobs[0].reason
    );
    assert_eq!(runner.calls.load(Ordering::SeqCst), 0);
    db.drop().await;
}

#[tokio::test]
async fn a_worker_that_lost_its_lease_drops_the_attempt() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let a = repo(s, "a").await;
    job(s, a, "x/a#0").await;
    // Renews too late: the lease runs out between two renewals.
    let late = WorkerConfig {
        lease: Duration::from_millis(200),
        renew_every: Duration::from_millis(1_500),
        poll_every: Duration::from_secs(60),
        ..config("late")
    };
    let hanging = Arc::new(Scripted::hanging());
    let (stop_late, late) = spawn_worker(s, &hanging, late);
    wait_for(s, Duration::from_secs(10), |j| {
        j.status == JobStatus::Running
    })
    .await;

    let runner = Arc::new(Scripted::default());
    let (stop, task) = spawn_worker(s, &runner, config("other"));
    let jobs = wait_for(s, Duration::from_secs(10), ended).await;
    assert_eq!(jobs[0].outcome, Some(Outcome::PrOpened));
    let start = Instant::now();
    while hanging.dropped.load(Ordering::SeqCst) == 0 {
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "the late worker kept running"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    stop.cancel();
    stop_late.cancel();
    task.await.unwrap();
    late.await.unwrap();
    assert_eq!(
        runs_of(s, &jobs[0]).await,
        [RunStatus::Interrupted, RunStatus::Finished]
    );
    db.drop().await;
}
