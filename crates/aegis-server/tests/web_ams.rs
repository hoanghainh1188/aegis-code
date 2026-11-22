//! #526: the AMS report page shows the contracts of the repos a person has
//! a role in, by SLA level, and their spend; the platform's admins see all.

mod common;

use aegis_ledger::IssueId;
use aegis_server::api::router;
use aegis_store::testing::test_db;
use aegis_store::{
    Actor, JobSource, ModeName, NewJob, NewRepo, RepoId, RepoRole, SlaPriority, Store, TenantId,
};
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::json;
use tower::ServiceExt;

async fn repo_in(s: &Store, slug: &str, tenant: TenantId) -> RepoId {
    s.create_repo_in_by(
        &NewRepo {
            slug: slug.into(),
            url: format!("https://github.com/x/{slug}"),
            default_branch: "main".into(),
            language: "rust".into(),
            mode: ModeName::POC,
            max_concurrent_jobs: 2,
            config: json!({}),
        },
        tenant,
        &Actor::cli(),
    )
    .await
    .unwrap()
    .id
}

async fn page(app: &axum::Router, cookie: &str) -> (StatusCode, String) {
    let request = Request::get("/ams")
        .header(header::HOST, "localhost:8080")
        .header(header::COOKIE, cookie)
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

#[tokio::test]
async fn the_report_shows_the_contracts_one_may_see() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let acme = s
        .create_tenant_by("acme", "Acme", &Actor::cli())
        .await
        .unwrap();
    let globex = s
        .create_tenant_by("globex", "Globex", &Actor::cli())
        .await
        .unwrap();
    let a = repo_in(s, "acme-app", acme.id).await;
    let g = repo_in(s, "globex-app", globex.id).await;
    for (repo, n) in [(a, 1), (g, 1)] {
        s.create_job(&NewJob {
            repo_id: repo,
            issue_ref: format!("x/y#{}{n}", repo.0),
            ledger_key: IssueId::parse(format!("j{}-{n}", repo.0)).unwrap(),
            source: JobSource::Jsm,
            priority: Some(SlaPriority::P1),
            // Already overdue, still queued.
            sla_deadline: Some(time::OffsetDateTime::now_utc() - time::Duration::minutes(5)),
        })
        .await
        .unwrap();
    }
    common::admin(s, "root@x.test").await;
    common::grant(s, "dev@acme.test", a, RepoRole::Viewer).await;
    let root = common::sign_in(s, "root@x.test").await;
    let dev = common::sign_in(s, "dev@acme.test").await;
    let app = router(s.clone(), common::auth(s));

    let (status, all) = page(&app, &root.cookie).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        all.contains("<th scope=\"row\">acme</th>")
            && all.contains("<th scope=\"row\">globex</th>"),
        "{all}"
    );
    assert!(all.contains("href=\"/ams\""), "the masthead links it");

    let (_, mine) = page(&app, &dev.cookie).await;
    assert!(mine.contains("<th scope=\"row\">acme</th>"), "{mine}");
    assert!(
        !mine.contains("globex"),
        "another contract is not shown: {mine}"
    );
    assert!(
        mine.contains("<td class=\"num bad\">1</td>"),
        "the overdue job: {mine}"
    );
    db.drop().await;
}
