//! Secrets at start (#30): a missing one stops the start, naming it and no
//! value; with a fake store standing in for Secrets Manager.

use std::path::Path;

use aegis_lifecycle::{ModeConfig, Modes};
use aegis_secrets::{MemorySource, Secrets};
use aegis_server::Role;
use aegis_server::config::ServerConfig;
use aegis_server::lifecycle::LifecycleRunner;
use aegis_server::secrets::{DATABASE_URL, load_at_start};
use aegis_server::webhook::{GITHUB_SECRET_ENV, WebhookConfig};
use aegis_store::testing::test_db;

const DB_URL: &str = "postgres://aegis:p4ssw0rd-not-real@db.internal/aegis";

#[tokio::test]
async fn the_start_stops_naming_a_missing_database_url() {
    let secrets = Secrets::new(MemorySource::new([(GITHUB_SECRET_ENV, "hook-s3cret")]));
    let err = load_at_start(&secrets, Role::All, None).await.unwrap_err();
    assert!(err.contains(DATABASE_URL), "{err}");
    assert!(!err.contains("hook-s3cret"), "{err}");
}

#[tokio::test]
async fn webhook_secrets_are_optional_and_read_at_start() {
    let source = MemorySource::new([(DATABASE_URL, DB_URL), (GITHUB_SECRET_ENV, "hook")]);
    let secrets = Secrets::new(source.clone());
    load_at_start(&secrets, Role::Api, None).await.unwrap();
    assert_eq!(secrets.get(GITHUB_SECRET_ENV).unwrap().expose(), "hook");
    // Read once at start, from the store, not on every delivery.
    let fetched = source.fetches();
    let config = WebhookConfig {
        secrets: secrets.clone(),
        ..WebhookConfig::from_env()
    };
    assert!(config.secrets.get(GITHUB_SECRET_ENV).is_some());
    assert_eq!(source.fetches(), fetched);
    let printed = format!("{config:?} {secrets:?}");
    assert!(
        !printed.contains("p4ssw0rd") && !printed.contains("\"hook\""),
        "{printed}"
    );
}

/// #554: the app's webhook secret is read at start, like the repo
/// webhooks'; the webhooks only check a secret the store has read.
#[tokio::test]
async fn the_apps_webhook_secret_is_read_at_start_with_github_app() {
    const APP_SECRET_ENV: &str = "AEGIS_GITHUB_APP_WEBHOOK_SECRET";
    let section = ServerConfig::from_toml("[github_app]\napp_id = 42\n")
        .unwrap()
        .github_app
        .unwrap();
    let source = || MemorySource::new([(DATABASE_URL, DB_URL), (APP_SECRET_ENV, "app-hook")]);

    let with_app = Secrets::new(source());
    load_at_start(&with_app, Role::Api, Some(&section))
        .await
        .unwrap();
    assert_eq!(with_app.get(APP_SECRET_ENV).unwrap().expose(), "app-hook");

    // A worker serves no webhook; without `[github_app]` it is not asked for.
    let worker = Secrets::new(source());
    load_at_start(&worker, Role::Worker, Some(&section))
        .await
        .unwrap();
    assert!(worker.get(APP_SECRET_ENV).is_none());
    let without_app = Secrets::new(source());
    load_at_start(&without_app, Role::Api, None).await.unwrap();
    assert!(without_app.get(APP_SECRET_ENV).is_none());

    // Still optional: an instance whose app secret is missing starts.
    let missing = Secrets::new(MemorySource::new([(DATABASE_URL, DB_URL)]));
    load_at_start(&missing, Role::Api, Some(&section))
        .await
        .unwrap();
}

fn poc_modes() -> Modes {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let poc = ModeConfig::load(&root.join("config/poc.toml"), &root).unwrap();
    Modes::new(Some(poc), None).unwrap()
}

#[tokio::test]
async fn a_worker_does_not_start_without_its_modes_api_key() {
    let Some(db) = test_db().await else { return };
    let err = LifecycleRunner::new(db.store.clone(), poc_modes(), Secrets::none())
        .await
        .err()
        .expect("no API key");
    assert!(
        err.contains("mode poc") && err.contains("ANTHROPIC_API_KEY"),
        "{err}"
    );

    let secrets = Secrets::new(MemorySource::new([("ANTHROPIC_API_KEY", "sk-not-real")]));
    assert!(
        LifecycleRunner::new(db.store.clone(), poc_modes(), secrets)
            .await
            .is_ok()
    );
    db.drop().await;
}
