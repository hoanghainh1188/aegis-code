//! The GitHub App's installations (#540): what its events said is kept,
//! so a repo whose installation was suspended, removed or not given the
//! repo says why, and installed repos not yet registered can be listed.

use aegis_store::testing::test_db;
use aegis_store::{AppOnRepo, InstallationSeen, NewRepo, RepoMode, RepoSelection, Store};

fn seen(id: i64, account: &str, selection: RepoSelection, repos: &[&str]) -> InstallationSeen {
    InstallationSeen {
        id,
        account: account.into(),
        selection,
        repositories: repos.iter().map(|r| (*r).to_owned()).collect(),
    }
}

async fn register(s: &Store, slug: &str, url: &str) {
    s.create_repo(&NewRepo {
        slug: slug.into(),
        url: url.into(),
        default_branch: "main".into(),
        language: "rust".into(),
        mode: RepoMode::Poc,
        max_concurrent_jobs: 1,
        config: serde_json::json!({"app": {}}),
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn an_installation_follows_its_events() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    assert_eq!(s.app_on_repo("Acme", "shop").await.unwrap(), None);

    s.installation_seen(&seen(7, "Acme", RepoSelection::Selected, &["Acme/Shop"]))
        .await
        .unwrap();
    let installed = Some(AppOnRepo::Installed { installation: 7 });
    assert_eq!(s.app_on_repo("acme", "shop").await.unwrap(), installed);
    let not_given = s.app_on_repo("acme", "blog").await.unwrap().unwrap();
    assert_eq!(not_given, AppOnRepo::NotGiven { installation: 7 });
    assert!(
        not_given
            .refusal("acme/blog")
            .unwrap()
            .contains("not given")
    );

    s.installation_repos_changed(
        7,
        "Acme",
        RepoSelection::Selected,
        &["acme/blog".into()],
        &["acme/shop".into()],
    )
    .await
    .unwrap();
    assert_eq!(
        s.app_on_repo("acme", "blog").await.unwrap(),
        Some(AppOnRepo::Installed { installation: 7 })
    );
    assert_eq!(
        s.app_on_repo("acme", "shop").await.unwrap(),
        Some(AppOnRepo::NotGiven { installation: 7 })
    );

    assert!(s.installation_suspended(7, true).await.unwrap());
    let suspended = s.app_on_repo("acme", "blog").await.unwrap().unwrap();
    assert_eq!(suspended, AppOnRepo::Suspended { installation: 7 });
    assert!(
        suspended
            .refusal("acme/blog")
            .unwrap()
            .contains("suspended")
    );
    assert!(s.installation_suspended(7, false).await.unwrap());
    assert!(!s.installation_suspended(99, true).await.unwrap());

    s.installation_removed(7, "Acme").await.unwrap();
    assert_eq!(
        s.app_on_repo("acme", "blog").await.unwrap(),
        Some(AppOnRepo::Removed { installation: 7 })
    );
    // Installed again (a new installation on the same account): it wins.
    s.installation_seen(&seen(8, "acme", RepoSelection::All, &[]))
        .await
        .unwrap();
    assert_eq!(
        s.app_on_repo("acme", "anything").await.unwrap(),
        Some(AppOnRepo::Installed { installation: 8 })
    );
}

#[tokio::test]
async fn installed_repos_not_registered_are_listed() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    s.installation_seen(&seen(
        3,
        "acme",
        RepoSelection::Selected,
        &["acme/shop", "acme/blog", "acme/docs"],
    ))
    .await
    .unwrap();
    s.installation_seen(&seen(4, "other", RepoSelection::Selected, &["other/api"]))
        .await
        .unwrap();
    s.installation_suspended(4, true).await.unwrap();
    register(s, "shop", "https://github.com/Acme/shop.git").await;
    assert_eq!(
        s.installed_unregistered().await.unwrap(),
        ["acme/blog", "acme/docs"]
    );
}

#[tokio::test]
async fn bad_ids_and_accounts_are_refused() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    assert!(
        s.installation_seen(&seen(0, "acme", RepoSelection::All, &[]))
            .await
            .is_err()
    );
    assert!(s.installation_removed(1, "").await.is_err());
}
