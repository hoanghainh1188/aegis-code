//! The GitHub App's webhook (ADR 0017 §5, #540) against a real database:
//! the secret that verifies a delivery says which path it is for, never
//! its payload; installation events are recorded from the app's webhook
//! only, and a suspended installation takes no job; the app checks who
//! may ask, with no comment token; its own comments start nothing.

use std::sync::Arc;

use aegis_secrets::{MemorySource, Secrets};
use aegis_server::config::ServerConfig;
use aegis_server::github_app::AppHandle;
use aegis_server::secrets::load_at_start;
use aegis_server::webhook::{
    DEFAULT_MAX_BODY_BYTES, GITHUB_SECRET_ENV, GitHubNotifier, WebhookConfig, router,
};
use aegis_store::testing::test_db;
use aegis_store::{AppOnRepo, JobFilter, NewRepo, Page, RepoMode, Store};
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use hmac::{Hmac, KeyInit, Mac};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sha2::Sha256;
use tower::ServiceExt;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const REPO_SECRET: &str = "repo-webhook-secret";
const APP_SECRET: &str = "app-webhook-secret";
const APP_SECRET_ENV: &str = "AEGIS_GITHUB_APP_WEBHOOK_SECRET";
const APP_KEY_ENV: &str = "AEGIS_GITHUB_APP_PRIVATE_KEY";

fn pem() -> String {
    use aws_lc_rs::encoding::{AsDer, Pkcs8V1Der};
    use base64::Engine;
    let pair = aws_lc_rs::rsa::KeyPair::generate(aws_lc_rs::rsa::KeySize::Rsa2048).unwrap();
    let der = AsDer::<Pkcs8V1Der>::as_der(&pair).unwrap();
    format!(
        "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n",
        base64::engine::general_purpose::STANDARD.encode(der.as_ref())
    )
}

/// The webhooks with both secrets, the app on `api`, and no comment token.
fn config(api: &str) -> WebhookConfig {
    let pem = pem();
    let secrets = Secrets::fixed([
        (GITHUB_SECRET_ENV, REPO_SECRET),
        (APP_SECRET_ENV, APP_SECRET),
        (APP_KEY_ENV, pem.as_str()),
    ]);
    let section = ServerConfig::from_toml(&format!("[github_app]\napp_id = 42\napi = \"{api}\"\n"))
        .unwrap()
        .github_app
        .unwrap();
    let app = Arc::new(AppHandle::new(section, secrets.clone()).unwrap());
    WebhookConfig {
        notifier: Some(GitHubNotifier::new(api, secrets.clone())),
        gitlab_notifier: None,
        label: "aegis".into(),
        burst: 100,
        per_minute: 600,
        max_body_bytes: DEFAULT_MAX_BODY_BYTES,
        modes: None,
        app: Some(app),
        secrets,
    }
}

async fn repo(s: &Store, slug: &str, settings: Value) {
    s.create_repo(&NewRepo {
        slug: slug.into(),
        url: format!("https://github.com/acme/{slug}"),
        default_branch: "main".into(),
        language: "rust".into(),
        mode: RepoMode::Poc,
        max_concurrent_jobs: 1,
        config: settings,
    })
    .await
    .unwrap();
}

fn on_app() -> Value {
    json!({"app": {}})
}

fn on_token() -> Value {
    json!({"fork": {"owner": "bot", "name": "blog", "token_env": "T"}})
}

fn sign(secret: &str, body: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(body);
    let hex: String = mac
        .finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("sha256={hex}")
}

async fn post(
    app: &Router,
    secret: &str,
    event: &str,
    delivery: &str,
    body: &Value,
) -> (StatusCode, Value) {
    let text = body.to_string();
    let request = Request::builder()
        .method("POST")
        .uri("/github")
        .header("x-github-event", event)
        .header("x-github-delivery", delivery)
        .header("x-hub-signature-256", sign(secret, text.as_bytes()))
        .body(Body::from(text))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn labeled(slug: &str, number: u64, author: &str) -> Value {
    json!({
        "action": "labeled",
        "label": { "name": "aegis" },
        "issue": { "number": number, "user": { "login": author, "type": "User" } },
        "repository": {
            "full_name": format!("acme/{slug}"),
            "html_url": format!("https://github.com/acme/{slug}")
        },
        "sender": { "login": author, "type": "User" }
    })
}

async fn job_count(s: &Store) -> usize {
    s.list_jobs(&JobFilter::default(), Page::first(100))
        .await
        .unwrap()
        .items
        .len()
}

#[tokio::test]
async fn the_verifying_secret_says_which_repos_a_delivery_is_for() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    repo(s, "shop", on_app()).await;
    repo(s, "blog", on_token()).await;
    let app = router(s.clone(), config("http://127.0.0.1:1"));

    // The repo's own webhook does not make jobs for a repo on the app path…
    let (status, reply) = post(&app, REPO_SECRET, "issues", "d-1", &labeled("shop", 1, "a")).await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_eq!(reply["data"]["status"], "ignored");
    assert!(
        reply["data"]["reason"]
            .as_str()
            .unwrap()
            .contains("the GitHub App's webhook"),
        "{reply}"
    );
    // …and the app's does not for a repo on the token path.
    let (_, reply) = post(&app, APP_SECRET, "issues", "d-2", &labeled("blog", 1, "a")).await;
    assert_eq!(reply["data"]["status"], "ignored", "{reply}");
    assert_eq!(job_count(s).await, 0);

    // Each from its own.
    let (status, reply) = post(&app, APP_SECRET, "issues", "d-3", &labeled("shop", 1, "a")).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{reply}");
    let (status, reply) = post(&app, REPO_SECRET, "issues", "d-4", &labeled("blog", 1, "a")).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{reply}");
    assert_eq!(job_count(s).await, 2);

    // Neither secret: refused.
    let (status, _) = post(&app, "guess", "issues", "d-5", &labeled("shop", 2, "a")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

fn installation(action: &str) -> Value {
    json!({
        "action": action,
        "installation": {
            "id": 5, "account": { "login": "acme" }, "repository_selection": "selected"
        },
        "repositories": [{ "full_name": "acme/shop" }, { "full_name": "acme/docs" }],
        "sender": { "login": "owner", "type": "User" }
    })
}

#[tokio::test]
async fn installation_events_count_from_the_app_and_a_suspended_one_takes_no_job() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    repo(s, "shop", on_app()).await;
    let app = router(s.clone(), config("http://127.0.0.1:1"));

    // From a repo's webhook, an installation event is not believed.
    let (_, reply) = post(
        &app,
        REPO_SECRET,
        "installation",
        "i-0",
        &installation("created"),
    )
    .await;
    assert_eq!(reply["data"]["status"], "ignored", "{reply}");
    assert_eq!(s.app_on_repo("acme", "shop").await.unwrap(), None);

    let (status, reply) = post(
        &app,
        APP_SECRET,
        "installation",
        "i-1",
        &installation("created"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_eq!(reply["data"]["status"], "recorded");
    assert_eq!(
        s.app_on_repo("acme", "shop").await.unwrap(),
        Some(AppOnRepo::Installed { installation: 5 })
    );
    assert_eq!(s.installed_unregistered().await.unwrap(), ["acme/docs"]);

    post(
        &app,
        APP_SECRET,
        "installation",
        "i-2",
        &installation("suspend"),
    )
    .await;
    let (_, reply) = post(&app, APP_SECRET, "issues", "d-1", &labeled("shop", 1, "a")).await;
    assert_eq!(reply["data"]["status"], "ignored", "{reply}");
    assert!(
        reply["data"]["reason"]
            .as_str()
            .unwrap()
            .contains("suspended"),
        "{reply}"
    );
    assert_eq!(job_count(s).await, 0);

    post(
        &app,
        APP_SECRET,
        "installation",
        "i-3",
        &installation("unsuspend"),
    )
    .await;
    let removed = json!({
        "action": "removed",
        "installation": { "id": 5, "account": { "login": "acme" }, "repository_selection": "selected" },
        "repository_selection": "selected",
        "repositories_added": [],
        "repositories_removed": [{ "full_name": "acme/shop" }]
    });
    post(
        &app,
        APP_SECRET,
        "installation_repositories",
        "i-4",
        &removed,
    )
    .await;
    let (_, reply) = post(&app, APP_SECRET, "issues", "d-2", &labeled("shop", 1, "a")).await;
    assert!(
        reply["data"]["reason"]
            .as_str()
            .unwrap()
            .contains("not given"),
        "{reply}"
    );

    post(
        &app,
        APP_SECRET,
        "installation",
        "i-5",
        &installation("created"),
    )
    .await;
    let (status, reply) = post(&app, APP_SECRET, "issues", "d-3", &labeled("shop", 1, "a")).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{reply}");
}

#[tokio::test]
async fn the_app_checks_who_may_ask_without_a_comment_token() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    repo(
        s,
        "shop",
        json!({"app": {}, "issue_trust": "collaborators"}),
    )
    .await;
    let github = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/shop/installation"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": 5})))
        .mount(&github)
        .await;
    Mock::given(method("POST"))
        .and(path("/app/installations/5/access_tokens"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "token": "ghs_comment",
            "expires_at": (chrono::Utc::now() + chrono::TimeDelta::hours(1)).to_rfc3339(),
        })))
        .mount(&github)
        .await;
    for (login, permission) in [("alice", "write"), ("mallory", "read")] {
        Mock::given(method("GET"))
            .and(path(format!(
                "/repos/acme/shop/collaborators/{login}/permission"
            )))
            .and(header("authorization", "Bearer ghs_comment"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"permission": permission})),
            )
            .mount(&github)
            .await;
    }
    let app = router(s.clone(), config(&github.uri()));

    let (_, reply) = post(
        &app,
        APP_SECRET,
        "issues",
        "d-1",
        &labeled("shop", 1, "mallory"),
    )
    .await;
    assert!(
        reply["data"]["reason"]
            .as_str()
            .unwrap()
            .contains("cannot write"),
        "{reply}"
    );
    let (status, reply) = post(
        &app,
        APP_SECRET,
        "issues",
        "d-2",
        &labeled("shop", 2, "alice"),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{reply}");
}

#[tokio::test]
async fn the_apps_own_comments_start_no_review_round() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    repo(s, "shop", on_app()).await;
    let app = router(s.clone(), config("http://127.0.0.1:1"));
    let comment = json!({
        "action": "created",
        "issue": {
            "number": 9, "state": "open", "html_url": "https://github.com/acme/shop/pull/9",
            "pull_request": { "html_url": "https://github.com/acme/shop/pull/9" },
            "user": { "login": "aegis-code[bot]", "type": "Bot" }
        },
        "comment": { "body": "/aegis fix", "user": { "login": "aegis-code[bot]", "type": "Bot" } },
        "repository": { "full_name": "acme/shop", "html_url": "https://github.com/acme/shop" },
        "sender": { "login": "aegis-code[bot]", "type": "Bot" }
    });
    let (_, reply) = post(&app, APP_SECRET, "issue_comment", "c-1", &comment).await;
    assert_eq!(reply["data"]["status"], "ignored", "{reply}");
    assert_eq!(reply["data"]["reason"], "sent by a bot", "{reply}");
}

/// #554: with the store as the server opens it, read by `load_at_start`
/// rather than built full, an app delivery verifies. The app's secret was
/// never read, and every app delivery was refused as badly signed.
#[tokio::test]
async fn an_app_delivery_verifies_with_the_secrets_read_at_start() {
    let Some(db) = test_db().await else { return };
    let section = ServerConfig::from_toml("[github_app]\napp_id = 42\n")
        .unwrap()
        .github_app
        .unwrap();
    let pem = pem();
    let secrets = Secrets::new(MemorySource::new([
        ("AEGIS_DATABASE_URL", "postgres://unused"),
        (APP_SECRET_ENV, APP_SECRET),
        (APP_KEY_ENV, pem.as_str()),
    ]));
    load_at_start(&secrets, aegis_server::Role::Api, Some(&section))
        .await
        .unwrap();
    let app = Arc::new(AppHandle::new(section, secrets.clone()).unwrap());
    let hooks = router(
        db.store.clone(),
        WebhookConfig {
            secrets,
            app: Some(app),
            ..config("http://127.0.0.1:9")
        },
    );
    let (status, body) = post(&hooks, APP_SECRET, "ping", "d-554", &json!({"zen": "hi"})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}
