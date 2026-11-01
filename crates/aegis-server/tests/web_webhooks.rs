//! #373: an admin sees on a repo's page what became of its recent
//! webhooks (#331's record): the job a delivery made or found, with a
//! link, the deliveries received before, and those that made no job with
//! why and what to do about it. Never a delivery's content.

mod common;

use aegis_ledger::IssueId;
use aegis_server::api::router;
use aegis_store::testing::test_db;
use aegis_store::{
    JobId, JobSource, NewJob, NewRepo, NewWebhookEvent, RepoId, RepoMode, RepoRole, Store,
    WebhookOutcome, WebhookSource,
};
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::Browser;
use http_body_util::BodyExt;
use serde_json::json;
use tower::ServiceExt;

async fn get(app: &Router, who: &Browser, uri: &str) -> (StatusCode, String) {
    let request = Request::get(uri)
        .header(header::HOST, "localhost:8080")
        .header(header::COOKIE, &who.cookie)
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

async fn repo(s: &Store, slug: &str) -> RepoId {
    s.create_repo(&NewRepo {
        slug: slug.into(),
        url: format!("https://github.com/x/{slug}"),
        default_branch: "main".into(),
        language: "rust".into(),
        mode: RepoMode::Poc,
        max_concurrent_jobs: 2,
        config: json!({}),
    })
    .await
    .unwrap()
    .id
}

async fn job(s: &Store, repo: RepoId, n: u32) -> JobId {
    s.create_job(&NewJob {
        repo_id: repo,
        issue_ref: format!("x/y#{n}"),
        ledger_key: IssueId::parse(format!("y-{n}")).unwrap(),
        source: JobSource::Github,
    })
    .await
    .unwrap()
    .id
}

async fn hook(
    s: &Store,
    repo: Option<RepoId>,
    delivery: &str,
    outcome: WebhookOutcome,
    reason: Option<&str>,
    job: Option<JobId>,
) {
    s.record_webhook(&NewWebhookEvent {
        source: WebhookSource::Github,
        delivery_id: repo.map(|_| delivery.to_owned()),
        event: repo.map(|_| "issues".to_owned()),
        repo,
        outcome,
        reason: reason.map(str::to_owned),
        job,
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn a_repos_page_says_what_became_of_its_webhooks() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let a = repo(s, "repo-a").await;
    let b = repo(s, "repo-b").await;
    common::admin(s, "admin@x.test").await;
    common::grant(s, "maint@x.test", a, RepoRole::Maintainer).await;
    let admin = common::sign_in(s, "admin@x.test").await;
    let maint = common::sign_in(s, "maint@x.test").await;
    let app = router(s.clone(), common::auth(s));
    let page = format!("/admin/repos/{}", a.0);

    let (_, empty) = get(&app, &admin, &page).await;
    assert!(
        empty.contains("Recent webhooks") && empty.contains("No webhook"),
        "{empty}"
    );

    let made = job(s, a, 1).await;
    let live = job(s, a, 2).await;
    let spent = job(s, a, 3).await;
    hook(
        s,
        Some(a),
        "d-queued",
        WebhookOutcome::Queued,
        Some("job for x/y#1"),
        Some(made),
    )
    .await;
    hook(
        s,
        Some(a),
        "d-queued",
        WebhookOutcome::Duplicate,
        None,
        None,
    )
    .await;
    hook(
        s,
        Some(a),
        "d-live",
        WebhookOutcome::AlreadyLive,
        Some("x/y#2 already has a job"),
        Some(live),
    )
    .await;
    hook(
        s,
        Some(a),
        "d-spent",
        WebhookOutcome::Refused,
        Some("the repo's monthly budget is spent: $50.00 of $50.00 used"),
        Some(spent),
    )
    .await;
    hook(
        s,
        Some(a),
        "d-label",
        WebhookOutcome::Ignored,
        Some("not the label `aegis` put on an issue"),
        None,
    )
    .await;
    hook(
        s,
        Some(a),
        "d-who",
        WebhookOutcome::Ignored,
        Some("the sender cannot write to the repository"),
        None,
    )
    .await;
    hook(
        s,
        Some(b),
        "d-other-repo",
        WebhookOutcome::Queued,
        None,
        None,
    )
    .await;
    hook(s, None, "-", WebhookOutcome::BadSignature, None, None).await;

    let (status, shown) = get(&app, &admin, &page).await;
    assert_eq!(status, StatusCode::OK);
    for id in ["d-queued", "d-live", "d-spent", "d-label", "d-who"] {
        assert!(shown.contains(id), "{id}: {shown}");
    }
    for job in [made, live, spent] {
        assert!(
            shown.contains(&format!(r#"href="/jobs/{}""#, job.0)),
            "job {}: {shown}",
            job.0
        );
    }
    for (what, hint) in [
        ("received before", "only the first counts"),
        ("refused", "raise its monthly cap"),
        ("ignored", "Put the label"),
        ("ignored", "owner, member or collaborator"),
    ] {
        assert!(
            shown.contains(what) && shown.contains(hint),
            "{hint}: {shown}"
        );
    }
    // Only this repo's, and what names no repo is not here.
    assert!(!shown.contains("d-other-repo"), "{shown}");
    assert!(!shown.contains(">bad signature<"), "{shown}");

    // Not for anyone else.
    assert_eq!(get(&app, &maint, &page).await.0, StatusCode::NOT_FOUND);
    db.drop().await;
}
