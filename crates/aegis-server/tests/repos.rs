//! Jobs of many repos (#162): each runs with the configuration its repo
//! has in the `repos` table, within that repo's `max_concurrent_jobs`.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use aegis_lang::Language;
use aegis_ledger::{IssueId, Outcome};
use aegis_lifecycle::{ModeConfig, Modes, RepoSettings, RepoSpec};
use aegis_provider::ProviderConfig;
use aegis_server::lifecycle::RepoConfigs;
use aegis_server::runner::{Attempt, JobRunner};
use aegis_server::worker::{WorkerConfig, run_worker};
use aegis_store::testing::test_db;
use aegis_store::{
    Claim, Job, JobFilter, JobSource, JobStatus, NewJob, NewRepo, Page, RepoId, RepoMode, Store,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

const POC: &str = include_str!("../../../config/poc.toml");
const JP: &str = include_str!("../../../config/customer-jp.toml");

/// What one job ran with.
#[derive(Debug, Clone, PartialEq)]
struct Seen {
    slug: String,
    bedrock: bool,
    core_model: String,
    budget_usd: f64,
    image: Option<String>,
}

/// Resolves each job's configuration as the real runner does, records it,
/// and holds the job a while so that limits show.
struct Resolving {
    configs: RepoConfigs,
    seen: Mutex<HashMap<String, Seen>>,
    running: Mutex<HashMap<RepoId, (u32, u32)>>,
    at_once: AtomicU32,
    most_at_once: AtomicU32,
}

impl Resolving {
    fn new(configs: RepoConfigs) -> Self {
        Self {
            configs,
            seen: Mutex::default(),
            running: Mutex::default(),
            at_once: AtomicU32::new(0),
            most_at_once: AtomicU32::new(0),
        }
    }

    fn most_of(&self, repo: RepoId) -> u32 {
        self.running.lock().unwrap().get(&repo).map_or(0, |c| c.1)
    }
}

impl JobRunner for Resolving {
    async fn run(&self, claim: &Claim, _cancel: CancellationToken) -> Attempt {
        let run = match self.configs.resolve(claim.job.repo_id).await {
            Ok(run) => run,
            Err(attempt) => return attempt,
        };
        let config = &run.config;
        let seen = Seen {
            slug: config.repo.key.clone(),
            bedrock: matches!(config.provider, ProviderConfig::Bedrock { .. }),
            core_model: config.models.core.id.clone(),
            budget_usd: config.budget.max_cost_usd,
            image: config.repo.profile_overrides().unwrap().image,
        };
        self.seen
            .lock()
            .unwrap()
            .insert(claim.job.issue_ref.clone(), seen);
        let repo = claim.job.repo_id;
        {
            let mut running = self.running.lock().unwrap();
            let c = running.entry(repo).or_default();
            c.0 += 1;
            c.1 = c.1.max(c.0);
        }
        let now = self.at_once.fetch_add(1, Ordering::SeqCst) + 1;
        self.most_at_once.fetch_max(now, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(150)).await;
        self.at_once.fetch_sub(1, Ordering::SeqCst);
        self.running.lock().unwrap().get_mut(&repo).unwrap().0 -= 1;
        Attempt::Ended {
            outcome: Outcome::PrOpened,
            reason: None,
            pr_url: Some(format!("https://github.com/me/{}/pull/1", run.repo.slug)),
        }
    }
}

fn modes(with_jp: bool) -> Modes {
    let jp = with_jp.then(|| ModeConfig::from_toml(JP).unwrap());
    Modes::new(Some(ModeConfig::from_toml(POC).unwrap()), jp).unwrap()
}

fn settings(extra: serde_json::Value) -> RepoSettings {
    let mut value = json!({
        "fork": { "owner": "me", "name": "app", "token_env": "GITHUB_TOKEN" }
    });
    value
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    serde_json::from_value(value).unwrap()
}

fn spec(
    slug: &str,
    language: Language,
    mode: RepoMode,
    max: u32,
    extra: serde_json::Value,
) -> RepoSpec {
    RepoSpec {
        slug: slug.into(),
        url: format!("https://github.com/me/{slug}"),
        default_branch: "main".into(),
        language,
        mode,
        max_concurrent_jobs: max,
        settings: settings(extra),
    }
}

async fn job(s: &Store, repo: RepoId, issue_ref: &str) -> Job {
    s.create_job(&NewJob {
        repo_id: repo,
        issue_ref: issue_ref.into(),
        ledger_key: IssueId::parse(issue_ref.replace(['/', '#'], "-")).unwrap(),
        source: JobSource::Github,
    })
    .await
    .unwrap()
}

fn worker() -> WorkerConfig {
    WorkerConfig {
        worker_id: "w1".into(),
        max_workers: 8,
        lease: Duration::from_secs(5),
        renew_every: Duration::from_secs(1),
        poll_every: Duration::from_millis(50),
        drain: Duration::from_secs(5),
        retry_base: Duration::from_millis(20),
        max_cost_usd: 4.0,
    }
}

/// Runs a worker until every job has ended.
async fn run_all(s: &Store, runner: &Arc<Resolving>) -> Vec<Job> {
    let shutdown = CancellationToken::new();
    let task = tokio::spawn(run_worker(
        s.clone(),
        Arc::clone(runner),
        worker(),
        shutdown.clone(),
    ));
    let start = Instant::now();
    let jobs = loop {
        let jobs = s
            .list_jobs(&JobFilter::default(), Page::first(100))
            .await
            .unwrap()
            .items;
        let ended = |j: &Job| matches!(j.status, JobStatus::Done | JobStatus::Failed);
        if jobs.iter().all(ended) {
            break jobs;
        }
        assert!(start.elapsed() < Duration::from_secs(20), "{jobs:#?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    shutdown.cancel();
    task.await.unwrap();
    jobs
}

#[tokio::test]
async fn two_repos_run_side_by_side_each_with_its_own_configuration_and_limit() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let modes = modes(true);
    let internal = modes
        .register(
            s,
            &spec("scrapy", Language::Python, RepoMode::Poc, 3, json!({})),
        )
        .await
        .unwrap();
    let customer = modes
        .register(
            s,
            &spec(
                "app.jp",
                Language::Rust,
                RepoMode::CustomerJp,
                1,
                json!({
                    "image": "registry.local/app-ci:1",
                    "budget": { "max_cost_usd": 2.0, "max_model_calls": 20 }
                }),
            ),
        )
        .await
        .unwrap();
    for n in 0..4 {
        job(s, internal.id, &format!("me/scrapy#{n}")).await;
        job(s, customer.id, &format!("me/app#{n}")).await;
    }
    let runner = Arc::new(Resolving::new(RepoConfigs::new(modes, s.clone())));
    let jobs = run_all(s, &runner).await;

    assert!(
        jobs.iter().all(|j| j.outcome == Some(Outcome::PrOpened)),
        "{jobs:#?}"
    );
    let seen = runner.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 8);
    for n in 0..4 {
        let poc = &seen[&format!("me/scrapy#{n}")];
        assert_eq!(poc.slug, "scrapy");
        assert!(!poc.bedrock, "a poc repo uses the Claude API");
        assert_eq!(poc.budget_usd, 4.0);
        assert_eq!(poc.image, None);
        let jp = &seen[&format!("me/app#{n}")];
        assert_eq!(jp.slug, "app.jp");
        assert!(jp.bedrock && jp.core_model.starts_with("jp."), "{jp:?}");
        assert_eq!(jp.budget_usd, 2.0);
        assert_eq!(jp.image.as_deref(), Some("registry.local/app-ci:1"));
    }
    // Each repo within its own limit, and both at once.
    assert_eq!(runner.most_of(customer.id), 1);
    let poc_most = runner.most_of(internal.id);
    assert!((2..=3).contains(&poc_most), "scrapy ran {poc_most} at once");
    assert!(runner.most_at_once.load(Ordering::SeqCst) > poc_most);
    db.drop().await;
}

#[tokio::test]
async fn a_job_of_a_repo_whose_mode_is_not_configured_stops_with_the_reason() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    // Registered by a deployment that has `customer-jp`; this one has not.
    let orphan = s
        .create_repo(&NewRepo {
            slug: "app.jp".into(),
            url: "https://github.com/me/app".into(),
            default_branch: "main".into(),
            language: "rust".into(),
            mode: RepoMode::CustomerJp,
            max_concurrent_jobs: 1,
            config: serde_json::to_value(settings(json!({}))).unwrap(),
        })
        .await
        .unwrap();
    let broken = s
        .create_repo(&NewRepo {
            slug: "broken".into(),
            url: "https://github.com/me/broken".into(),
            default_branch: "main".into(),
            language: "rust".into(),
            mode: RepoMode::Poc,
            max_concurrent_jobs: 1,
            config: json!({ "fork": "not a table" }),
        })
        .await
        .unwrap();
    job(s, orphan.id, "me/app#1").await;
    job(s, broken.id, "me/broken#1").await;
    let runner = Arc::new(Resolving::new(RepoConfigs::new(modes(false), s.clone())));
    let jobs = run_all(s, &runner).await;

    for j in &jobs {
        assert_eq!(
            (j.status, j.attempts),
            (JobStatus::Failed, 1),
            "not retried: {j:?}"
        );
    }
    let reason = |r: &str| {
        jobs.iter()
            .find(|j| j.issue_ref == r)
            .and_then(|j| j.reason.clone())
            .unwrap()
    };
    let orphan = reason("me/app#1");
    assert!(
        orphan.contains("no configuration for mode `customer-jp`"),
        "{orphan}"
    );
    let broken = reason("me/broken#1");
    assert!(broken.contains("repo `broken`"), "{broken}");
    assert!(runner.seen.lock().unwrap().is_empty());
    db.drop().await;
}
