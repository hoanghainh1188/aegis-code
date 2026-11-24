//! #599: a repo's pre-flight check from its page: one at a time, its items
//! as they come, how it ended, and audited.

use aegis_store::testing::test_db;
use aegis_store::{
    Actor, MAX_PREFLIGHT_ITEMS, ModeName, NewRepo, PreflightItem, PreflightState, RepoId,
};

fn item(name: &str, status: &str) -> PreflightItem {
    PreflightItem {
        category: "Git".into(),
        name: name.into(),
        status: status.into(),
        detail: format!("{name} detail"),
        is_blocking: status == "fail",
    }
}

#[tokio::test]
async fn a_check_runs_once_at_a_time_and_keeps_its_items() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let repo = s
        .create_repo(&NewRepo {
            slug: "shop".into(),
            url: "https://github.com/acme/shop".into(),
            default_branch: "main".into(),
            language: "rust".into(),
            mode: ModeName::POC,
            max_concurrent_jobs: 1,
            config: serde_json::json!({}),
        })
        .await
        .unwrap()
        .id;
    assert!(s.repo_preflight(repo).await.unwrap().is_none());
    let admin = Actor::cli();
    assert!(s.start_preflight_by(repo, &admin).await.unwrap());
    assert!(
        !s.start_preflight_by(repo, &admin).await.unwrap(),
        "one is running"
    );
    s.add_preflight_item(repo, &item("Repository Path", "pass"))
        .await
        .unwrap();
    s.add_preflight_item(repo, &item("Lockfile", "warn"))
        .await
        .unwrap();
    let running = s.repo_preflight(repo).await.unwrap().unwrap();
    assert_eq!(running.state, PreflightState::Running);
    assert_eq!(running.items.len(), 2);
    assert_eq!(running.items[1], item("Lockfile", "warn"));
    assert!(running.finished_at.is_none());

    s.finish_preflight(repo, PreflightState::Warned)
        .await
        .unwrap();
    let done = s.repo_preflight(repo).await.unwrap().unwrap();
    assert_eq!(done.state, PreflightState::Warned);
    assert!(done.state.ready());
    assert!(done.finished_at.is_some());
    // An item after the end is not kept.
    s.add_preflight_item(repo, &item("Late", "pass"))
        .await
        .unwrap();
    assert_eq!(
        s.repo_preflight(repo).await.unwrap().unwrap().items.len(),
        2
    );
    assert!(
        s.finish_preflight(repo, PreflightState::Running)
            .await
            .is_err()
    );

    // Another starts afresh.
    assert!(s.start_preflight_by(repo, &admin).await.unwrap());
    let again = s.repo_preflight(repo).await.unwrap().unwrap();
    assert!(again.items.is_empty());
    for i in 0..MAX_PREFLIGHT_ITEMS + 5 {
        s.add_preflight_item(repo, &item(&format!("i{i}"), "pass"))
            .await
            .unwrap();
    }
    assert_eq!(
        s.repo_preflight(repo).await.unwrap().unwrap().items.len(),
        MAX_PREFLIGHT_ITEMS
    );
    s.finish_preflight(repo, PreflightState::Failed)
        .await
        .unwrap();
    assert!(!s.repo_preflight(repo).await.unwrap().unwrap().state.ready());

    let audited = db
        .raw("SELECT 1 FROM audit_log WHERE action = 'repo.preflight'")
        .await
        .unwrap();
    assert_eq!(audited, 2);
    assert!(
        s.start_preflight_by(RepoId(9_999), &admin).await.is_err(),
        "no such repo"
    );
    db.drop().await;
}
