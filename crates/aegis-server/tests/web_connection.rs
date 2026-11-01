//! #331: a repo's connection to its forge, for the admin: the webhook to
//! make, whether each secret is in the store (never its value), its last
//! webhooks, and a check of its tokens and URL against the forge's API,
//! played here by a mock server.

mod common;

use std::path::PathBuf;

use aegis_lifecycle::{ModeConfig, Modes};
use aegis_secrets::Secrets;
use aegis_server::api::router_with;
use aegis_server::web::RepoAdmin;
use aegis_server::webhook::{COMMENT_TOKEN_ENV, GITHUB_SECRET_ENV};
use aegis_store::testing::test_db;
use aegis_store::{
    NewRepo, NewWebhookEvent, RepoId, RepoMode, RepoRole, Store, WebhookOutcome, WebhookSource,
};
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::Browser;
use http_body_util::BodyExt;
use serde_json::json;
use tower::ServiceExt;
use wiremock::matchers::{header as has_header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const POC: &str = include_str!("../../../config/poc.toml");
const FORK_TOKEN: &str = "fork-token-value-do-not-show";
const COMMENT_TOKEN: &str = "comment-token-value-do-not-show";
const HOOK_SECRET: &str = "webhook-secret-value-do-not-show";

async fn send(app: &Router, request: Request<Body>) -> (StatusCode, String) {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

async fn get(app: &Router, who: &Browser, uri: &str) -> (StatusCode, String) {
    let request = Request::get(uri)
        .header(header::HOST, "localhost:8080")
        .header(header::COOKIE, &who.cookie)
        .body(Body::empty())
        .unwrap();
    send(app, request).await
}

async fn post(app: &Router, who: &Browser, uri: &str) -> (StatusCode, String) {
    let request = Request::post(uri)
        .header(header::HOST, "localhost:8080")
        .header(header::ORIGIN, "http://localhost:8080")
        .header(header::COOKIE, &who.cookie)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(format!("_csrf={}", who.csrf)))
        .unwrap();
    send(app, request).await
}

/// The repo `acme/app` on GitHub, its fork `aegis-bot/app` whose API is
/// `api`.
async fn repo(s: &Store, api: &str) -> RepoId {
    s.create_repo(&NewRepo {
        slug: "app".into(),
        url: "https://github.com/acme/app".into(),
        default_branch: "main".into(),
        language: "go".into(),
        mode: RepoMode::Poc,
        max_concurrent_jobs: 1,
        config: json!({ "fork": {
            "owner": "aegis-bot", "name": "app", "api": api, "token_env": "AEGIS_TEST_FORK_TOKEN"
        } }),
    })
    .await
    .unwrap()
    .id
}

fn repo_admin(secrets: Secrets, api: &str) -> RepoAdmin {
    let mut poc = ModeConfig::from_toml(POC).unwrap();
    poc.run.state_dir = PathBuf::from("/nonexistent/aegis-test-state");
    RepoAdmin::new(Modes::new(Some(poc), None).unwrap(), secrets).with_apis(api, api)
}

fn all_secrets() -> Secrets {
    Secrets::fixed([
        ("AEGIS_TEST_FORK_TOKEN", FORK_TOKEN),
        (COMMENT_TOKEN_ENV, COMMENT_TOKEN),
        (GITHUB_SECRET_ENV, HOOK_SECRET),
    ])
}

fn github_repo(full_name: &str, push: bool, pull: bool) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "full_name": full_name,
        "html_url": format!("https://github.com/{full_name}"),
        "permissions": { "admin": false, "push": push, "pull": pull },
        "has_issues": true,
        "archived": false
    }))
}

async fn admin_app(s: &Store, secrets: Secrets, api: &str) -> (Router, Browser) {
    common::admin(s, "admin@x.test").await;
    let admin = common::sign_in(s, "admin@x.test").await;
    let app = router_with(s.clone(), common::auth(s), Some(repo_admin(secrets, api)));
    (app, admin)
}

#[tokio::test]
async fn the_page_says_what_to_make_and_what_the_store_has_never_the_values() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let forge = MockServer::start().await;
    let id = repo(s, &forge.uri()).await;
    let secrets = Secrets::fixed([("AEGIS_TEST_FORK_TOKEN", FORK_TOKEN)]);
    let (app, admin) = admin_app(s, secrets, &forge.uri()).await;
    common::grant(s, "maint@x.test", id, RepoRole::Maintainer).await;
    let maint = common::sign_in(s, "maint@x.test").await;
    let uri = format!("/admin/repos/{}/connection", id.0);

    let (status, page) = get(&app, &admin, &uri).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert!(page.contains("/webhooks/github"), "{page}");
    assert!(page.contains("public_url"), "no public URL set: {page}");
    for event in [
        "Issues",
        "Workflow runs",
        "Pull request reviews",
        "Issue comments",
    ] {
        assert!(page.contains(event), "{event}: {page}");
    }
    // Names and whether they are set, never values.
    assert!(page.contains("AEGIS_TEST_FORK_TOKEN") && page.contains(COMMENT_TOKEN_ENV));
    assert!(
        page.contains(">set<") && page.contains(">missing<"),
        "{page}"
    );
    assert!(!page.contains(FORK_TOKEN), "{page}");
    assert!(page.contains("No webhook from this repo yet"), "{page}");

    // Its webhooks: the ignored with why, the ping, bad signatures counted.
    for (outcome, reason) in [
        (WebhookOutcome::Ping, None),
        (
            WebhookOutcome::Ignored,
            Some("label `bug`, not `aegis`".to_owned()),
        ),
    ] {
        s.record_webhook(&NewWebhookEvent {
            source: WebhookSource::Github,
            delivery_id: Some("d".into()),
            event: Some("issues".into()),
            repo: Some(id),
            outcome,
            reason,
        })
        .await
        .unwrap();
    }
    s.record_webhook(&NewWebhookEvent {
        source: WebhookSource::Github,
        delivery_id: None,
        event: None,
        repo: None,
        outcome: WebhookOutcome::BadSignature,
        reason: None,
    })
    .await
    .unwrap();
    let (_, page) = get(&app, &admin, &uri).await;
    assert!(page.contains("ping: connected"), "{page}");
    assert!(
        page.contains("ignored") && page.contains("label `bug`, not `aegis`"),
        "{page}"
    );
    assert!(
        page.contains("1 github webhook(s) with a bad signature"),
        "{page}"
    );

    // Not for anyone else.
    assert_eq!(get(&app, &maint, &uri).await.0, StatusCode::NOT_FOUND);
    assert_eq!(post(&app, &maint, &uri).await.0, StatusCode::NOT_FOUND);
    db.drop().await;
}

#[tokio::test]
async fn the_check_passes_when_the_tokens_and_the_url_are_right() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let forge = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/aegis-bot/app"))
        .and(has_header("authorization", format!("Bearer {FORK_TOKEN}")))
        .respond_with(github_repo("aegis-bot/app", true, true))
        .mount(&forge)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/app"))
        .and(has_header(
            "authorization",
            format!("Bearer {COMMENT_TOKEN}"),
        ))
        .respond_with(github_repo("acme/app", false, true))
        .mount(&forge)
        .await;
    let id = repo(s, &forge.uri()).await;
    let (app, admin) = admin_app(s, all_secrets(), &forge.uri()).await;

    let (status, page) = post(&app, &admin, &format!("/admin/repos/{}/connection", id.0)).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page.matches("<li class=\"pass\">").count(), 3, "{page}");
    assert!(
        page.contains("may push `agent/*` branches to `aegis-bot/app`"),
        "{page}"
    );
    assert!(
        page.contains("may comment on the issues of `acme/app`"),
        "{page}"
    );
    for secret in [FORK_TOKEN, COMMENT_TOKEN, HOOK_SECRET] {
        assert!(!page.contains(secret), "{page}");
    }
    db.drop().await;
}

#[tokio::test]
async fn the_check_says_why_a_token_or_the_url_is_wrong() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let forge = MockServer::start().await;
    // The fork's token is refused; the repo was renamed.
    Mock::given(method("GET"))
        .and(path("/repos/aegis-bot/app"))
        .respond_with(
            ResponseTemplate::new(401).set_body_json(json!({ "message": "Bad credentials" })),
        )
        .mount(&forge)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/app"))
        .respond_with(github_repo("acme/app-next", false, true))
        .mount(&forge)
        .await;
    let id = repo(s, &forge.uri()).await;
    let (app, admin) = admin_app(s, all_secrets(), &forge.uri()).await;
    let uri = format!("/admin/repos/{}/connection", id.0);

    let (_, page) = post(&app, &admin, &uri).await;
    assert!(page.contains("refused the token"), "{page}");
    assert!(
        page.contains("acme/app-next") && page.contains("renamed or moved"),
        "{page}"
    );
    assert!(
        !page.contains(FORK_TOKEN) && !page.contains(COMMENT_TOKEN),
        "{page}"
    );

    // Read-only: the token reads the fork but may not push.
    forge.reset().await;
    Mock::given(method("GET"))
        .and(path("/repos/aegis-bot/app"))
        .respond_with(github_repo("aegis-bot/app", false, true))
        .mount(&forge)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/app"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({ "message": "Not Found" })))
        .mount(&forge)
        .await;
    let (_, page) = post(&app, &admin, &uri).await;
    assert!(page.contains("may not push"), "{page}");
    assert!(
        page.contains("is not there, or the token cannot see it"),
        "{page}"
    );

    // No comment token: said, and the URL check is skipped.
    let secrets = Secrets::fixed([("AEGIS_TEST_FORK_TOKEN", FORK_TOKEN)]);
    let app = router_with(
        s.clone(),
        common::auth(s),
        Some(repo_admin(secrets, &forge.uri())),
    );
    let (_, page) = post(&app, &admin, &uri).await;
    assert!(
        page.contains(&format!("the secret store has no `{COMMENT_TOKEN_ENV}`")),
        "{page}"
    );
    assert!(page.contains("<li class=\"skip\">"), "{page}");
    db.drop().await;
}
