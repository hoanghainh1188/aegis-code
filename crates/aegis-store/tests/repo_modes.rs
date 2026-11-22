//! #591: a repo's mode is any name the `repos.mode` CHECK allows, not
//! only `poc` and `customer-jp`; the migration that widened the CHECK
//! keeps the rows those two modes wrote.

use aegis_store::testing::test_db;
use aegis_store::{ModeName, NewRepo};

fn new_repo(slug: &str, mode: ModeName) -> NewRepo {
    NewRepo {
        slug: slug.into(),
        url: format!("https://github.com/example/{slug}"),
        default_branch: "main".into(),
        language: "rust".into(),
        mode,
        max_concurrent_jobs: 1,
        config: serde_json::json!({}),
    }
}

#[tokio::test]
async fn repos_take_a_mode_the_server_names() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    for (slug, mode) in [("eu", "customer-eu"), ("local", "local-ollama")] {
        let mode = ModeName::parse(mode).unwrap();
        let created = s.create_repo(&new_repo(slug, mode.clone())).await.unwrap();
        assert_eq!(created.mode, mode);
        let read = s.repo_by_slug(slug).await.unwrap().unwrap();
        assert_eq!(read.mode, mode);
        assert_eq!(s.repo(created.id).await.unwrap().unwrap().mode, mode);
    }
    db.drop().await;
}

#[tokio::test]
async fn the_database_refuses_a_mode_name_the_type_would() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    s.create_repo(&new_repo("app", ModeName::POC))
        .await
        .unwrap();
    for bad in [
        "UPDATE repos SET mode = 'Customer-EU'",
        "UPDATE repos SET mode = '-eu'",
        "UPDATE repos SET mode = ''",
        "UPDATE repos SET mode = repeat('a', 65)",
    ] {
        let err = db.raw(bad).await.expect_err(bad);
        let db_err = err.as_database_error().expect(bad);
        assert_eq!(db_err.code().as_deref(), Some("23514"), "{bad}");
        assert_eq!(db_err.constraint(), Some("repos_mode_check"), "{bad}");
    }
    assert_eq!(
        s.repo_by_slug("app").await.unwrap().unwrap().mode,
        ModeName::POC
    );
    db.drop().await;
}

#[tokio::test]
async fn widening_the_check_keeps_the_rows_of_the_two_old_modes() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    // The CHECK of `0001_init.sql`, with rows it allowed.
    db.raw(
        "ALTER TABLE repos DROP CONSTRAINT repos_mode_check;
         ALTER TABLE repos ADD CONSTRAINT repos_mode_check
             CHECK (mode IN ('poc', 'customer-jp'))",
    )
    .await
    .unwrap();
    s.create_repo(&new_repo("scrapy", ModeName::POC))
        .await
        .unwrap();
    s.create_repo(&new_repo("app-jp", ModeName::CUSTOMER_JP))
        .await
        .unwrap();
    db.raw(include_str!(
        "../migrations/20261121120000_repo_mode_slug.sql"
    ))
    .await
    .unwrap();
    assert_eq!(
        s.repo_by_slug("scrapy").await.unwrap().unwrap().mode,
        ModeName::POC
    );
    assert_eq!(
        s.repo_by_slug("app-jp").await.unwrap().unwrap().mode,
        ModeName::CUSTOMER_JP
    );
    let eu = ModeName::parse("customer-eu").unwrap();
    s.create_repo(&new_repo("app-eu", eu.clone()))
        .await
        .unwrap();
    assert_eq!(s.repo_by_slug("app-eu").await.unwrap().unwrap().mode, eu);
    db.drop().await;
}
