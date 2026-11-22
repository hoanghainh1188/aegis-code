//! The GitHub App path of ADR 0017 (#538) against a fake GitHub: the
//! app's JWT finds the installation, installation tokens are minted for
//! one repo and one use, cached until ten minutes before they expire, and
//! never show up in an error.

use std::sync::Arc;

use aegis_pr::{AppKey, GitHub, GitHubApp, PrError, TokenSource, TokenUse};
use aws_lc_rs::encoding::{AsDer, Pkcs8V1Der};
use aws_lc_rs::rsa::{KeyPair, KeySize};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::json;
use time::format_description::well_known::Rfc3339;
use time::{Duration, OffsetDateTime};
use wiremock::matchers::{body_partial_json, header, header_regex, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const JWT: &str = r"^Bearer [A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+$";

fn key() -> AppKey {
    let pair = KeyPair::generate(KeySize::Rsa2048).unwrap();
    let der = AsDer::<Pkcs8V1Der>::as_der(&pair).unwrap();
    let pem = format!(
        "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n",
        STANDARD.encode(der.as_ref())
    );
    AppKey::from_pem(&pem).unwrap()
}

fn app(server: &MockServer) -> Arc<GitHubApp> {
    Arc::new(GitHubApp::new(&server.uri(), "123456", key()).unwrap())
}

async fn installed(server: &MockServer, times: u64) {
    Mock::given(method("GET"))
        .and(path("/repos/acme/shop/installation"))
        .and(header_regex("authorization", JWT))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": 7})))
        .expect(times)
        .mount(server)
        .await;
}

fn minted(token: &str, life: Duration) -> ResponseTemplate {
    ResponseTemplate::new(201).set_body_json(json!({
        "token": token,
        "expires_at": (OffsetDateTime::now_utc() + life).format(&Rfc3339).unwrap(),
    }))
}

fn source(app: &Arc<GitHubApp>, use_: TokenUse) -> TokenSource {
    TokenSource::App {
        app: Arc::clone(app),
        owner: "acme".into(),
        repo: "shop".into(),
        use_,
        holder: None,
    }
}

#[tokio::test]
async fn a_token_is_minted_for_one_repo_and_use_then_cached() {
    let server = MockServer::start().await;
    installed(&server, 1).await;
    Mock::given(method("POST"))
        .and(path("/app/installations/7/access_tokens"))
        .and(header_regex("authorization", JWT))
        .and(body_partial_json(json!({
            "repositories": ["shop"],
            "permissions": {"contents": "read", "issues": "read", "metadata": "read"},
        })))
        .respond_with(minted("ghs_read", Duration::hours(1)))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/shop/collaborators/alice/permission"))
        .and(header("authorization", "Bearer ghs_read"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"permission": "write"})))
        .expect(2)
        .mount(&server)
        .await;

    let app = app(&server);
    let github =
        GitHub::with_source(&server.uri(), "acme", "shop", source(&app, TokenUse::Read)).unwrap();
    for _ in 0..2 {
        assert_eq!(github.permission("alice").await.unwrap(), "write");
    }
}

#[tokio::test]
async fn each_use_gets_its_own_token_with_its_permissions() {
    let server = MockServer::start().await;
    installed(&server, 1).await;
    Mock::given(method("POST"))
        .and(path("/app/installations/7/access_tokens"))
        .and(body_partial_json(json!({
            "permissions": {"contents": "write", "pull_requests": "write"},
        })))
        .respond_with(minted("ghs_publish", Duration::hours(1)))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/app/installations/7/access_tokens"))
        .and(body_partial_json(
            json!({"permissions": {"actions": "read"}}),
        ))
        .respond_with(minted("ghs_ci", Duration::hours(1)))
        .expect(1)
        .mount(&server)
        .await;

    let app = app(&server);
    let publish = source(&app, TokenUse::Publish);
    assert_eq!(publish.token().await.unwrap(), "ghs_publish");
    assert_eq!(
        publish.for_use(TokenUse::Ci).token().await.unwrap(),
        "ghs_ci"
    );
    assert_eq!(publish.token().await.unwrap(), "ghs_publish");
}

#[tokio::test]
async fn a_token_ten_minutes_from_expiry_is_minted_again() {
    let server = MockServer::start().await;
    installed(&server, 1).await;
    Mock::given(method("POST"))
        .and(path("/app/installations/7/access_tokens"))
        .respond_with(minted("ghs_short", Duration::minutes(9)))
        .expect(2)
        .mount(&server)
        .await;

    let app = app(&server);
    for _ in 0..2 {
        app.token("acme", "shop", TokenUse::Read, None)
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn a_refused_token_drops_the_installation_and_says_so_without_secrets() {
    let server = MockServer::start().await;
    installed(&server, 2).await;
    Mock::given(method("POST"))
        .and(path("/app/installations/7/access_tokens"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({"message": "Not Found"})))
        .expect(2)
        .mount(&server)
        .await;

    let app = app(&server);
    for _ in 0..2 {
        let err = app
            .token("acme", "shop", TokenUse::Read, None)
            .await
            .unwrap_err();
        let text = err.to_string();
        assert!(matches!(err, PrError::GitHub { status: 404, .. }), "{text}");
        assert!(text.contains("installation 7"), "{text}");
        assert!(!text.contains("Bearer") && !text.contains("eyJ"), "{text}");
    }
}

#[tokio::test]
async fn a_repo_without_the_app_is_named_and_not_cached() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/shop/installation"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({"message": "Not Found"})))
        .expect(2)
        .mount(&server)
        .await;

    let app = app(&server);
    for _ in 0..2 {
        let err = app.installation("acme", "shop").await.unwrap_err();
        assert!(
            err.to_string().contains("not installed on `acme/shop`"),
            "{err}"
        );
    }
}

#[tokio::test]
async fn an_api_error_on_the_app_path_never_carries_the_token() {
    let server = MockServer::start().await;
    installed(&server, 1).await;
    Mock::given(method("POST"))
        .and(path("/app/installations/7/access_tokens"))
        .respond_with(minted("ghs_secret_value", Duration::hours(1)))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/shop/collaborators/alice/permission"))
        .respond_with(
            ResponseTemplate::new(403).set_body_json(json!({"message": "Resource not accessible"})),
        )
        .mount(&server)
        .await;

    let app = app(&server);
    let github =
        GitHub::with_source(&server.uri(), "acme", "shop", source(&app, TokenUse::Read)).unwrap();
    let err = github.permission("alice").await.unwrap_err();
    assert!(!err.to_string().contains("ghs_secret_value"), "{err}");
    assert!(!format!("{github:?}").contains("ghs_secret_value"));
    // The push remote is on the web host; the token still comes from the
    // app, asked for now.
    let on_github = GitHub::with_source(
        "https://api.github.com",
        "acme",
        "shop",
        source(&app, TokenUse::Publish),
    )
    .unwrap();
    let remote = on_github.remote_now().await.unwrap();
    assert!(
        !format!("{remote:?}").contains("ghs_secret_value"),
        "{remote:?}"
    );
    assert!(matches!(on_github.remote(), Err(PrError::App(_))));
}

#[tokio::test]
async fn a_source_for_another_repo_is_refused() {
    let server = MockServer::start().await;
    let app = app(&server);
    let err = GitHub::with_source(&server.uri(), "acme", "other", source(&app, TokenUse::Read))
        .unwrap_err();
    assert!(matches!(err, PrError::App(_)), "{err}");
}

#[test]
fn the_token_path_is_unchanged() {
    let github = GitHub::new("https://api.github.com", "acme", "shop", "ghp_fixed").unwrap();
    assert!(github.remote().is_ok());
    assert!(!format!("{github:?}").contains("ghp_fixed"));
}

#[tokio::test]
async fn the_gate_is_asked_before_every_token_and_can_refuse_it() {
    let server = MockServer::start().await;
    installed(&server, 1).await;
    Mock::given(method("POST"))
        .and(path("/app/installations/7/access_tokens"))
        .respond_with(minted("ghs_gated", Duration::hours(1)))
        .expect(1)
        .mount(&server)
        .await;
    let asked = Arc::new(std::sync::Mutex::new(Vec::new()));
    let gate: aegis_pr::Gate = {
        let asked = Arc::clone(&asked);
        Arc::new(move |installation, holder| {
            asked.lock().unwrap().push((installation, holder));
            Box::pin(async move {
                if holder == Some(1) {
                    Ok(())
                } else {
                    Err(format!("installation {installation} is not this holder's"))
                }
            })
        })
    };
    let app = Arc::new(
        GitHubApp::new(&server.uri(), "123456", key())
            .unwrap()
            .with_gate(gate),
    );
    assert_eq!(
        app.token("acme", "shop", TokenUse::Read, Some(1))
            .await
            .unwrap(),
        "ghs_gated"
    );
    // Cached for holder 1, still refused to another: the gate is asked
    // before the cache too.
    let err = app
        .token("acme", "shop", TokenUse::Read, Some(2))
        .await
        .unwrap_err();
    assert!(matches!(err, PrError::App(_)), "{err}");
    assert!(err.to_string().contains("not this holder's"), "{err}");
    assert!(
        app.token("acme", "shop", TokenUse::Read, None)
            .await
            .is_err()
    );
    assert_eq!(
        *asked.lock().unwrap(),
        [(7, Some(1)), (7, Some(2)), (7, None)]
    );
}
