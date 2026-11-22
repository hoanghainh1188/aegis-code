//! ChatOps (#594) against mock Slack and Teams webhooks: the payloads each
//! takes, a slow or failing webhook that only logs, and, with a real
//! Postgres (`AEGIS_TEST_DATABASE_URL`), a worker telling the channels of
//! a PR, a P1 failure and a budget alert.

use std::sync::Arc;
use std::time::{Duration, Instant};

use aegis_ledger::{GateRun, IssueId, LedgerEvent, LedgerStore, Outcome, Tier, TokenCounts};
use aegis_secrets::Secrets;
use aegis_server::chatops::message::{ChatMessage, Link};
use aegis_server::chatops::{
    ChannelSection, ChatEvent, ChatKind, ChatNotifier, ChatSender, ChatopsSection,
};
use aegis_server::runner::{Attempt, JobRunner};
use aegis_server::worker::{JobNotifier, WorkerConfig, run_worker_notifying};
use aegis_store::testing::test_db;
use aegis_store::{
    Actor, Claim, Job, JobFilter, JobSource, JobStatus, ModeName, NewJob, NewRepo, Page, RepoId,
    SlaPriority, Store,
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn channel(kind: ChatKind, secret: &str) -> ChannelSection {
    ChannelSection {
        kind,
        webhook_env: secret.into(),
        tenants: vec![],
        repos: vec![],
        events: vec![
            ChatEvent::PrOpened,
            ChatEvent::Diagnosed,
            ChatEvent::P1Stopped,
            ChatEvent::Budget,
        ],
    }
}

fn message() -> ChatMessage {
    ChatMessage {
        event: ChatEvent::PrOpened,
        title: "aegis-code opened a pull request in api".into(),
        summary: None,
        facts: vec![("Cost".into(), "$1.20".into())],
        link: Some(Link {
            label: "Open pull request",
            url: "https://github.com/acme/api/pull/7".into(),
        }),
        footer: "aegis-code job 1 · github".into(),
    }
}

async fn bodies(server: &MockServer, at: &str) -> Vec<Value> {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|r| r.url.path() == at)
        .map(|r| serde_json::from_slice(&r.body).unwrap())
        .collect()
}

#[tokio::test]
async fn slack_and_teams_get_their_own_payloads() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/slack"))
        .and(header("content-type", "application/json"))
        .and(body_partial_json(json!({
            "text": "aegis-code opened a pull request in api",
            "blocks": [{ "type": "header" }],
        })))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/teams"))
        .and(body_partial_json(json!({
            "type": "message",
            "attachments": [{ "contentType": "application/vnd.microsoft.card.adaptive" }],
        })))
        // Workflows answer 202.
        .respond_with(ResponseTemplate::new(202))
        .expect(1)
        .mount(&server)
        .await;
    let slack_url = format!("{}/slack", server.uri());
    let teams_url = format!("{}/teams", server.uri());
    let secrets = Secrets::fixed([
        ("SLACK_HOOK", slack_url.as_str()),
        ("TEAMS_HOOK", teams_url.as_str()),
    ]);
    let sender = ChatSender::new(secrets);

    sender
        .send(&channel(ChatKind::Slack, "SLACK_HOOK"), &message())
        .await
        .unwrap();
    sender
        .send(&channel(ChatKind::Teams, "TEAMS_HOOK"), &message())
        .await
        .unwrap();

    let teams = &bodies(&server, "/teams").await[0];
    let card = &teams["attachments"][0]["content"];
    assert_eq!(card["type"], "AdaptiveCard");
    assert_eq!(
        card["actions"][0]["url"],
        "https://github.com/acme/api/pull/7"
    );
    let slack = &bodies(&server, "/slack").await[0];
    assert_eq!(slack["blocks"][2]["elements"][0]["type"], "button");
}

#[tokio::test]
async fn a_slow_webhook_times_out_without_naming_its_url() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(3)))
        .mount(&server)
        .await;
    let url = format!("{}/services/T000/B000/s3cr3t-token", server.uri());
    let sender = ChatSender::new(Secrets::fixed([("SLACK_HOOK", url.as_str())]))
        .with_timeout(Duration::from_millis(200));

    let started = Instant::now();
    let err = sender
        .send(&channel(ChatKind::Slack, "SLACK_HOOK"), &message())
        .await
        .unwrap_err();
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    assert!(err.contains("timed out"), "{err}");
    assert!(!err.contains("s3cr3t-token"), "the URL is a secret: {err}");
}

#[tokio::test]
async fn a_refused_post_or_a_missing_secret_is_an_error_not_a_panic() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(404).set_body_string("no_service"))
        .mount(&server)
        .await;
    let url = format!("{}/gone", server.uri());
    let sender = ChatSender::new(Secrets::fixed([
        ("GONE", url.as_str()),
        ("PLAIN", "http://hooks.slack.com/services/x"),
    ]));
    let err = sender
        .send(&channel(ChatKind::Teams, "GONE"), &message())
        .await
        .unwrap_err();
    assert!(err.contains("404"), "{err}");
    let err = sender
        .send(&channel(ChatKind::Slack, "MISSING"), &message())
        .await
        .unwrap_err();
    assert_eq!(err, "no secret MISSING");
    let err = sender
        .send(&channel(ChatKind::Slack, "PLAIN"), &message())
        .await
        .unwrap_err();
    assert!(err.contains("use https"), "{err}");
    assert!(!err.contains("hooks.slack.com"), "{err}");
}

#[tokio::test]
async fn no_channel_no_notifier() {
    // `from_section` reads nothing from the database without channels.
    let Some(db) = test_db().await else { return };
    let none = ChatNotifier::from_section(
        &ChatopsSection::default(),
        db.store.clone(),
        &Secrets::none(),
    )
    .await
    .unwrap();
    assert!(none.is_none());
}

/// Records what a run would (its cost, one passed gate), then ends so.
struct Spending {
    store: Store,
    usd: f64,
    end: Attempt,
}

impl JobRunner for Spending {
    async fn run(&self, claim: &Claim, _cancel: CancellationToken) -> Attempt {
        ran(&self.store, &claim.job, self.usd).await;
        self.end.clone()
    }
}

fn pr() -> Attempt {
    Attempt::Ended {
        outcome: Outcome::PrOpened,
        reason: None,
        pr_url: Some("https://github.com/acme/api/pull/9".into()),
        patch: None,
        diagnosis: None,
    }
}

async fn run_one(store: &Store, runner: Spending, notifier: ChatNotifier) {
    let shutdown = CancellationToken::new();
    let task = tokio::spawn(run_worker_notifying(
        store.clone(),
        Arc::new(runner),
        Arc::new(notifier),
        config(),
        shutdown.clone(),
    ));
    wait_ended(store).await;
    // The notifier runs before the attempt counts out; the drain waits.
    shutdown.cancel();
    task.await.unwrap();
}

#[tokio::test]
async fn a_worker_tells_its_channels_of_a_pr_and_the_budget_it_took_past_80() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let api = repo(s, "api").await;
    s.set_monthly_budget_by(api, Some(10.0), &Actor::cli())
        .await
        .unwrap();
    job(s, api, "acme/api#1", None).await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let (slack, teams) = (
        format!("{}/slack", server.uri()),
        format!("{}/teams", server.uri()),
    );
    let secrets = Secrets::fixed([
        ("SLACK_HOOK", slack.as_str()),
        ("TEAMS_HOOK", teams.as_str()),
    ]);
    let elsewhere = ChannelSection {
        repos: vec!["web".into()],
        ..channel(ChatKind::Teams, "TEAMS_HOOK")
    };
    let notifier = ChatNotifier::new(
        s.clone(),
        ChatSender::new(secrets),
        vec![channel(ChatKind::Slack, "SLACK_HOOK"), elsewhere],
    );

    let runner = Spending {
        store: s.clone(),
        usd: 8.5,
        end: pr(),
    };
    run_one(s, runner, notifier).await;

    let got = wait_requests(&server, "/slack", 2).await;
    let titles: Vec<&str> = got.iter().map(|b| b["text"].as_str().unwrap()).collect();
    assert_eq!(
        titles,
        [
            "aegis-code opened a pull request in api",
            "Monthly budget of repo api at 80%",
        ]
    );
    let fields = &got[0]["blocks"][1]["fields"];
    assert_eq!(fields[1]["text"], "*Cost*\n$8.50");
    assert_eq!(fields[3]["text"], "*Gate*\npassed (42s)");
    assert_eq!(
        got[0]["blocks"][2]["elements"][0]["url"],
        "https://github.com/acme/api/pull/9"
    );
    assert!(
        bodies(&server, "/teams").await.is_empty(),
        "kept to repo web"
    );
}

#[tokio::test]
async fn a_p1_failure_is_told_and_a_slow_webhook_does_not_fail_the_job() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let api = repo(s, "api").await;
    job(s, api, "acme/api#2", Some(SlaPriority::P1)).await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(3)))
        .mount(&server)
        .await;
    let url = format!("{}/teams", server.uri());
    let notifier = ChatNotifier::new(
        s.clone(),
        ChatSender::new(Secrets::fixed([("TEAMS_HOOK", url.as_str())]))
            .with_timeout(Duration::from_millis(300)),
        vec![channel(ChatKind::Teams, "TEAMS_HOOK")],
    );
    let runner = Spending {
        store: s.clone(),
        usd: 0.5,
        end: Attempt::Rejected(
            "the sandbox could not start: token ghp_0123456789abcdefghijklmnopqrstuvwxyz".into(),
        ),
    };
    let started = Instant::now();
    run_one(s, runner, notifier.clone()).await;
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );

    let jobs = wait_ended(s).await;
    assert_eq!(
        jobs[0].status,
        JobStatus::Failed,
        "the post timed out, the job did not"
    );
    let got = wait_requests(&server, "/teams", 1).await;
    let card = &got[0]["attachments"][0]["content"];
    assert_eq!(card["body"][0]["text"], "P1 job stopped in api");
    let summary = card["body"][1]["text"].as_str().unwrap();
    assert!(
        summary.starts_with("the sandbox could not start"),
        "{summary}"
    );
    assert!(!summary.contains("ghp_0123456789"), "masked: {summary}");

    // Told again directly: the post is bounded, whatever the webhook does.
    let started = Instant::now();
    notifier.job_ended(&jobs[0], None).await;
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
}

fn config() -> WorkerConfig {
    WorkerConfig {
        worker_id: "chatops-test".into(),
        max_workers: 2,
        lease: Duration::from_secs(5),
        renew_every: Duration::from_millis(500),
        poll_every: Duration::from_millis(50),
        drain: Duration::from_secs(5),
        retry_base: Duration::from_millis(20),
        max_cost_usd: 4.0,
    }
}

async fn repo(s: &Store, slug: &str) -> RepoId {
    s.create_repo(&NewRepo {
        slug: slug.into(),
        url: format!("https://github.com/acme/{slug}"),
        default_branch: "main".into(),
        language: "rust".into(),
        mode: ModeName::POC,
        max_concurrent_jobs: 2,
        config: json!({}),
    })
    .await
    .unwrap()
    .id
}

async fn job(s: &Store, repo: RepoId, issue_ref: &str, priority: Option<SlaPriority>) -> Job {
    s.create_job(&NewJob {
        repo_id: repo,
        issue_ref: issue_ref.into(),
        ledger_key: IssueId::parse(issue_ref.replace(['/', '#'], "-")).unwrap(),
        source: JobSource::Github,
        priority,
        sla_deadline: None,
    })
    .await
    .unwrap()
}

/// What `job` spent and how its gate went, as its run would record them.
async fn ran(s: &Store, job: &Job, usd: f64) {
    let ledger = s.ledger();
    let call = LedgerEvent::ModelCall {
        tier: Tier::Core,
        model: "m".into(),
        tokens: TokenCounts::default(),
        cost_usd: usd,
    };
    ledger.append(&job.ledger_key, &call).await.unwrap();
    let gate = LedgerEvent::GateRun(GateRun {
        passed: true,
        failed_step: None,
        duration_s: 42.0,
        sidecar_s: None,
    });
    ledger.append(&job.ledger_key, &gate).await.unwrap();
}

async fn wait_ended(s: &Store) -> Vec<Job> {
    let start = Instant::now();
    loop {
        let jobs = s
            .list_jobs(&JobFilter::default(), Page::first(50))
            .await
            .unwrap()
            .items;
        if jobs
            .iter()
            .all(|j| matches!(j.status, JobStatus::Done | JobStatus::Failed))
        {
            return jobs;
        }
        assert!(start.elapsed() < Duration::from_secs(10), "{jobs:#?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn wait_requests(server: &MockServer, at: &str, n: usize) -> Vec<Value> {
    let start = Instant::now();
    loop {
        let got = bodies(server, at).await;
        if got.len() >= n {
            return got;
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "{} of {n} at {at}",
            got.len()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
