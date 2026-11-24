//! Runbooks and business rules synced from a repo's main branch (#660).

use aegis_store::testing::test_db;
use aegis_store::{
    Actor, ActorKind, GitSync, MemoryKind, ModeName, NewRepo, NewRepoMemory, Page, RepoId, Store,
};

fn new_repo(slug: &str) -> NewRepo {
    NewRepo {
        slug: slug.into(),
        url: format!("https://github.com/acme/{slug}"),
        default_branch: "main".into(),
        language: "rust".into(),
        mode: ModeName::POC,
        max_concurrent_jobs: 1,
        config: serde_json::json!({}),
    }
}

fn synced(repo_id: RepoId, kind: MemoryKind, source_ref: &str, summary: &str) -> NewRepoMemory {
    NewRepoMemory {
        repo_id,
        kind,
        source_ref: source_ref.into(),
        title: format!("Title of {source_ref}"),
        summary: summary.into(),
        paths: Vec::new(),
        keywords: "docs".into(),
        job_id: None,
        scope: None,
    }
}

async fn refs(s: &Store, repo: RepoId) -> Vec<String> {
    let mut all: Vec<String> = s
        .list_repo_memories(repo, None, Page::first(100))
        .await
        .unwrap()
        .items
        .into_iter()
        .map(|m| m.source_ref)
        .collect();
    all.sort();
    all
}

#[tokio::test]
async fn a_sync_adds_updates_keeps_and_removes_git_memories_only() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let repo = s.create_repo(&new_repo("billing")).await.unwrap().id;
    let other = s.create_repo(&new_repo("portal")).await.unwrap().id;
    let actor = Actor::memory_sync();
    // A memory written by hand, and another repo's synced one: no sync of
    // `billing` touches them.
    s.upsert_repo_memory(&synced(
        repo,
        MemoryKind::Runbook,
        "runbook:manual",
        "Kept by hand.",
    ))
    .await
    .unwrap();
    s.sync_git_memories(
        other,
        &[synced(
            other,
            MemoryKind::Runbook,
            "git:docs/a.md#x",
            "Portal.",
        )],
        &actor,
    )
    .await
    .unwrap();

    let first = [
        synced(
            repo,
            MemoryKind::Runbook,
            "git:docs/runbooks/db.md#failover",
            "Promote the replica.",
        ),
        synced(
            repo,
            MemoryKind::BusinessRule,
            "git:docs/invariants.md#money",
            "Integers in minor units.",
        ),
        synced(
            repo,
            MemoryKind::Runbook,
            "git:docs/runbooks/db.md#backup",
            "Nightly.",
        ),
    ];
    let done = s.sync_git_memories(repo, &first, &actor).await.unwrap();
    assert_eq!(
        done,
        GitSync {
            added: 3,
            ..GitSync::default()
        }
    );

    // The same files again write nothing.
    let again = s.sync_git_memories(repo, &first, &actor).await.unwrap();
    assert_eq!(
        again,
        GitSync {
            unchanged: 3,
            ..GitSync::default()
        }
    );

    // One section changed, one removed from the branch.
    let second = [
        synced(
            repo,
            MemoryKind::Runbook,
            "git:docs/runbooks/db.md#failover",
            "Promote the replica, then fence the old primary.",
        ),
        synced(
            repo,
            MemoryKind::BusinessRule,
            "git:docs/invariants.md#money",
            "Integers in minor units.",
        ),
    ];
    let done = s.sync_git_memories(repo, &second, &actor).await.unwrap();
    assert_eq!(
        done,
        GitSync {
            updated: 1,
            unchanged: 1,
            removed: 1,
            ..GitSync::default()
        }
    );
    assert_eq!(
        refs(s, repo).await,
        [
            "git:docs/invariants.md#money",
            "git:docs/runbooks/db.md#failover",
            "runbook:manual",
        ]
    );
    assert_eq!(refs(s, other).await, ["git:docs/a.md#x"]);
    let failover = s
        .list_repo_memories(repo, Some(MemoryKind::Runbook), Page::first(10))
        .await
        .unwrap()
        .items
        .into_iter()
        .find(|m| m.source_ref.ends_with("#failover"))
        .unwrap();
    assert!(failover.summary.contains("fence the old primary"));

    // Nothing named any more: every `git:` memory goes, the hand-written
    // one stays.
    let done = s.sync_git_memories(repo, &[], &actor).await.unwrap();
    assert_eq!(done.removed, 2);
    assert_eq!(refs(s, repo).await, ["runbook:manual"]);

    let log = s
        .audit_log(Some(repo), Page::first(100))
        .await
        .unwrap()
        .items;
    let syncs: Vec<_> = log
        .iter()
        .filter(|e| e.action == "memory.sync" && e.repo_id == Some(repo))
        .collect();
    assert_eq!(syncs.len(), 4);
    assert!(
        syncs
            .iter()
            .all(|e| e.actor.kind == ActorKind::System && e.actor.name == "memory-sync")
    );
    assert!(log.iter().any(|e| e.action == "memory.delete"
        && e.detail["source_ref"] == "git:docs/runbooks/db.md#backup"));
}

#[tokio::test]
async fn a_sync_writes_only_git_runbooks_and_rules_of_its_repo() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let repo = s.create_repo(&new_repo("billing")).await.unwrap().id;
    let other = s.create_repo(&new_repo("portal")).await.unwrap().id;
    let actor = Actor::memory_sync();
    for bad in [
        synced(repo, MemoryKind::Runbook, "runbook:x", "Not from git."),
        synced(
            repo,
            MemoryKind::MergedPr,
            "git:docs/a.md#x",
            "Not a runbook.",
        ),
        synced(
            other,
            MemoryKind::Runbook,
            "git:docs/a.md#x",
            "Another repo's.",
        ),
    ] {
        let refused = s.sync_git_memories(repo, &[bad], &actor).await;
        assert!(refused.is_err(), "{refused:?}");
    }
    assert!(refs(s, repo).await.is_empty());
    assert!(refs(s, other).await.is_empty());
}
