//! #589: the language of a repo's issue comments, looked up as its jobs'
//! is, repo → tenant → `[web] default_locale`, and a tenant's default set
//! from the command line.

use aegis_pr::Locale;
use aegis_server::locale::Locales;
use aegis_server::tenant_cli;
use aegis_store::testing::test_db;
use aegis_store::{Actor, ModeName, NewRepo, RepoId, Store, TenantId};
use serde_json::{Value, json};

async fn repo_in(s: &Store, slug: &str, tenant: TenantId, config: Value) -> RepoId {
    s.create_repo_in_by(
        &NewRepo {
            slug: slug.into(),
            url: format!("https://github.com/x/{slug}"),
            default_branch: "main".into(),
            language: "rust".into(),
            mode: ModeName::POC,
            max_concurrent_jobs: 1,
            config,
        },
        tenant,
        &Actor::cli(),
    )
    .await
    .unwrap()
    .id
}

fn args(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_owned()).collect()
}

#[tokio::test]
async fn a_repo_writes_in_its_own_then_its_tenants_then_the_servers_language() {
    let Some(db) = test_db().await else {
        return;
    };
    let s = &db.store;
    let acme = s
        .create_tenant_by("acme", "ACME", &Actor::cli())
        .await
        .unwrap();
    let said = tenant_cli::run(s, &args(&["locale", "acme", "ja"]))
        .await
        .unwrap();
    assert!(said.contains("`ja`"), "{said}");

    let own = repo_in(s, "own", acme.id, json!({"locale": "vi"})).await;
    let of_tenant = repo_in(s, "of-tenant", acme.id, json!({})).await;
    let plain = repo_in(s, "plain", TenantId::DEFAULT, json!({})).await;
    let locales = Locales::new(s.clone(), Locale::En);
    assert_eq!(locales.of(own).await, Locale::Vi);
    assert_eq!(locales.of(of_tenant).await, Locale::Ja);
    assert_eq!(locales.of(plain).await, Locale::En);
    // A server whose default is Japanese; no repo at all is the default.
    assert_eq!(
        Locales::new(s.clone(), Locale::Ja).of(plain).await,
        Locale::Ja
    );
    assert_eq!(locales.of(RepoId(999_999)).await, Locale::En);

    // `none` gives the tenant's repos back to the server's default.
    tenant_cli::run(s, &args(&["locale", "acme", "none"]))
        .await
        .unwrap();
    assert_eq!(locales.of(of_tenant).await, Locale::En);
    let listed = tenant_cli::run(s, &args(&["list"])).await.unwrap();
    assert!(listed.contains("locale the server's default"), "{listed}");
    // Codes are checked before the store and by it.
    assert!(
        tenant_cli::run(s, &args(&["locale", "acme", "fr"]))
            .await
            .is_err()
    );
    assert!(
        s.set_tenant_locale_by(acme.id, Some("fr"), &Actor::cli())
            .await
            .is_err()
    );
    db.drop().await;
}
