//! #372: a repo's monthly cap is changed from the pages by whoever may
//! change it through `PUT /api/v1/repos/{id}/budget` (a maintainer of the
//! repo, or an admin), through the store's `set_monthly_budget_by`, so it
//! is checked and audited the same. A viewer reads it; anyone else does
//! not find it.

mod common;

use aegis_server::api::router;
use aegis_store::testing::test_db;
use aegis_store::{NewRepo, Page, RepoId, RepoMode, RepoRole, Store};
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::Browser;
use http_body_util::BodyExt;
use serde_json::json;
use tower::ServiceExt;

struct Answer {
    status: StatusCode,
    location: String,
    body: String,
}

async fn send(app: &Router, request: Request<Body>) -> Answer {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let location = response
        .headers()
        .get(header::LOCATION)
        .map(|l| l.to_str().unwrap().to_owned())
        .unwrap_or_default();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    Answer {
        status,
        location,
        body: String::from_utf8(bytes.to_vec()).unwrap(),
    }
}

async fn get(app: &Router, who: &Browser, uri: &str) -> Answer {
    let request = Request::get(uri)
        .header(header::HOST, "localhost:8080")
        .header(header::COOKIE, &who.cookie)
        .body(Body::empty())
        .unwrap();
    send(app, request).await
}

async fn post(app: &Router, who: &Browser, uri: &str, fields: &str) -> Answer {
    let request = Request::post(uri)
        .header(header::HOST, "localhost:8080")
        .header(header::ORIGIN, "http://localhost:8080")
        .header(header::COOKIE, &who.cookie)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(format!("{fields}&_csrf={}", who.csrf)))
        .unwrap();
    send(app, request).await
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

async fn cap(s: &Store, repo: RepoId) -> Option<f64> {
    s.repo_budget(repo).await.unwrap().unwrap().monthly_usd
}

#[tokio::test]
async fn a_maintainer_or_an_admin_changes_the_cap_and_it_is_audited() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let a = repo(s, "repo-a").await;
    common::admin(s, "admin@x.test").await;
    common::grant(s, "maint@x.test", a, RepoRole::Maintainer).await;
    let admin = common::sign_in(s, "admin@x.test").await;
    let maint = common::sign_in(s, "maint@x.test").await;
    let app = router(s.clone(), common::auth(s));
    let page = format!("/repos/{}/budget", a.0);

    // What is spent this month, and in which time zone, beside the form.
    let shown = get(&app, &maint, &page).await;
    assert_eq!(shown.status, StatusCode::OK, "{}", shown.body);
    assert!(
        shown.body.contains("repo-a")
            && shown.body.contains("$0.00")
            && shown.body.contains("UTC")
            && shown.body.contains(&format!(r#"action="{page}""#)),
        "{}",
        shown.body
    );

    let sent = post(&app, &maint, &page, "monthly_usd=25.5").await;
    assert_eq!(sent.status, StatusCode::SEE_OTHER, "{}", sent.body);
    assert_eq!(sent.location, format!("{page}?done=saved"));
    assert_eq!(cap(s, a).await, Some(25.5));
    let entry = s
        .audit_log(None, Page::first(1))
        .await
        .unwrap()
        .items
        .remove(0);
    assert_eq!(
        (
            entry.actor.name.as_str(),
            entry.action.as_str(),
            entry.repo_id
        ),
        ("maint@x.test", "repo.budget", Some(a))
    );
    assert_eq!(entry.detail, json!({ "monthly_usd": 25.5, "was": null }));
    let after = get(&app, &maint, &sent.location).await;
    assert!(
        after.body.contains("$25.50") && after.body.contains(r#"role="status""#),
        "{}",
        after.body
    );

    // An admin too; and the cap can be taken away, back to the mode's.
    let sent = post(&app, &admin, &page, "monthly_usd=").await;
    assert_eq!(sent.status, StatusCode::SEE_OTHER, "{}", sent.body);
    assert_eq!(cap(s, a).await, None);
    let entry = s
        .audit_log(None, Page::first(1))
        .await
        .unwrap()
        .items
        .remove(0);
    assert_eq!(entry.actor.name, "admin@x.test");
    assert_eq!(entry.detail, json!({ "monthly_usd": null, "was": 25.5 }));

    // A cap below a job's reserve: said plainly that new jobs wait.
    post(&app, &maint, &page, "monthly_usd=1").await;
    let low = get(&app, &maint, &format!("{page}?done=saved")).await;
    assert!(low.body.contains("next month"), "{}", low.body);

    // The admin's repo page and the dashboard lead to it.
    let repo_page = get(&app, &admin, &format!("/admin/repos/{}", a.0)).await;
    assert!(
        repo_page.body.contains(&format!(r#"href="{page}""#)),
        "{}",
        repo_page.body
    );
    let dash = get(&app, &maint, "/").await;
    assert!(
        dash.body.contains(&format!(r#"href="{page}""#)),
        "{}",
        dash.body
    );
    db.drop().await;
}

#[tokio::test]
async fn a_viewer_reads_and_others_do_not_find_it() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let a = repo(s, "repo-a").await;
    common::grant(s, "view@x.test", a, RepoRole::Viewer).await;
    common::grant(s, "rev@x.test", a, RepoRole::Reviewer).await;
    let viewer = common::sign_in(s, "view@x.test").await;
    let reviewer = common::sign_in(s, "rev@x.test").await;
    let stranger = common::sign_in(s, "who@x.test").await;
    let app = router(s.clone(), common::auth(s));
    let page = format!("/repos/{}/budget", a.0);

    let shown = get(&app, &viewer, &page).await;
    assert_eq!(shown.status, StatusCode::OK);
    assert!(
        !shown.body.contains("<form method=\"post\" action=\"/repos"),
        "{}",
        shown.body
    );
    assert!(
        !get(&app, &viewer, "/")
            .await
            .body
            .contains(&format!(r#"href="{page}""#))
    );
    for who in [&viewer, &reviewer] {
        let sent = post(&app, who, &page, "monthly_usd=10").await;
        assert_eq!(sent.status, StatusCode::FORBIDDEN, "{}", sent.body);
    }
    assert_eq!(
        get(&app, &stranger, &page).await.status,
        StatusCode::NOT_FOUND
    );
    let sent = post(&app, &stranger, &page, "monthly_usd=10").await;
    assert_eq!(sent.status, StatusCode::NOT_FOUND);
    assert_eq!(
        get(&app, &viewer, "/repos/999999/budget").await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(cap(s, a).await, None);
    db.drop().await;
}

#[tokio::test]
async fn a_cap_that_is_not_a_sum_of_dollars_is_refused_plainly() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let a = repo(s, "repo-a").await;
    common::grant(s, "maint@x.test", a, RepoRole::Maintainer).await;
    let maint = common::sign_in(s, "maint@x.test").await;
    let app = router(s.clone(), common::auth(s));
    let page = format!("/repos/{}/budget", a.0);

    for (value, says) in [
        ("abc", "not a number"),
        ("-1", "0 or above"),
        ("inf", "not a number"),
    ] {
        let sent = post(&app, &maint, &page, &format!("monthly_usd={value}")).await;
        assert_eq!(sent.status, StatusCode::BAD_REQUEST, "{value}");
        assert!(sent.body.contains(says), "{value}: {}", sent.body);
    }
    assert_eq!(cap(s, a).await, None);
    db.drop().await;
}
