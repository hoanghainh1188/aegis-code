//! #706 (3/3): an error spike's report reaches people, and they turn it
//! into a fix. For a repo whose reports go to GitHub, the spike's
//! diagnosis opens an issue with the report; `/aegis fix` there, from
//! someone who can write to the repo, queues the fix job, whose issue is
//! the spike with its diagnosis and whose end is told on that issue. On
//! the dashboard, a maintainer asks for the same fix from the job's page.

mod common;

use std::time::Duration;

use aegis_ledger::{IssueId, IssueKind, Outcome};
use aegis_secrets::Secrets;
use aegis_server::anomaly::apm_issue;
use aegis_server::api::router;
use aegis_server::webhook::{
    COMMENT_TOKEN_ENV, DEFAULT_MAX_BODY_BYTES, GITHUB_SECRET_ENV, GitHubNotifier, WebhookConfig,
};
use aegis_server::worker::JobNotifier;
use aegis_store::testing::test_db;
use aegis_store::{
    AnomalyLimits, AttemptEnd, Job, JobSource, JobStatus, ModeName, NewAnomaly, NewJob, NewRepo,
    Repo, RepoRole, Store,
};
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const HOOK_SECRET: &str = "github-secret";
const REPORT: &str = "**Diagnosis**: code_bug (confidence 0.80). Read-only: nothing was changed.";
const LIMITS: AnomalyLimits = AnomalyLimits {
    dedupe: Duration::from_secs(3600),
    per_day: 10,
};

fn secrets() -> Secrets {
    Secrets::fixed([
        (GITHUB_SECRET_ENV, HOOK_SECRET),
        (COMMENT_TOKEN_ENV, "comment-token"),
    ])
}

async fn repo(s: &Store, report_to: &str) -> Repo {
    s.create_repo(&NewRepo {
        slug: "shop".into(),
        url: "https://github.com/acme/shop".into(),
        default_branch: "main".into(),
        language: "rust".into(),
        mode: ModeName::POC,
        max_concurrent_jobs: 2,
        config: json!({ "apm_anomaly": { "report_to": report_to } }),
    })
    .await
    .unwrap()
}

/// Spike `name` of `repo`, queued, run and diagnosed.
async fn diagnosed(s: &Store, repo: &Repo, name: &str) -> Job {
    let job = NewJob {
        repo_id: repo.id,
        issue_ref: name.into(),
        ledger_key: IssueId::parse(format!("shop-{name}")).unwrap(),
        source: JobSource::Apm,
        priority: None,
        sla_deadline: None,
    };
    let spike = NewAnomaly {
        fingerprint: format!("sentry:{name}"),
        title: "Error spike: TypeError: total is undefined".into(),
        body: "An error spike found in sentry before any ticket (#706).".into(),
    };
    s.deliver_anomaly(&job, &spike, LIMITS)
        .await
        .unwrap()
        .unwrap();
    let claim = s
        .claim_job("w", Duration::from_secs(60), 10)
        .await
        .unwrap()
        .unwrap();
    let end = AttemptEnd::Done {
        outcome: Outcome::Diagnosed,
        reason: None,
        pr_url: None,
        patch: None,
        diagnosis: Some(REPORT.into()),
    };
    s.finish_attempt(&claim.run, "w", &end).await.unwrap()
}

fn hooks(s: &Store, github_api: &str) -> Router {
    aegis_server::webhook::router(
        s.clone(),
        WebhookConfig {
            notifier: Some(GitHubNotifier::new(github_api, secrets())),
            secrets: secrets(),
            label: "aegis".into(),
            burst: 100,
            per_minute: 600,
            gitlab_notifier: None,
            bitbucket_notifier: None,
            bitbucket_dc_notifier: None,
            azure_devops_notifier: None,
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
            modes: None,
            app: None,
        },
    )
}

async fn deliver(app: &Router, delivery: &str, body: &Value) -> (StatusCode, Value) {
    use hmac::{Hmac, KeyInit, Mac};
    let body = body.to_string();
    let mut mac = Hmac::<sha2::Sha256>::new_from_slice(HOOK_SECRET.as_bytes()).unwrap();
    mac.update(body.as_bytes());
    let hex: String = mac
        .finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let request = Request::builder()
        .method("POST")
        .uri("/github")
        .header("x-github-event", "issue_comment")
        .header("x-github-delivery", delivery)
        .header("x-hub-signature-256", format!("sha256={hex}"))
        .body(Body::from(body))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let answer: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, answer["data"].clone())
}

fn commented(number: u64, text: &str, login: &str) -> Value {
    json!({
        "action": "created",
        "comment": { "body": text },
        "issue": { "number": number, "state": "open",
                   "html_url": format!("https://github.com/acme/shop/issues/{number}") },
        "repository": { "full_name": "acme/shop", "html_url": "https://github.com/acme/shop" },
        "sender": { "login": login, "type": "User" }
    })
}

#[tokio::test]
async fn a_spike_report_opens_an_issue_where_a_writer_asks_for_the_fix() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let shop = repo(s, "github").await;
    let github = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/repos/acme/shop/issues"))
        .and(body_string_contains("Error spike: TypeError"))
        .and(body_string_contains("code_bug (confidence 0.80)"))
        .and(body_string_contains("/aegis fix"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "number": 42, "html_url": "https://github.com/acme/shop/issues/42"
        })))
        .expect(1)
        .mount(&github)
        .await;
    for (login, permission) in [("lan", "write"), ("eve", "read")] {
        Mock::given(method("GET"))
            .and(path(format!(
                "/repos/acme/shop/collaborators/{login}/permission"
            )))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({ "permission": permission })),
            )
            .mount(&github)
            .await;
    }
    // The fix job's end is told on the spike's issue, once.
    Mock::given(method("POST"))
        .and(path("/repos/acme/shop/issues/42/comments"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "id": 1, "html_url": "https://github.com/acme/shop/issues/42#c1"
        })))
        .expect(1)
        .mount(&github)
        .await;

    let spike = diagnosed(s, &shop, "apm-1").await;
    let notifier = GitHubNotifier::new(&github.uri(), secrets()).with_app(None, s.clone());
    notifier.job_ended(&spike, Some(REPORT)).await;
    assert_eq!(
        s.anomaly_ticket(shop.id, "apm-1").await.unwrap().as_deref(),
        Some("acme/shop#42")
    );

    let app = hooks(s, &github.uri());
    let (_, data) = deliver(&app, "f0", &commented(42, "/aegis fix", "eve")).await;
    assert_eq!(data["status"], "ignored", "a reader fixes nothing: {data}");
    let (_, data) = deliver(&app, "f1", &commented(7, "/aegis fix", "lan")).await;
    assert_eq!(data["status"], "ignored", "not a spike's issue: {data}");
    let (status, data) = deliver(&app, "f2", &commented(42, "Real.\n/aegis fix", "lan")).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{data}");
    assert_eq!(data["status"], "queued", "{data}");
    let fix = s
        .job(aegis_store::JobId(data["job_id"].as_i64().unwrap()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (fix.source, fix.status, fix.issue_ref.as_str()),
        (JobSource::Apm, JobStatus::Queued, "apm-1-fix")
    );
    let (_, data) = deliver(&app, "f3", &commented(42, "/aegis fix", "lan")).await;
    assert_eq!(data["status"], "refused", "one fix at a time: {data}");

    // The fix job works on the spike with its diagnosis, linked to the issue.
    let issue = apm_issue(&fix, &shop, s).await.unwrap();
    assert_eq!(issue.kind, Some(IssueKind::Bug));
    assert_eq!(
        issue.title,
        "Fix: Error spike: TypeError: total is undefined"
    );
    assert!(issue.body.contains(REPORT), "{}", issue.body);
    assert_eq!(issue.url, "https://github.com/acme/shop/issues/42");
    assert_eq!(issue.id, fix.ledger_key.as_str());

    // Its end is told on that issue; no second issue is opened.
    let claim = s
        .claim_job("w", Duration::from_secs(60), 10)
        .await
        .unwrap()
        .unwrap();
    let end = AttemptEnd::Done {
        outcome: Outcome::PrOpened,
        reason: None,
        pr_url: Some("https://github.com/acme/shop/pull/43".into()),
        patch: None,
        diagnosis: None,
    };
    let ended = s.finish_attempt(&claim.run, "w", &end).await.unwrap();
    notifier.job_ended(&ended, None).await;
    db.drop().await;
}

#[tokio::test]
async fn a_dashboard_spike_opens_no_issue_and_a_maintainer_asks_for_its_fix() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let shop = repo(s, "dashboard").await;
    let github = MockServer::start().await;
    let spike = diagnosed(s, &shop, "apm-2").await;
    GitHubNotifier::new(&github.uri(), secrets())
        .with_app(None, s.clone())
        .job_ended(&spike, Some(REPORT))
        .await;
    assert!(github.received_requests().await.unwrap().is_empty());
    assert_eq!(s.anomaly_ticket(shop.id, "apm-2").await.unwrap(), None);

    let app = router(s.clone(), common::auth(s));
    let action = format!("/jobs/{}/fix", spike.id.0);
    let looker = common::sign_in(s, "rev@example.com").await;
    common::grant(s, "rev@example.com", shop.id, RepoRole::Reviewer).await;
    assert!(
        !page(&app, &looker, &format!("/jobs/{}", spike.id.0))
            .await
            .contains(&action)
    );
    assert_eq!(form(&app, &looker, &action).await.0, StatusCode::FORBIDDEN);

    let lead = common::sign_in(s, "lead@example.com").await;
    common::grant(s, "lead@example.com", shop.id, RepoRole::Maintainer).await;
    let seen = page(&app, &lead, &format!("/jobs/{}", spike.id.0)).await;
    assert!(seen.contains(&action), "{seen}");
    let (status, location, _) = form(&app, &lead, &action).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert!(location.ends_with("?done=fix_asked"), "{location}");
    let seen = page(&app, &lead, &location).await;
    assert!(seen.contains("Fix asked for"), "{seen}");
    assert!(!seen.contains("/fix\""), "a fix job offers no fix: {seen}");
    db.drop().await;
}

async fn send(app: &Router, request: Request<Body>) -> (StatusCode, String, String) {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let location = response
        .headers()
        .get(header::LOCATION)
        .map(|l| l.to_str().unwrap().to_owned())
        .unwrap_or_default();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, location, String::from_utf8(bytes.to_vec()).unwrap())
}

async fn page(app: &Router, who: &common::Browser, uri: &str) -> String {
    let request = Request::get(uri)
        .header(header::HOST, "localhost:8080")
        .header(header::COOKIE, &who.cookie)
        .body(Body::empty())
        .unwrap();
    send(app, request).await.2
}

async fn form(app: &Router, who: &common::Browser, uri: &str) -> (StatusCode, String, String) {
    let request = Request::post(uri)
        .header(header::HOST, "localhost:8080")
        .header(header::ORIGIN, "http://localhost:8080")
        .header(header::COOKIE, &who.cookie)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(format!("_csrf={}", who.csrf)))
        .unwrap();
    send(app, request).await
}
