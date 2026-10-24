//! Repos' monthly budgets (#31) against a real Postgres: a repo with less
//! than a job's cap left of its month gets no new job (API, webhook,
//! retry), its queued jobs wait with the reason until the month turns,
//! and only a maintainer or an admin changes the cap, audited.

mod common;

use std::time::{Duration, Instant};

use aegis_ledger::{IssueId, LedgerEvent, LedgerStore, Tier, TokenCounts};
use aegis_secrets::Secrets;
use aegis_server::api;
use aegis_server::webhook::{
    COMMENT_TOKEN_ENV, GITHUB_SECRET_ENV, GitHubNotifier, WebhookConfig, router,
};
use aegis_store::testing::test_db;
use aegis_store::{
    Actor, BUDGET_SPENT, BudgetRules, BudgetState, Job, JobFilter, JobSource, JobStatus, NewJob,
    NewRepo, Page, RepoId, RepoMode, RepoRole, Store, StoreError,
};
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use hmac::{Hmac, KeyInit, Mac};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sha2::Sha256;
use tower::ServiceExt;
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SECRET: &str = "github-secret";

async fn repo(s: &Store, slug: &str) -> RepoId {
    s.create_repo(&NewRepo {
        slug: slug.into(),
        url: format!("https://github.com/acme/{slug}"),
        default_branch: "main".into(),
        language: "rust".into(),
        mode: RepoMode::Poc,
        max_concurrent_jobs: 4,
        config: json!({}),
    })
    .await
    .unwrap()
    .id
}

async fn job(s: &Store, repo: RepoId, issue_ref: &str) -> Result<Job, StoreError> {
    let key = issue_ref.replace(['/', '#'], "-");
    s.create_job(&NewJob {
        repo_id: repo,
        issue_ref: issue_ref.into(),
        ledger_key: IssueId::parse(key).unwrap(),
        source: JobSource::Github,
    })
    .await
}

/// `usd` spent now in `job`'s ledger.
async fn spend(s: &Store, job: &Job, usd: f64) {
    let call = LedgerEvent::ModelCall {
        tier: Tier::Core,
        model: "m".into(),
        tokens: TokenCounts::default(),
        cost_usd: usd,
    };
    s.ledger().append(&job.ledger_key, &call).await.unwrap();
}

async fn send(app: &Router, request: Request<Body>) -> (StatusCode, Value) {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn call(method: &str, uri: &str, token: &str, body: Option<Value>) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
        .unwrap()
}

async fn budget_audits(s: &Store, repo: RepoId) -> Vec<(String, Value)> {
    s.audit_log(Some(repo), Page::first(50))
        .await
        .unwrap()
        .items
        .into_iter()
        .filter(|e| e.action == "repo.budget")
        .map(|e| (e.actor.name, e.detail))
        .collect()
}

#[tokio::test]
async fn a_spent_budget_refuses_new_jobs_with_the_reason() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let a = repo(s, "a").await;
    common::grant(s, "connector", a, RepoRole::Maintainer).await;
    let token = common::token(s, "connector").await;
    let app = api::router(s.clone(), common::auth(s));

    let (status, body) = send(
        &app,
        call(
            "PUT",
            &format!("/api/v1/repos/{}/budget", a.0),
            &token,
            Some(json!({ "monthly_usd": 10.0 })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["state"], "ok");

    // $6.50 spent: $3.50 left, less than one job's $4.
    let first = job(s, a, "acme/a#1").await.unwrap();
    spend(s, &first, 6.5).await;
    let create = json!({ "repo_id": a.0, "issue_ref": "acme/a#2", "source": "github" });
    let (status, body) = send(&app, call("POST", "/api/v1/jobs", &token, Some(create))).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "budget_spent");
    let message = body["error"]["message"].as_str().unwrap();
    assert!(message.starts_with(BUDGET_SPENT), "{message}");
    assert!(message.contains("$6.50 of $10.00"), "{message}");

    // A retry is a new job too.
    s.request_cancel(first.id).await.unwrap();
    let (status, body) = send(
        &app,
        call(
            "POST",
            &format!("/api/v1/jobs/{}/retry", first.id.0),
            &token,
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "budget_spent");

    let (status, body) = send(
        &app,
        call(
            "GET",
            &format!("/api/v1/repos/{}/budget", a.0),
            &token,
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["state"], "spent");
    assert_eq!(body["data"]["spent_usd"], 6.5);
    assert_eq!(body["data"]["left_usd"], 3.5);
    let jobs = s.list_jobs(&JobFilter::default(), Page::first(10)).await;
    assert_eq!(jobs.unwrap().items.len(), 1, "nothing was created");
    db.drop().await;
}

#[tokio::test]
async fn queued_jobs_wait_while_the_budget_is_spent_and_run_next_month() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let (a, b) = (repo(s, "a").await, repo(s, "b").await);
    s.set_monthly_budget_by(a, Some(10.0), &Actor::cli())
        .await
        .unwrap();
    // Queued while there was budget left.
    let first = job(s, a, "acme/a#1").await.unwrap();
    let second = job(s, a, "acme/a#2").await.unwrap();
    let other = job(s, b, "acme/b#1").await.unwrap();
    spend(s, &first, 7.0).await;

    let lease = Duration::from_secs(60);
    let claimed = s.claim_job("w", lease, 10).await.unwrap().unwrap();
    assert_eq!(claimed.job.id, other.id, "b has no cap and still runs");
    assert!(s.claim_job("w", lease, 10).await.unwrap().is_none());
    for id in [first.id, second.id] {
        let waiting = s.job(id).await.unwrap().unwrap();
        assert_eq!(waiting.status, JobStatus::Queued);
        let reason = waiting.reason.unwrap();
        assert!(
            reason.starts_with(&format!("waiting: {BUDGET_SPENT}")),
            "{reason}"
        );
        assert!(reason.contains("$7.00 of $10.00"), "{reason}");
    }

    // The month turns: what was spent is last month's.
    db.raw("UPDATE ledger_events SET recorded_at = recorded_at - interval '40 days'")
        .await
        .unwrap();
    let budget = s.repo_budget(a).await.unwrap().unwrap();
    assert_eq!(budget.spent_usd, 0.0);
    assert_eq!(budget.state(s.budget_rules()), BudgetState::Ok);
    let claimed = s.claim_job("w", lease, 10).await.unwrap().unwrap();
    assert_eq!(claimed.job.id, first.id);
    assert_eq!(claimed.job.reason, None, "the wait is over");
    assert!(job(s, a, "acme/a#3").await.is_ok());
    db.drop().await;
}

#[tokio::test]
async fn the_month_begins_in_the_configured_time_zone() {
    let Some(db) = test_db().await else { return };
    let rules = |time_zone: &str| BudgetRules {
        time_zone: time_zone.into(),
        ..BudgetRules::default()
    };
    let err = db
        .store
        .clone()
        .with_budget(rules("Mars/Olympus"))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("time zone"), "{err}");
    let s = db
        .store
        .clone()
        .with_budget(rules("Asia/Tokyo"))
        .await
        .unwrap();
    let a = repo(&s, "a").await;
    let j = job(&s, a, "acme/a#1").await.unwrap();
    spend(&s, &j, 1.0).await;
    // 08:00 on the 1st in Tokyo is still last month in UTC (23:00).
    db.raw(
        "UPDATE ledger_events SET recorded_at =
             (date_trunc('month', now() AT TIME ZONE 'Asia/Tokyo') + interval '8 hours')
             AT TIME ZONE 'Asia/Tokyo'",
    )
    .await
    .unwrap();
    assert_eq!(s.repo_budget(a).await.unwrap().unwrap().spent_usd, 1.0);
    let utc = db.store.clone().with_budget(rules("UTC")).await.unwrap();
    assert_eq!(utc.repo_budget(a).await.unwrap().unwrap().spent_usd, 0.0);
    db.drop().await;
}

#[tokio::test]
async fn only_a_maintainer_or_an_admin_changes_the_cap() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let (a, b) = (repo(s, "a").await, repo(s, "b").await);
    common::grant(s, "dashboard", a, RepoRole::Viewer).await;
    common::grant(s, "connector", a, RepoRole::Maintainer).await;
    common::admin(s, "ops").await;
    let viewer = common::token(s, "dashboard").await;
    let maintainer = common::token(s, "connector").await;
    let admin = common::token(s, "ops").await;
    let app = api::router(s.clone(), common::auth(s));
    let put = |repo: RepoId, token: &str, body: Value| {
        call(
            "PUT",
            &format!("/api/v1/repos/{}/budget", repo.0),
            token,
            Some(body),
        )
    };

    let (status, body) = send(&app, put(a, &viewer, json!({ "monthly_usd": 1000.0 }))).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let (status, _) = send(
        &app,
        call(
            "GET",
            &format!("/api/v1/repos/{}/budget", a.0),
            &viewer,
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "a viewer reads it");
    // A repo the maintainer has no role in does not exist for them.
    let (status, _) = send(&app, put(b, &maintainer, json!({ "monthly_usd": 1.0 }))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(s.repo_budget(a).await.unwrap().unwrap().monthly_usd, None);
    assert!(budget_audits(s, a).await.is_empty());

    for bad in [json!({}), json!({ "monthly_usd": -1.0 })] {
        let (status, body) = send(&app, put(a, &maintainer, bad.clone())).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{bad}: {body}");
    }
    let (status, body) = send(&app, put(a, &maintainer, json!({ "monthly_usd": 50.0 }))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = send(&app, put(b, &admin, json!({ "monthly_usd": 20.0 }))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = send(&app, put(a, &admin, json!({ "monthly_usd": null }))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["state"], "unlimited");

    // Newest first.
    assert_eq!(
        budget_audits(s, a).await,
        vec![
            ("ops".into(), json!({ "monthly_usd": null, "was": 50.0 })),
            (
                "connector".into(),
                json!({ "monthly_usd": 50.0, "was": null })
            ),
        ]
    );
    assert_eq!(budget_audits(s, b).await.len(), 1);
    db.drop().await;
}

fn sign(body: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(SECRET.as_bytes()).unwrap();
    mac.update(body);
    let hex: String = mac
        .finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("sha256={hex}")
}

#[tokio::test]
async fn a_webhook_job_refused_for_the_budget_is_told_on_the_issue() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let a = repo(s, "a").await;
    s.set_monthly_budget_by(a, Some(2.0), &Actor::cli())
        .await
        .unwrap();
    let github_api = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/repos/acme/a/issues/7/comments"))
        .and(body_string_contains("monthly budget"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "id": 1, "html_url": "https://github.com/acme/a/issues/7#issuecomment-1"
        })))
        .expect(1)
        .mount(&github_api)
        .await;
    let config = WebhookConfig {
        secrets: Secrets::fixed([(GITHUB_SECRET_ENV, SECRET)]),
        label: "aegis".into(),
        burst: 10,
        per_minute: 60,
        notifier: Some(GitHubNotifier::new(
            &github_api.uri(),
            Secrets::fixed([(COMMENT_TOKEN_ENV, "comment-token")]),
        )),
    };
    let app = router(s.clone(), config);
    let body = json!({
        "action": "labeled",
        "label": { "name": "aegis" },
        "issue": { "number": 7, "title": "t", "body": "b" },
        "repository": { "full_name": "acme/a", "html_url": "https://github.com/acme/a" }
    });
    let signature = sign(body.to_string().as_bytes());
    let request = Request::post("/github")
        .header("x-github-event", "issues")
        .header("x-github-delivery", "d-1")
        .header("x-hub-signature-256", signature)
        .body(Body::from(body.to_string()))
        .unwrap();
    let (status, reply) = send(&app, request).await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_eq!(reply["data"]["status"], "refused");

    let jobs = s.list_jobs(&JobFilter::default(), Page::first(10)).await;
    let jobs = jobs.unwrap().items;
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].status, JobStatus::Cancelled);
    assert!(jobs[0].reason.as_deref().unwrap().starts_with(BUDGET_SPENT));
    assert!(
        s.claim_job("w", Duration::from_secs(60), 1)
            .await
            .unwrap()
            .is_none()
    );

    let start = Instant::now();
    while github_api
        .received_requests()
        .await
        .unwrap_or_default()
        .is_empty()
    {
        assert!(start.elapsed() < Duration::from_secs(10), "no comment came");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    github_api.verify().await;
    db.drop().await;
}
