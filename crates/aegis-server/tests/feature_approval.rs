//! #675: a plan waiting for approval is queued again by a maintainer, from
//! the API or the dashboard, or by `/aegis approve` on its issue from
//! someone who can write to the repo; a reviewer cannot, and a job not
//! waiting is refused. A feature's own budget is read and set from the API.

mod common;

use std::time::Duration;

use aegis_ledger::{IssueId, Outcome};
use aegis_server::api::router;
use aegis_store::testing::test_db;
use aegis_store::{
    AttemptEnd, Job, JobSource, ModeName, NewJob, NewRepo, Page, RepoId, RepoRole, Store,
};
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;

async fn repo(s: &Store) -> RepoId {
    s.create_repo(&NewRepo {
        slug: "shop".into(),
        url: "https://github.com/acme/shop".into(),
        default_branch: "main".into(),
        language: "rust".into(),
        mode: ModeName::POC,
        max_concurrent_jobs: 2,
        config: serde_json::json!({}),
    })
    .await
    .unwrap()
    .id
}

/// A job of `repo` run to `outcome`.
async fn ended(s: &Store, repo: RepoId, n: u32, outcome: Outcome) -> Job {
    s.create_job(&NewJob {
        repo_id: repo,
        issue_ref: format!("acme/shop#{n}"),
        ledger_key: IssueId::parse(format!("shop-{n}")).unwrap(),
        source: JobSource::Github,
        priority: None,
        sla_deadline: None,
    })
    .await
    .unwrap();
    let claim = s
        .claim_job("w", Duration::from_secs(60), 10)
        .await
        .unwrap()
        .unwrap();
    let end = AttemptEnd::Done {
        outcome,
        reason: None,
        pr_url: None,
        patch: None,
        diagnosis: None,
    };
    s.finish_attempt(&claim.run, "w", &end).await.unwrap()
}

async fn approve(app: &Router, token: &str, job: &Job) -> (StatusCode, Value) {
    let request = Request::post(format!("/api/v1/jobs/{}/approve", job.id.0))
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn a_maintainer_approves_a_waiting_plan_from_the_api() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let shop = repo(s).await;
    let plan = ended(s, shop, 1, Outcome::AwaitingApproval).await;
    let stopped = ended(s, shop, 2, Outcome::StoppedSteps).await;
    let app = router(s.clone(), common::auth(s));
    let reviewer = common::token(s, "reviewer-bot").await;
    common::grant(s, "reviewer-bot", shop, RepoRole::Reviewer).await;
    let maintainer = common::token(s, "lead-bot").await;
    common::grant(s, "lead-bot", shop, RepoRole::Maintainer).await;

    let (status, _) = approve(&app, &reviewer, &plan).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, body) = approve(&app, &maintainer, &stopped).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");

    let (status, body) = approve(&app, &maintainer, &plan).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["status"], "queued", "{body}");
    assert_eq!(s.approval(plan.id).await.unwrap().unwrap().by, "lead-bot");
    let log = s
        .audit_log(Some(shop), Page::first(20))
        .await
        .unwrap()
        .items;
    assert!(
        log.iter()
            .any(|e| e.action == "job.approve" && e.actor.name == "lead-bot")
    );
    let (status, _) = approve(&app, &maintainer, &plan).await;
    assert_eq!(status, StatusCode::CONFLICT, "approved once");
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

#[tokio::test]
async fn a_maintainer_approves_a_waiting_plan_from_the_dashboard() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let shop = repo(s).await;
    let plan = ended(s, shop, 1, Outcome::AwaitingApproval).await;
    let app = router(s.clone(), common::auth(s));
    let action = format!("/jobs/{}/approve", plan.id.0);

    let looker = common::sign_in(s, "rev@example.com").await;
    common::grant(s, "rev@example.com", shop, RepoRole::Reviewer).await;
    let seen = page(&app, &looker, &format!("/jobs/{}", plan.id.0)).await;
    assert!(
        !seen.contains(&action),
        "only a maintainer approves: {seen}"
    );
    let (status, _, _) = form(&app, &looker, &action).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let lead = common::sign_in(s, "lead@example.com").await;
    common::grant(s, "lead@example.com", shop, RepoRole::Maintainer).await;
    let seen = page(&app, &lead, &format!("/jobs/{}", plan.id.0)).await;
    assert!(seen.contains(&action), "{seen}");
    let (status, location, _) = form(&app, &lead, &action).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(location, format!("/jobs/{}?done=approved", plan.id.0));
    let seen = page(&app, &lead, &location).await;
    assert!(seen.contains("Plan approved"), "{seen}");
    assert!(!seen.contains(&action), "approved once");
    assert_eq!(
        s.approval(plan.id).await.unwrap().unwrap().by,
        "lead@example.com"
    );
    db.drop().await;
}

async fn call(
    app: &Router,
    token: &str,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
        .unwrap();
    let (status, _, body) = send(app, request).await;
    (status, serde_json::from_str(&body).unwrap_or(Value::Null))
}

#[tokio::test]
async fn a_features_budget_is_read_and_set_from_the_api() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let shop = repo(s).await;
    let plan = ended(s, shop, 1, Outcome::AwaitingApproval).await;
    let app = router(s.clone(), common::auth(s));
    let reviewer = common::token(s, "reviewer-bot").await;
    common::grant(s, "reviewer-bot", shop, RepoRole::Reviewer).await;
    let maintainer = common::token(s, "lead-bot").await;
    common::grant(s, "lead-bot", shop, RepoRole::Maintainer).await;
    let uri = format!("/api/v1/jobs/{}/epic-budget", plan.id.0);

    let (status, body) = call(&app, &reviewer, "GET", &uri, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["ceiling_usd"], Value::Null);
    assert_eq!(body["data"]["state"], "unlimited");
    let ceiling = serde_json::json!({ "ceiling_usd": 30.0 });
    let (status, _) = call(&app, &reviewer, "PUT", &uri, Some(ceiling.clone())).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, body) = call(&app, &maintainer, "PUT", &uri, Some(serde_json::json!({}))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "required: {body}");
    let (status, body) = call(&app, &maintainer, "PUT", &uri, Some(ceiling)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["ceiling_usd"], 30.0);
    assert_eq!(body["data"]["state"], "ok");
    let log = s
        .audit_log(Some(shop), Page::first(20))
        .await
        .unwrap()
        .items;
    assert!(
        log.iter()
            .any(|e| e.action == "job.epic_budget" && e.actor.name == "lead-bot")
    );
    db.drop().await;
}

const HOOK_SECRET: &str = "github-secret";

/// The webhook router, GitHub's API played by `github_api`.
fn hooks(s: &Store, github_api: &str) -> Router {
    use aegis_server::webhook::{
        COMMENT_TOKEN_ENV, DEFAULT_MAX_BODY_BYTES, GITHUB_SECRET_ENV, GitHubNotifier, WebhookConfig,
    };
    let secrets = aegis_secrets::Secrets::fixed([
        (GITHUB_SECRET_ENV, HOOK_SECRET),
        (COMMENT_TOKEN_ENV, "comment-token"),
    ]);
    aegis_server::webhook::router(
        s.clone(),
        WebhookConfig {
            notifier: Some(GitHubNotifier::new(github_api, secrets.clone())),
            secrets,
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

/// GitHub delivers `body` as `event`; the answer's `data`.
async fn deliver(app: &Router, event: &str, delivery: &str, body: &Value) -> (StatusCode, Value) {
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
        .header("x-github-event", event)
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

/// `text` commented on issue `number` of acme/shop by `login`.
fn commented(number: u64, text: &str, login: &str) -> Value {
    serde_json::json!({
        "action": "created",
        "comment": { "body": text },
        "issue": { "number": number, "state": "open",
                   "html_url": format!("https://github.com/acme/shop/issues/{number}") },
        "repository": { "full_name": "acme/shop", "html_url": "https://github.com/acme/shop" },
        "sender": { "login": login, "type": "User" }
    })
}

#[tokio::test]
async fn a_writer_approves_a_plan_on_its_issue() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let shop = repo(s).await;
    let github_api = MockServer::start().await;
    for (login, permission) in [("lan", "write"), ("eve", "read")] {
        Mock::given(method("GET"))
            .and(path(format!(
                "/repos/acme/shop/collaborators/{login}/permission"
            )))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "permission": permission })),
            )
            .mount(&github_api)
            .await;
    }
    // The refusal below is told on issue 2, once.
    Mock::given(method("POST"))
        .and(path("/repos/acme/shop/issues/2/comments"))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
            "id": 1, "html_url": "https://github.com/acme/shop/issues/2#issuecomment-1"
        })))
        .expect(1)
        .mount(&github_api)
        .await;
    let app = hooks(s, &github_api.uri());
    let plan = ended(s, shop, 1, Outcome::AwaitingApproval).await;
    let second = ended(s, shop, 2, Outcome::AwaitingApproval).await;

    let (_, data) = deliver(
        &app,
        "issue_comment",
        "a0",
        &commented(1, "/aegis approve", "eve"),
    )
    .await;
    assert_eq!(
        data["status"], "ignored",
        "a reader approves nothing: {data}"
    );
    let (_, data) = deliver(
        &app,
        "issue_comment",
        "a1",
        &commented(1, "LGTM, approve", "lan"),
    )
    .await;
    assert_eq!(data["status"], "ignored", "not the command: {data}");
    let (_, data) = deliver(
        &app,
        "issue_comment",
        "a2",
        &commented(9, "/aegis approve", "lan"),
    )
    .await;
    assert_eq!(data["status"], "ignored", "no plan there: {data}");
    assert_eq!(s.approval(plan.id).await.unwrap(), None);

    let text = "Looks right.\n/aegis approve mode=stacked_prs";
    let (status, data) = deliver(&app, "issue_comment", "a3", &commented(1, text, "lan")).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{data}");
    assert_eq!(data["status"], "approved", "{data}");
    assert_eq!(data["job_id"], plan.id.0);
    assert_eq!(s.approval(plan.id).await.unwrap().unwrap().by, "github:lan");
    let (_, data) = deliver(
        &app,
        "issue_comment",
        "a4",
        &commented(1, "/aegis approve", "lan"),
    )
    .await;
    assert_eq!(data["status"], "ignored", "approved once: {data}");

    // Another live job of the ticket: refused, and told on the issue.
    s.create_job(&NewJob {
        repo_id: shop,
        issue_ref: "acme/shop#2".into(),
        ledger_key: IssueId::parse("shop-2-cli").unwrap(),
        source: JobSource::Github,
        priority: None,
        sla_deadline: None,
    })
    .await
    .unwrap();
    let (status, data) = deliver(
        &app,
        "issue_comment",
        "a5",
        &commented(2, "/aegis approve", "lan"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{data}");
    assert_eq!(data["status"], "refused", "{data}");
    assert_eq!(data["job_id"], second.id.0);
    db.drop().await;
}
