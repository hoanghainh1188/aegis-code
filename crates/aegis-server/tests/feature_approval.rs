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
    AttemptEnd, Job, JobSource, JobStatus, ModeName, NewJob, NewRepo, Page, RepoId, RepoRole, Store,
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
        // One revision of a plan at most.
        config: serde_json::json!({ "feature_lane": { "max_revisions": 1 } }),
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
    s.finish_attempt(&claim.run, "w", &done(outcome))
        .await
        .unwrap()
}

fn done(outcome: Outcome) -> AttemptEnd {
    AttemptEnd::Done {
        outcome,
        reason: None,
        pr_url: None,
        patch: None,
        diagnosis: None,
    }
}

async fn approve(app: &Router, token: &str, job: &Job) -> (StatusCode, Value) {
    approve_as(app, token, job, "").await
}

async fn approve_as(app: &Router, token: &str, job: &Job, query: &str) -> (StatusCode, Value) {
    let request = Request::post(format!("/api/v1/jobs/{}/approve{query}", job.id.0))
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
    let sent_back = ended(s, shop, 3, Outcome::AwaitingApproval).await;
    let app = router(s.clone(), common::auth(s));
    let reviewer = common::token(s, "reviewer-bot").await;
    common::grant(s, "reviewer-bot", shop, RepoRole::Reviewer).await;
    let maintainer = common::token(s, "lead-bot").await;
    common::grant(s, "lead-bot", shop, RepoRole::Maintainer).await;

    let (status, _) = approve(&app, &reviewer, &plan).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, body) = approve(&app, &maintainer, &stopped).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");

    // A mode the platform does not know is refused; the approval keeps
    // the job waiting, approved, for its sub-jobs (#678).
    let (status, body) = approve_as(&app, &maintainer, &plan, "?mode=auto").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let (status, body) = approve_as(&app, &maintainer, &plan, "?mode=integration_pr").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["status"], "done", "{body}");
    let approval = s.approval(plan.id).await.unwrap().unwrap();
    assert_eq!(
        (approval.by.as_str(), approval.delivery_mode),
        ("lead-bot", Some(aegis_store::DeliveryMode::IntegrationPr))
    );
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

    // Sent back for a revision (#677), within the repo's cap.
    let revise = |job: &Job, feedback: &str| {
        (
            format!("/api/v1/jobs/{}/revise", job.id.0),
            serde_json::json!({ "feedback": feedback }),
        )
    };
    let (uri, body) = revise(&sent_back, "Fewer sub-jobs.");
    let (status, _) = call(&app, &reviewer, "POST", &uri, Some(body.clone())).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = call(
        &app,
        &maintainer,
        "POST",
        &uri,
        Some(serde_json::json!({ "feedback": " " })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, answer) = call(&app, &maintainer, "POST", &uri, Some(body.clone())).await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    assert_eq!(answer["data"]["status"], "queued", "{answer}");
    let (status, _) = call(&app, &maintainer, "POST", &uri, Some(body)).await;
    assert_eq!(status, StatusCode::CONFLICT, "it no longer waits");
    let (uri, body) = revise(&plan, "Too late.");
    let (status, _) = call(&app, &maintainer, "POST", &uri, Some(body)).await;
    assert_eq!(status, StatusCode::CONFLICT, "approved already");
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
    // Each refusal below is told on its issue, once.
    for n in [1, 2, 3] {
        Mock::given(method("POST"))
            .and(path(format!("/repos/acme/shop/issues/{n}/comments")))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "id": n, "html_url": format!("https://github.com/acme/shop/issues/{n}#c")
            })))
            .expect(1)
            .mount(&github_api)
            .await;
    }
    let app = hooks(s, &github_api.uri());
    let plan = ended(s, shop, 1, Outcome::AwaitingApproval).await;
    let second = ended(s, shop, 2, Outcome::AwaitingApproval).await;
    let third = ended(s, shop, 3, Outcome::AwaitingApproval).await;
    let say = |n: u64, text: &str, login: &str| commented(n, text, login);

    let (_, data) = deliver(
        &app,
        "issue_comment",
        "a0",
        &say(1, "/aegis approve", "eve"),
    )
    .await;
    assert_eq!(
        data["status"], "ignored",
        "a reader approves nothing: {data}"
    );
    let (_, data) = deliver(&app, "issue_comment", "a1", &say(1, "LGTM, approve", "lan")).await;
    assert_eq!(data["status"], "ignored", "not the command: {data}");
    let (_, data) = deliver(
        &app,
        "issue_comment",
        "a2",
        &say(9, "/aegis approve", "lan"),
    )
    .await;
    assert_eq!(data["status"], "ignored", "no plan there: {data}");
    assert_eq!(s.approval(plan.id).await.unwrap(), None);

    let text = "Looks right.\n/aegis approve mode=stacked_prs";
    let (status, data) = deliver(&app, "issue_comment", "a3", &say(1, text, "lan")).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{data}");
    assert_eq!(data["status"], "approved", "{data}");
    assert_eq!(data["job_id"], plan.id.0);
    let approval = s.approval(plan.id).await.unwrap().unwrap();
    assert_eq!(
        (approval.by.as_str(), approval.delivery_mode),
        ("github:lan", Some(aegis_store::DeliveryMode::StackedPrs))
    );
    // Approved once: told on issue 1.
    let (_, data) = deliver(
        &app,
        "issue_comment",
        "a4",
        &say(1, "/aegis approve", "lan"),
    )
    .await;
    assert_eq!(data["status"], "refused", "{data}");

    // Sent back with feedback: the job plans again.
    let (_, data) = deliver(&app, "issue_comment", "r0", &say(2, "/aegis revise", "lan")).await;
    assert_eq!(data["status"], "ignored", "no feedback: {data}");
    let (_, data) = deliver(
        &app,
        "issue_comment",
        "r1",
        &say(2, "/aegis revise Split it", "eve"),
    )
    .await;
    assert_eq!(
        data["status"], "ignored",
        "a reader revises nothing: {data}"
    );
    let feedback = "/aegis revise Keep the export out of it.\nOne sub-job is enough.";
    let (status, data) = deliver(&app, "issue_comment", "r2", &say(2, feedback, "lan")).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{data}");
    assert_eq!(data["status"], "revising", "{data}");
    assert_eq!(data["job_id"], second.id.0);
    let asked = s.revision_request(second.id).await.unwrap().unwrap();
    assert_eq!(
        (asked.revision, asked.feedback.as_str(), asked.by.as_str()),
        (
            1,
            "Keep the export out of it.\nOne sub-job is enough.",
            "github:lan"
        )
    );
    assert_eq!(
        s.job(second.id).await.unwrap().unwrap().status,
        JobStatus::Queued
    );

    // The revised plan waits again; the repo's cap is one revision:
    // refused, and told on issue 2.
    let rerun = s
        .claim_job("w", Duration::from_secs(60), 10)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(rerun.job.id, second.id);
    s.finish_attempt(&rerun.run, "w", &done(Outcome::AwaitingApproval))
        .await
        .unwrap();
    let (_, data) = deliver(
        &app,
        "issue_comment",
        "r3",
        &say(2, "/aegis revise Again", "lan"),
    )
    .await;
    assert_eq!(data["status"], "refused", "{data}");
    assert!(
        data["reason"].as_str().unwrap().contains("revised 1 times"),
        "{data}"
    );

    // An unknown mode: refused, and told on issue 3.
    let (_, data) = deliver(
        &app,
        "issue_comment",
        "a5",
        &say(3, "/aegis approve mode=yolo", "lan"),
    )
    .await;
    assert_eq!(data["status"], "refused", "{data}");
    assert!(
        data["reason"].as_str().unwrap().contains("mode `yolo`"),
        "{data}"
    );
    assert_eq!(data["job_id"], third.id.0);
    db.drop().await;
}

/// The plan Core wrote for a two-part feature, as the worker keeps it.
fn two_part_plan(delivery: &str) -> Value {
    serde_json::json!({
        "summary": "Refunds of a charge.",
        "criteria": [{"given": "a charge", "when": "it is refunded", "then": "the refund never exceeds it"}],
        "contracts": ["fn refund(charge: &Charge, cents: u64) -> Result<Refund>"],
        "dependencies": [],
        "sub_jobs": [
            {"id": "model", "title": "The refund model", "scope": ["src/refund.rs"], "depends_on": []},
            {"id": "api", "title": "The refund endpoint", "scope": ["src/api.rs"], "depends_on": ["model"]}
        ],
        "delivery": delivery
    })
}

/// #678: approving a plan delivered as stacked PRs queues its sub-jobs,
/// each on the one before.
#[tokio::test]
async fn an_approved_stacked_plan_queues_its_sub_jobs() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let shop = repo(s).await;
    let stacked = ended(s, shop, 1, Outcome::AwaitingApproval).await;
    s.save_feature_plan(stacked.id, &two_part_plan("integration_pr"))
        .await
        .unwrap();
    let app = router(s.clone(), common::auth(s));
    let maintainer = common::token(s, "lead-bot").await;
    common::grant(s, "lead-bot", shop, RepoRole::Maintainer).await;

    // The person's mode wins over the plan's recommendation.
    let (status, body) = approve_as(&app, &maintainer, &stacked, "?mode=stacked_prs").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let subs = s.sub_jobs(stacked.id).await.unwrap();
    assert_eq!(subs.len(), 2);
    assert!(subs.iter().all(|j| j.status == JobStatus::Queued));
    assert_eq!(
        s.plan_key(subs[0].id).await.unwrap().as_deref(),
        Some("model")
    );
    assert_eq!(
        s.plan_key(subs[1].id).await.unwrap().as_deref(),
        Some("api")
    );
    assert_eq!(
        s.job_links(subs[1].id).await.unwrap().depends_on,
        vec![subs[0].id]
    );
    assert_eq!(subs[0].issue_ref, "acme/shop#1");

    // What each runs: its part, the first from the main branch, the
    // second on the first's PR once it is open.
    use aegis_server::feature_gate::sub_job_task;
    let first = sub_job_task(s, &subs[0]).await.unwrap().unwrap();
    assert_eq!(first.sub_job, "model");
    assert_eq!(
        first.delivery,
        aegis_lifecycle::FeatureDeliveryMode::StackedPrs
    );
    assert_eq!(first.stacked_on, None);
    assert_eq!(first.plan.sub_jobs.len(), 2);
    let claim = s
        .claim_job("w", Duration::from_secs(60), 10)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claim.job.id, subs[0].id);
    s.finish_attempt(
        &claim.run,
        "w",
        &AttemptEnd::Done {
            outcome: Outcome::PrOpened,
            reason: None,
            pr_url: Some("https://github.com/acme/shop/pull/9".into()),
            patch: None,
            diagnosis: None,
        },
    )
    .await
    .unwrap();
    let second = sub_job_task(s, &subs[1]).await.unwrap().unwrap();
    assert_eq!(second.sub_job, "api");
    let on = second.stacked_on.unwrap();
    assert_eq!(
        on.branch,
        format!("agent/{}", subs[0].ledger_key.as_str().to_ascii_lowercase())
    );
    assert_eq!(
        on.pr_url.as_deref(),
        Some("https://github.com/acme/shop/pull/9")
    );
    // The planning job, and a sub-job made by hand, run as they did.
    assert!(sub_job_task(s, &stacked).await.unwrap().is_none());
    let by_hand = s
        .create_sub_job_by(
            stacked.id,
            &[],
            &NewJob {
                repo_id: shop,
                issue_ref: "acme/shop#1".into(),
                ledger_key: IssueId::parse("shop-1-hand").unwrap(),
                source: JobSource::Github,
                priority: None,
                sla_deadline: None,
            },
            &aegis_store::Actor::system(),
        )
        .await
        .unwrap();
    assert!(sub_job_task(s, &by_hand).await.unwrap().is_none());

    db.drop().await;
}

/// #678: approving a plan delivered as one integration PR queues its
/// sub-jobs as its DAG says, each merged into the feature's own branch;
/// once every part is in, the plan is queued for the feature's PR.
#[tokio::test]
async fn an_approved_integration_plan_merges_its_parts_then_opens_one_pr() {
    use aegis_server::feature_gate::{feature_delivery, sub_job_task};
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let shop = repo(s).await;
    let root = ended(s, shop, 2, Outcome::AwaitingApproval).await;
    s.save_feature_plan(root.id, &two_part_plan("integration_pr"))
        .await
        .unwrap();
    let app = router(s.clone(), common::auth(s));
    let maintainer = common::token(s, "lead-bot").await;
    common::grant(s, "lead-bot", shop, RepoRole::Maintainer).await;
    let (status, body) = approve(&app, &maintainer, &root).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let subs = s.sub_jobs(root.id).await.unwrap();
    assert_eq!(subs.len(), 2);
    // `api` depends on `model`, as the plan says.
    assert_eq!(
        s.job_links(subs[1].id).await.unwrap().depends_on,
        vec![subs[0].id]
    );
    let task = sub_job_task(s, &subs[1]).await.unwrap().unwrap();
    assert_eq!(
        task.delivery,
        aegis_lifecycle::FeatureDeliveryMode::IntegrationPr
    );
    assert_eq!(task.integration_branch.as_deref(), Some("agent/shop-2"));
    assert_eq!(task.stacked_on, None);
    assert!(feature_delivery(s, &root).await.unwrap().is_none());

    for sub in &subs {
        let claim = s
            .claim_job("w", Duration::from_secs(60), 10)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(claim.job.id, sub.id);
        s.finish_attempt(&claim.run, "w", &done(Outcome::Integrated))
            .await
            .unwrap();
    }
    let queued = s.job(root.id).await.unwrap().unwrap();
    assert_eq!(queued.status, JobStatus::Queued);
    let delivery = feature_delivery(s, &root).await.unwrap().unwrap();
    assert_eq!(delivery.branch, "agent/shop-2");
    assert_eq!(delivery.plan.sub_jobs.len(), 2);
    // A sub-job, or a plan delivered as stacked PRs, opens no such PR.
    assert!(feature_delivery(s, &subs[0]).await.unwrap().is_none());
    db.drop().await;
}
