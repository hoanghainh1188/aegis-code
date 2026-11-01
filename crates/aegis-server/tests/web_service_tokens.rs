//! #371: an admin adds a service (CI, a connector) and makes and revokes
//! its API tokens from its page, as `aegis-server token|revoke-tokens`
//! do. A new token is shown once, in the answer to the request that made
//! it, never again, and the audit log keeps only its ID.

mod common;

use aegis_server::api::router;
use aegis_store::testing::test_db;
use aegis_store::{NewRepo, Page, RepoId, RepoMode, RepoRole, Store, UserKind};
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::Browser;
use http_body_util::BodyExt;
use serde_json::{Value, json};
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

/// `GET <uri>` of the API with a bearer `token`: its status and body.
async fn api(app: &Router, token: &str, uri: &str) -> (StatusCode, Value) {
    let request = Request::get(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    let a = send(app, request).await;
    (
        a.status,
        serde_json::from_str(&a.body).unwrap_or(Value::Null),
    )
}

async fn repo(s: &Store, slug: &str) -> RepoId {
    s.create_repo(&NewRepo {
        slug: slug.into(),
        url: format!("https://github.com/x/{slug}"),
        default_branch: "main".into(),
        language: "rust".into(),
        mode: RepoMode::Poc,
        max_concurrent_jobs: 1,
        config: json!({}),
    })
    .await
    .unwrap()
    .id
}

/// The token a page shows once.
fn shown_token(body: &str) -> String {
    body.split("<pre class=\"token\"><code>")
        .nth(1)
        .and_then(|t| t.split('<').next())
        .expect("the new token, shown once")
        .to_owned()
}

#[tokio::test]
async fn an_admin_adds_a_service_and_makes_its_token_shown_once() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let a = repo(s, "repo-a").await;
    common::admin(s, "admin@x.test").await;
    let admin = common::sign_in(s, "admin@x.test").await;
    let app = router(s.clone(), common::auth(s));

    let sent = post(&app, &admin, "/admin/services", "name=CI-Bot").await;
    assert_eq!(sent.status, StatusCode::SEE_OTHER, "{}", sent.body);
    let page = sent.location.split('?').next().unwrap().to_owned();
    assert!(page.starts_with("/admin/users/"), "{}", sent.location);
    let entry = s
        .audit_log(None, Page::first(1))
        .await
        .unwrap()
        .items
        .remove(0);
    assert_eq!(
        (entry.action.as_str(), entry.actor.name.as_str()),
        ("user.create", "admin@x.test")
    );
    let shown = get(&app, &admin, &sent.location).await;
    assert!(
        shown.body.contains("ci-bot") && shown.body.contains("No live token"),
        "{}",
        shown.body
    );

    let made = post(&app, &admin, &format!("{page}/tokens"), "x=1").await;
    assert_eq!(made.status, StatusCode::OK, "{}", made.body);
    let token = shown_token(&made.body);
    assert!(token.len() >= 32, "{token}");
    // It acts as the service, with the service's roles.
    s.set_repo_role(
        UserKind::Service,
        "ci-bot",
        a,
        Some(RepoRole::Maintainer),
        &aegis_store::Actor::cli(),
    )
    .await
    .unwrap();
    let (status, me) = api(&app, &token, "/api/v1/me").await;
    assert_eq!(status, StatusCode::OK, "{me}");
    assert_eq!(me["data"]["name"], "ci-bot", "{me}");
    let (_, repos) = api(&app, &token, "/api/v1/repos").await;
    assert!(repos.to_string().contains("repo-a"), "{repos}");

    // Never shown again, nor kept in the audit log: its ID only.
    let again = get(&app, &admin, &page).await;
    assert!(!again.body.contains(&token), "{}", again.body);
    assert!(again.body.contains("1 live token"), "{}", again.body);
    let created = s.audit_log(None, Page::first(5)).await.unwrap();
    let entry = created
        .items
        .iter()
        .find(|e| e.action == "token.create")
        .unwrap();
    assert!(entry.detail["token_id"].is_i64(), "{:?}", entry.detail);
    assert!(!entry.detail.to_string().contains(&token));
    db.drop().await;
}

#[tokio::test]
async fn an_admin_revokes_one_token_or_all() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    common::admin(s, "admin@x.test").await;
    let admin = common::sign_in(s, "admin@x.test").await;
    let app = router(s.clone(), common::auth(s));
    let sent = post(&app, &admin, "/admin/services", "name=ci-bot").await;
    let page = sent.location.split('?').next().unwrap().to_owned();
    let mut tokens = Vec::new();
    for _ in 0..3 {
        let made = post(&app, &admin, &format!("{page}/tokens"), "x=1").await;
        tokens.push(shown_token(&made.body));
    }
    let id = |s: &Store| {
        let s = s.clone();
        async move {
            let user = s
                .list_users(Some("ci-bot"), Page::first(5))
                .await
                .unwrap()
                .items
                .remove(0)
                .user
                .id;
            s.service_tokens(user).await.unwrap()
        }
    };
    let live = id(s).await;
    assert_eq!(live.len(), 3);
    // The newest first: revoke the last one made.
    let one = post(
        &app,
        &admin,
        &format!("{page}/tokens/{}/revoke", live[0].id),
        "x=1",
    )
    .await;
    assert_eq!(one.status, StatusCode::SEE_OTHER, "{}", one.body);
    assert_eq!(
        api(&app, &tokens[2], "/api/v1/me").await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(api(&app, &tokens[0], "/api/v1/me").await.0, StatusCode::OK);
    let entry = s
        .audit_log(None, Page::first(1))
        .await
        .unwrap()
        .items
        .remove(0);
    assert_eq!(entry.action, "token.revoke");

    let all = post(&app, &admin, &format!("{page}/tokens/revoke-all"), "x=1").await;
    assert_eq!(all.status, StatusCode::SEE_OTHER, "{}", all.body);
    for token in &tokens {
        assert_eq!(
            api(&app, token, "/api/v1/me").await.0,
            StatusCode::UNAUTHORIZED
        );
    }
    assert!(
        get(&app, &admin, &page)
            .await
            .body
            .contains("No live token")
    );
    db.drop().await;
}

#[tokio::test]
async fn only_an_admin_and_only_for_a_service() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let a = repo(s, "repo-a").await;
    common::admin(s, "admin@x.test").await;
    common::grant(s, "maint@x.test", a, RepoRole::Maintainer).await;
    let admin = common::sign_in(s, "admin@x.test").await;
    let maint = common::sign_in(s, "maint@x.test").await;
    let app = router(s.clone(), common::auth(s));

    assert_eq!(
        post(&app, &maint, "/admin/services", "name=x").await.status,
        StatusCode::NOT_FOUND
    );
    let sent = post(&app, &admin, "/admin/services", "name=ci-bot").await;
    let page = sent.location.split('?').next().unwrap().to_owned();
    assert_eq!(
        post(&app, &maint, &format!("{page}/tokens"), "x=1")
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        post(&app, &maint, &format!("{page}/tokens/revoke-all"), "x=1")
            .await
            .status,
        StatusCode::NOT_FOUND
    );

    // A name with `@` is a person; a service is added once.
    let person = post(&app, &admin, "/admin/services", "name=a%40b.test").await;
    assert_eq!(person.status, StatusCode::BAD_REQUEST, "{}", person.body);
    let twice = post(&app, &admin, "/admin/services", "name=ci-bot").await;
    assert_eq!(twice.status, StatusCode::CONFLICT, "{}", twice.body);
    // A person gets personal tokens at /me/tokens, not here.
    let maint_page = format!("/admin/users/{}/tokens", maint.user.0);
    assert_eq!(
        post(&app, &admin, &maint_page, "x=1").await.status,
        StatusCode::BAD_REQUEST
    );
    db.drop().await;
}
