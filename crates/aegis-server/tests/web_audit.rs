//! #368: the admin reads the audit log from the pages: who did what, when,
//! filtered by repo, by who and by action, a page at a time. It does not
//! exist for anyone else.

mod common;

use aegis_server::api::router;
use aegis_store::testing::test_db;
use aegis_store::{
    Actor, ActorKind, AuditFilter, NewRepo, Page, RepoId, RepoMode, RepoRole, Store, UserKind,
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

/// Registered by `actor`, audited as `repo.register`.
async fn repo(s: &Store, slug: &str, url: &str, actor: &Actor) -> RepoId {
    s.create_repo_by(
        &NewRepo {
            slug: slug.into(),
            url: url.into(),
            default_branch: "main".into(),
            language: "rust".into(),
            mode: RepoMode::Poc,
            max_concurrent_jobs: 1,
            config: json!({}),
        },
        actor,
    )
    .await
    .unwrap()
    .id
}

fn person(name: &str) -> Actor {
    Actor {
        kind: ActorKind::User,
        user: None,
        name: name.into(),
    }
}

/// The table's rows: the text between `<tbody>` and `</tbody>`.
fn rows(page: &str) -> &str {
    let start = page.find("<tbody>").unwrap();
    let end = start + page[start..].find("</tbody>").unwrap();
    &page[start..end]
}

#[tokio::test]
async fn an_admin_reads_who_did_what_filtered() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let ops = person("ops@x.test");
    let a = repo(s, "repo-a", "https://github.com/x/a", &ops).await;
    let b = repo(s, "repo-b", "https://github.com/x/b", &Actor::cli()).await;
    s.set_repo_role(
        UserKind::Person,
        "dev@x.test",
        a,
        Some(RepoRole::Viewer),
        &ops,
    )
    .await
    .unwrap();
    s.set_repo_role(UserKind::Person, "dev@x.test", a, None, &ops)
        .await
        .unwrap();
    common::admin(s, "admin@x.test").await;
    let admin = common::sign_in(s, "admin@x.test").await;
    let app = router(s.clone(), common::auth(s));

    let (status, page) = get(&app, &admin, "/admin/audit").await;
    assert_eq!(status, StatusCode::OK, "{page}");
    let all = rows(&page);
    for text in [
        "repo.register",
        "role.grant",
        "role.revoke",
        "ops@x.test",
        "repo-a",
        "repo-b",
    ] {
        assert!(all.contains(text), "{text}: {all}");
    }
    // Who: a person, the command line…
    assert!(all.contains("(cli)"), "{all}");
    assert!(
        all.contains(&format!("href=\"/admin/repos/{}\"", a.0)),
        "{all}"
    );
    // The filters offer the log's actions.
    assert!(page.contains("<option value=\"role.grant\""), "{page}");

    let (_, page) = get(&app, &admin, &format!("/admin/audit?repo_id={}", b.0)).await;
    let only_b = rows(&page);
    assert!(
        only_b.contains("repo-b") && !only_b.contains("role.grant"),
        "{only_b}"
    );

    let (_, page) = get(&app, &admin, "/admin/audit?actor=OPS%40x.test").await;
    let by_ops = rows(&page);
    assert!(
        by_ops.contains("role.revoke") && !by_ops.contains("repo-b"),
        "{by_ops}"
    );

    let (_, page) = get(&app, &admin, "/admin/audit?action=role.").await;
    let roles = rows(&page);
    assert!(
        roles.contains("role.grant") && roles.contains("role.revoke"),
        "{roles}"
    );
    assert!(!roles.contains("repo.register"), "{roles}");
    let (_, page) = get(&app, &admin, "/admin/audit?action=role.grant").await;
    assert!(!rows(&page).contains("role.revoke"), "{page}");

    // The repo's page links to its entries.
    let (_, page) = get(&app, &admin, &format!("/admin/repos/{}", a.0)).await;
    assert!(
        page.contains(&format!("href=\"/admin/audit?repo_id={}\"", a.0)),
        "{page}"
    );
    db.drop().await;
}

#[tokio::test]
async fn the_log_is_paged_escaped_and_only_for_an_admin() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let ops = person("ops@x.test");
    let a = repo(s, "repo-a", "https://github.com/x/<b>bold</b>", &ops).await;
    for n in 0..55 {
        let who = format!("dev{n}@x.test");
        s.set_repo_role(UserKind::Person, &who, a, Some(RepoRole::Viewer), &ops)
            .await
            .unwrap();
    }
    common::admin(s, "admin@x.test").await;
    common::grant(s, "maint@x.test", a, RepoRole::Maintainer).await;
    let admin = common::sign_in(s, "admin@x.test").await;
    let maint = common::sign_in(s, "maint@x.test").await;
    let app = router(s.clone(), common::auth(s));

    let (_, first) = get(&app, &admin, "/admin/audit?action=role.grant").await;
    assert_eq!(rows(&first).matches("<tr>").count(), 50, "{first}");
    let next = first
        .split("href=\"")
        .find(|h| h.starts_with("/admin/audit?") && h.contains("cursor="))
        .and_then(|h| h.split('"').next())
        .expect("a next page")
        .replace("&#38;", "&")
        .replace("&amp;", "&");
    assert!(next.contains("action=role.grant"), "{next}");
    let (_, second) = get(&app, &admin, &next).await;
    let grants = AuditFilter {
        action: Some("role.grant".into()),
        ..AuditFilter::default()
    };
    let all = s
        .audit_log_where(&grants, Page::first(Page::MAX_LIMIT))
        .await
        .unwrap()
        .items
        .len();
    assert!(all > 55, "{all}");
    assert_eq!(rows(&second).matches("<tr>").count(), all - 50, "{second}");
    assert!(!second.contains("cursor="), "{second}");

    // What an entry holds is shown as text.
    let (_, page) = get(&app, &admin, "/admin/audit?action=repo.register").await;
    assert!(!page.contains("<b>bold</b>"), "{page}");
    assert!(
        page.contains("&#60;b&#62;bold") || page.contains("&lt;b&gt;bold"),
        "{page}"
    );

    for uri in ["/admin/audit", "/admin/audit?action=role."] {
        assert_eq!(
            get(&app, &maint, uri).await.0,
            StatusCode::NOT_FOUND,
            "{uri}"
        );
    }
    let (status, _) = get(&app, &admin, "/admin/audit?repo_id=x").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    db.drop().await;
}

/// A token made on the pages is audited by its ID, never its value.
#[tokio::test]
async fn the_log_never_holds_a_token() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    common::admin(s, "admin@x.test").await;
    let admin = common::sign_in(s, "admin@x.test").await;
    let app = router(s.clone(), common::auth(s));

    let request = Request::post("/me/tokens")
        .header(header::HOST, "localhost:8080")
        .header(header::ORIGIN, "http://localhost:8080")
        .header(header::COOKIE, &admin.cookie)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(format!(
            "label=laptop&days=7&_csrf={}",
            admin.csrf
        )))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let shown = String::from_utf8(bytes.to_vec()).unwrap();
    let token = shown
        .split("<pre class=\"token\"><code>")
        .nth(1)
        .and_then(|t| t.split('<').next())
        .expect("the new token, shown once")
        .to_owned();
    assert!(token.len() >= 32, "{token}");

    let (_, page) = get(&app, &admin, "/admin/audit?action=token.create").await;
    assert!(rows(&page).contains("token.create"), "{page}");
    assert!(!page.contains(&token), "{page}");
    db.drop().await;
}
