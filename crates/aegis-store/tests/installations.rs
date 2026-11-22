//! The GitHub App's installations (#540): what its events said is kept,
//! so a repo whose installation was suspended, removed or not given the
//! repo says why, and installed repos not yet registered can be listed.

use aegis_store::testing::test_db;
use aegis_store::{AppOnRepo, InstallationSeen, ModeName, NewRepo, RepoSelection, Store};

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
        mode: ModeName::POC,
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

#[tokio::test]
async fn an_installation_is_bound_to_one_tenant_and_audited() {
    use aegis_store::{Actor, StoreError, TenantId, binding_refusal};
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let actor = Actor::cli();
    let acme = s.create_tenant_by("acme", "Acme", &actor).await.unwrap();
    let other = s.create_tenant_by("other", "Other", &actor).await.unwrap();
    s.installation_seen(&seen(5, "acme", RepoSelection::All, &[]))
        .await
        .unwrap();
    assert_eq!(s.installation_tenant(5).await.unwrap(), Some(None));
    assert_eq!(s.installation_tenant(6).await.unwrap(), None);
    for (bound, why) in [(None, "not recorded"), (Some(None), "not bound")] {
        assert!(binding_refusal(5, bound, acme.id).unwrap().contains(why));
    }

    s.bind_installation_by(5, Some(acme.id), &actor)
        .await
        .unwrap();
    assert_eq!(s.installation_tenant(5).await.unwrap(), Some(Some(acme.id)));
    assert_eq!(binding_refusal(5, Some(Some(acme.id)), acme.id), None);
    assert!(
        binding_refusal(5, Some(Some(acme.id)), other.id)
            .unwrap()
            .contains("another tenant")
    );
    // Bound again to the same: fine. To another: unbind first.
    s.bind_installation_by(5, Some(acme.id), &actor)
        .await
        .unwrap();
    assert!(matches!(
        s.bind_installation_by(5, Some(other.id), &actor).await,
        Err(StoreError::Conflict(_))
    ));
    s.bind_installation_by(5, None, &actor).await.unwrap();
    s.bind_installation_by(5, Some(other.id), &actor)
        .await
        .unwrap();
    assert!(matches!(
        s.bind_installation_by(9, Some(acme.id), &actor).await,
        Err(StoreError::NotFound(_))
    ));
    assert!(
        s.bind_installation_by(5, Some(TenantId(999)), &actor)
            .await
            .is_err()
    );
    let listed = s.installations().await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].tenant, Some(other.id));
    let audited = s
        .audit_log_where(
            &aegis_store::AuditFilter {
                action: Some("installation.tenant".into()),
                ..Default::default()
            },
            aegis_store::Page::first(50),
        )
        .await
        .unwrap()
        .items;
    assert_eq!(
        audited
            .iter()
            .filter(|e| e.action == "installation.tenant")
            .count(),
        4
    );
}
