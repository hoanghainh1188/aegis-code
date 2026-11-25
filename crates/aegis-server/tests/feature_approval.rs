//! #675: a plan waiting for approval is queued again by a maintainer, from
//! the API; a reviewer cannot, and a job not waiting is refused.

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
use axum::http::{Request, StatusCode};
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
