//! "Xong khi" of #541 (ADR 0017 §6): two tenants install the same GitHub
//! App on their own orgs, and neither can register the other's repo nor
//! get a token for it; an installation bound to no tenant gets no token.

use std::sync::Arc;

use aegis_lifecycle::RepoSettings;
use aegis_secrets::Secrets;
use aegis_server::config::ServerConfig;
use aegis_server::github_app::{AppHandle, check_binding, read_token};
use aegis_store::testing::test_db;
use aegis_store::{Actor, InstallationSeen, RepoSelection, Store};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const KEY_ENV: &str = "AEGIS_GITHUB_APP_PRIVATE_KEY";

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

async fn installed(s: &Store, github: &MockServer, id: i64, org: &str, repo: &str) {
    s.installation_seen(&InstallationSeen {
        id,
        account: org.into(),
        selection: RepoSelection::Selected,
        repositories: vec![format!("{org}/{repo}")],
    })
    .await
    .unwrap();
    Mock::given(method("GET"))
        .and(path(format!("/repos/{org}/{repo}/installation")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": id})))
        .mount(github)
        .await;
}

async fn mints(github: &MockServer, id: i64, token: &str, times: u64) {
    Mock::given(method("POST"))
        .and(path(format!("/app/installations/{id}/access_tokens")))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "token": token,
            "expires_at": (chrono::Utc::now() + chrono::TimeDelta::hours(1)).to_rfc3339(),
        })))
        .expect(times)
        .mount(github)
        .await;
}

#[tokio::test]
async fn neither_tenant_gets_a_token_for_the_others_repo() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let actor = Actor::cli();
    let acme = s.create_tenant_by("acme", "Acme", &actor).await.unwrap();
    let globex = s
        .create_tenant_by("globex", "Globex", &actor)
        .await
        .unwrap();
    let github = MockServer::start().await;
    installed(s, &github, 5, "acme", "shop").await;
    installed(s, &github, 6, "globex", "api").await;
    installed(s, &github, 7, "initech", "tps").await;
    s.bind_installation_by(5, Some(acme.id), &actor)
        .await
        .unwrap();
    s.bind_installation_by(6, Some(globex.id), &actor)
        .await
        .unwrap();
    // Each installation's token is minted for its own tenant only, once
    // (then cached); 7 is bound to nobody and gets none.
    mints(&github, 5, "ghs_acme", 1).await;
    mints(&github, 6, "ghs_globex", 1).await;
    mints(&github, 7, "ghs_initech", 0).await;

    let pem = pem();
    let secrets = Secrets::fixed([(KEY_ENV, pem.as_str())]);
    let section = ServerConfig::from_toml(&format!(
        "[github_app]\napp_id = 42\napi = \"{}\"\n",
        github.uri()
    ))
    .unwrap()
    .github_app
    .unwrap();
    let app = Arc::new(
        AppHandle::new(section, secrets.clone())
            .unwrap()
            .with_store(s.clone()),
    );
    let settings: RepoSettings = serde_json::from_value(json!({"app": {}})).unwrap();
    let (shop, api, tps) = (
        "https://github.com/acme/shop",
        "https://github.com/globex/api",
        "https://github.com/initech/tps",
    );

    // Registration: only into the tenant the installation is bound to.
    assert!(check_binding(s, &settings, shop, acme.id).await.is_ok());
    let err = check_binding(s, &settings, shop, globex.id)
        .await
        .unwrap_err();
    assert!(err.contains("another tenant"), "{err}");
    let err = check_binding(s, &settings, tps, acme.id).await.unwrap_err();
    assert!(err.contains("not bound"), "{err}");
    let err = check_binding(s, &settings, "https://github.com/hooli/x", acme.id)
        .await
        .unwrap_err();
    assert!(err.contains("not known to be installed"), "{err}");

    // Tokens: asked at each one, refused across tenants even once cached.
    let token = |url: &'static str, tenant| {
        let (app, secrets, settings) = (Arc::clone(&app), secrets.clone(), settings.clone());
        async move { read_token(&secrets, Some(&app), &settings, url, tenant).await }
    };
    assert_eq!(
        token(shop, acme.id).await.unwrap().as_deref(),
        Some("ghs_acme")
    );
    assert_eq!(
        token(api, globex.id).await.unwrap().as_deref(),
        Some("ghs_globex")
    );
    let err = token(shop, globex.id).await.unwrap_err();
    assert!(err.contains("another tenant"), "{err}");
    let err = token(api, acme.id).await.unwrap_err();
    assert!(err.contains("another tenant"), "{err}");
    let err = token(tps, acme.id).await.unwrap_err();
    assert!(err.contains("not bound"), "{err}");

    // Unbinding takes effect at the next token, cache or not.
    s.bind_installation_by(5, None, &actor).await.unwrap();
    let err = token(shop, acme.id).await.unwrap_err();
    assert!(err.contains("not bound"), "{err}");
}
