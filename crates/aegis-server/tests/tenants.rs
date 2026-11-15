//! #525: a tenant's admin administers its repos (their pages, roles,
//! pauses, jobs) and no other tenant's, and none of the platform's pages.

mod common;

use aegis_server::api::router;
use aegis_store::testing::test_db;
use aegis_store::{Actor, NewRepo, RepoId, RepoMode, Store, TenantId, UserKind};
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::Browser;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

async fn send(app: &Router, request: Request<Body>) -> (StatusCode, String) {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

async fn page(app: &Router, who: &Browser, uri: &str) -> (StatusCode, String) {
    let request = Request::get(uri)
        .header(header::HOST, "localhost:8080")
        .header(header::COOKIE, &who.cookie)
        .body(Body::empty())
        .unwrap();
    send(app, request).await
}

async fn form(app: &Router, who: &Browser, uri: &str, fields: &str) -> StatusCode {
    let request = Request::post(uri)
        .header(header::HOST, "localhost:8080")
        .header(header::ORIGIN, "http://localhost:8080")
        .header(header::COOKIE, &who.cookie)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(format!("{fields}&_csrf={}", who.csrf)))
        .unwrap();
    send(app, request).await.0
}

async fn repo_in(s: &Store, slug: &str, tenant: TenantId) -> RepoId {
    s.create_repo_in_by(
        &NewRepo {
            slug: slug.into(),
            url: format!("https://github.com/x/{slug}"),
            default_branch: "main".into(),
            language: "rust".into(),
            mode: RepoMode::Poc,
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

#[tokio::test]
async fn a_tenant_admin_administers_its_repos_only() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let acme = s
        .create_tenant_by("acme", "Acme", &Actor::cli())
        .await
        .unwrap();
    let mine = repo_in(s, "acme-app", acme.id).await;
    let theirs = repo_in(s, "other-app", TenantId::DEFAULT).await;
    s.set_tenant_admin_by(
        acme.id,
        UserKind::Person,
        "ops@acme.test",
        true,
        &Actor::cli(),
    )
    .await
    .unwrap();
    common::admin(s, "root@x.test").await;
    let ops = common::sign_in(s, "ops@acme.test").await;
    let app = router(s.clone(), common::auth(s));

    // The masthead offers the repos' pages; they list its repos only.
    let (status, list) = page(&app, &ops, "/admin/repos").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        list.contains("acme-app") && !list.contains("other-app"),
        "{list}"
    );
    assert!(
        !list.contains("Add a repo"),
        "registering repos is the platform's"
    );
    let (_, home) = page(&app, &ops, "/").await;
    assert!(home.contains("href=\"/admin/repos\""));

    // Its repo's page, roles and pause; not another tenant's.
    assert_eq!(
        page(&app, &ops, &format!("/admin/repos/{}", mine.0))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        page(&app, &ops, &format!("/admin/repos/{}", theirs.0))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let granted = form(
        &app,
        &ops,
        &format!("/admin/repos/{}/roles", mine.0),
        "who=dev%40acme.test&role=maintainer",
    )
    .await;
    assert_eq!(granted, StatusCode::SEE_OTHER);
    let refused = form(
        &app,
        &ops,
        &format!("/admin/repos/{}/roles", theirs.0),
        "who=dev%40acme.test&role=maintainer",
    )
    .await;
    assert_eq!(refused, StatusCode::NOT_FOUND);
    let paused = form(
        &app,
        &ops,
        &format!("/admin/repos/{}/pause", mine.0),
        "reason=x",
    )
    .await;
    assert_eq!(paused, StatusCode::SEE_OTHER);
    assert!(s.repo_pause(mine).await.unwrap().is_some());
    let not = form(
        &app,
        &ops,
        &format!("/admin/repos/{}/pause", theirs.0),
        "reason=x",
    )
    .await;
    assert_eq!(not, StatusCode::NOT_FOUND);
    assert!(s.repo_pause(theirs).await.unwrap().is_none());

    // The platform's pages stay the platform admin's.
    for uri in ["/admin/users", "/admin/audit", "/admin/pause"] {
        assert_eq!(
            page(&app, &ops, uri).await.0,
            StatusCode::NOT_FOUND,
            "{uri}"
        );
    }
    db.drop().await;
}

#[tokio::test]
async fn a_tenant_admins_token_sees_its_tenants_repos_only() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let acme = s
        .create_tenant_by("acme", "Acme", &Actor::cli())
        .await
        .unwrap();
    let mine = repo_in(s, "acme-app", acme.id).await;
    repo_in(s, "other-app", TenantId::DEFAULT).await;
    s.set_tenant_admin_by(acme.id, UserKind::Service, "acme-bot", true, &Actor::cli())
        .await
        .unwrap();
    let token = common::token(s, "acme-bot").await;
    let app = router(s.clone(), common::auth(s));
    let request = Request::get("/api/v1/repos")
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    let (status, text) = send(&app, request).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let body: Value = serde_json::from_str(&text).unwrap();
    let ids: Vec<i64> = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_i64().unwrap())
        .collect();
    assert_eq!(ids, [mine.0]);
    db.drop().await;
}
