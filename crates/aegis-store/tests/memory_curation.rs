//! Curation of a repo's memory (#658): what each job was given, and a
//! memory marked obsolete, left out of recall until its text changes.

use aegis_store::testing::test_db;
use aegis_store::{
    Actor, JobSource, MemoryKind, ModeName, NewJob, NewRepo, NewRepoMemory, Page, RepoId, Store,
};

async fn new_repo(s: &Store, slug: &str) -> RepoId {
    s.create_repo(&NewRepo {
        slug: slug.into(),
        url: format!("https://github.com/acme/{slug}"),
        default_branch: "main".into(),
        language: "rust".into(),
        mode: ModeName::POC,
        max_concurrent_jobs: 1,
        config: serde_json::json!({}),
    })
    .await
    .unwrap()
    .id
}

fn lesson(repo_id: RepoId, source_ref: &str, summary: &str) -> NewRepoMemory {
    NewRepoMemory {
        repo_id,
        kind: MemoryKind::MergedPr,
        source_ref: source_ref.into(),
        title: "Fix duplicate charge on payment retry".into(),
        summary: summary.into(),
        paths: vec!["src/pay/charge.rs".into()],
        keywords: "payment retry duplicate charge idempotency".into(),
        job_id: None,
        scope: None,
    }
}

async fn recalled(s: &Store, repo: RepoId) -> Vec<String> {
    s.recall_repo_memories(repo, "duplicate charge on payment retry", "", 10)
        .await
        .unwrap()
        .into_iter()
        .map(|m| m.source_ref)
        .collect()
}

#[tokio::test]
async fn an_obsolete_memory_is_not_recalled_until_its_text_changes() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let repo = new_repo(s, "pay").await;
    let other = new_repo(s, "portal").await;
    let one = s
        .upsert_repo_memory(&lesson(repo, "pr/1", "Added an idempotency key."))
        .await
        .unwrap();
    s.upsert_repo_memory(&lesson(repo, "pr/2", "Retried charges dedupe by key."))
        .await
        .unwrap();
    assert_eq!(recalled(s, repo).await.len(), 2);

    let actor = Actor::cli();
    // Not another repo's, and once only.
    assert!(
        !s.mark_memory_obsolete_by(other, one.id, &actor, None)
            .await
            .unwrap()
    );
    assert!(
        s.mark_memory_obsolete_by(repo, one.id, &actor, None)
            .await
            .unwrap()
    );
    assert!(
        !s.mark_memory_obsolete_by(repo, one.id, &actor, None)
            .await
            .unwrap()
    );
    assert_eq!(recalled(s, repo).await, ["pr/2"]);

    // Written again with the same text (a merged PR extracted again): still
    // obsolete. With new text: recalled again.
    s.upsert_repo_memory(&lesson(repo, "pr/1", "Added an idempotency key."))
        .await
        .unwrap();
    assert_eq!(recalled(s, repo).await, ["pr/2"]);
    s.upsert_repo_memory(&lesson(repo, "pr/1", "Idempotency key per charge attempt."))
        .await
        .unwrap();
    assert_eq!(recalled(s, repo).await.len(), 2);

    let log = s
        .audit_log(Some(repo), Page::first(20))
        .await
        .unwrap()
        .items;
    let marked: Vec<_> = log
        .iter()
        .filter(|e| e.action == "memory.obsolete")
        .collect();
    assert_eq!(marked.len(), 1);
    assert_eq!(marked[0].detail["source_ref"], "pr/1");
}

#[tokio::test]
async fn a_job_keeps_the_memories_it_was_given_in_order() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let repo = new_repo(s, "pay").await;
    let job = s
        .create_job(&NewJob {
            repo_id: repo,
            issue_ref: "acme/pay#1".into(),
            ledger_key: aegis_ledger::IssueId::parse("pay-1").unwrap(),
            source: JobSource::Github,
            priority: None,
            sla_deadline: None,
        })
        .await
        .unwrap();
    let a = s
        .upsert_repo_memory(&lesson(repo, "pr/1", "A."))
        .await
        .unwrap();
    let b = s
        .upsert_repo_memory(&lesson(repo, "pr/2", "B."))
        .await
        .unwrap();
    let c = s
        .upsert_repo_memory(&lesson(repo, "pr/3", "C."))
        .await
        .unwrap();

    s.record_recalled_memories(job.id, &[c.id, a.id])
        .await
        .unwrap();
    let given = s.recalled_memories(job.id).await.unwrap();
    let order: Vec<(i32, &str)> = given
        .iter()
        .map(|r| (r.rank, r.memory.source_ref.as_str()))
        .collect();
    assert_eq!(order, [(1, "pr/3"), (2, "pr/1")]);
    assert!(given.iter().all(|r| r.obsolete.is_none()));

    // A retry keeps the last recall.
    s.record_recalled_memories(job.id, &[b.id]).await.unwrap();
    assert_eq!(s.recalled_memories(job.id).await.unwrap().len(), 1);

    // Marked from the job's page: it says so, and the audit names the job.
    s.mark_memory_obsolete_by(repo, b.id, &Actor::cli(), Some(job.id))
        .await
        .unwrap();
    let given = s.recalled_memories(job.id).await.unwrap();
    assert!(given[0].obsolete.is_some());
    let log = s
        .audit_log(Some(repo), Page::first(20))
        .await
        .unwrap()
        .items;
    let marked = log.iter().find(|e| e.action == "memory.obsolete").unwrap();
    assert_eq!(marked.job_id, Some(job.id));

    // A deleted memory leaves the job's list.
    s.delete_repo_memory_by(repo, b.id, &Actor::cli())
        .await
        .unwrap();
    assert!(s.recalled_memories(job.id).await.unwrap().is_empty());
}
