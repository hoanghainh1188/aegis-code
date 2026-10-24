//! Repos, jobs and runs (#22): constraints and paging.

use aegis_ledger::Outcome;
use aegis_store::testing::test_db;
use aegis_store::{
    JobEnd, JobFilter, JobSource, JobStatus, NewJob, NewRepo, Page, RepoMode, RunEnd, RunStatus,
    StoreError,
};

fn repo(slug: &str) -> NewRepo {
    NewRepo {
        slug: slug.into(),
        url: format!("https://github.com/example/{slug}"),
        default_branch: "main".into(),
        language: "rust".into(),
        mode: RepoMode::Poc,
        config: serde_json::json!({ "image": "aegis/rust:1" }),
    }
}

fn job(repo_id: aegis_store::RepoId, n: u32) -> NewJob {
    NewJob {
        repo_id,
        issue_ref: format!("example/typst#{n}"),
        ledger_key: aegis_ledger::IssueId::parse(format!("typst-{n}")).unwrap(),
        source: JobSource::Github,
    }
}

#[tokio::test]
async fn repos_are_created_found_and_unique_by_slug() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let typst = s.create_repo(&repo("typst")).await.unwrap();
    assert_eq!(typst.slug, "typst");
    assert_eq!(typst.mode, RepoMode::Poc);
    assert_eq!(typst.config["image"], "aegis/rust:1");
    assert_eq!(s.repo(typst.id).await.unwrap(), Some(typst.clone()));
    assert_eq!(s.repo_by_slug("typst").await.unwrap(), Some(typst));

    let err = s.create_repo(&repo("typst")).await.unwrap_err();
    assert!(matches!(err, StoreError::Conflict(_)), "{err}");
    let err = s.create_repo(&repo("../etc")).await.unwrap_err();
    assert!(matches!(err, StoreError::Invalid { .. }), "{err}");
    db.drop().await;
}

#[tokio::test]
async fn lists_are_paged_and_capped() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    for n in 0..5 {
        s.create_repo(&repo(&format!("r{n}"))).await.unwrap();
    }
    let first = s.list_repos(Page::first(2)).await.unwrap();
    let slugs = |p: &aegis_store::PageOf<aegis_store::Repo>| {
        p.items.iter().map(|r| r.slug.clone()).collect::<Vec<_>>()
    };
    assert_eq!(slugs(&first), ["r0", "r1"]);
    let second = s.list_repos(first.next_page().unwrap()).await.unwrap();
    assert_eq!(slugs(&second), ["r2", "r3"]);
    let third = s.list_repos(second.next_page().unwrap()).await.unwrap();
    assert_eq!(slugs(&third), ["r4"]);
    assert!(third.next_page().is_none());

    assert_eq!(Page::first(0).limit(), 1);
    assert_eq!(Page::first(10_000).limit(), Page::MAX_LIMIT);
    db.drop().await;
}

#[tokio::test]
async fn a_job_runs_attempts_and_ends_with_its_outcome() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let r = s.create_repo(&repo("typst")).await.unwrap();
    let j = s.create_job(&job(r.id, 1)).await.unwrap();
    assert_eq!(
        (j.status, j.outcome, j.attempts),
        (JobStatus::Queued, None, 0)
    );

    let first = s.start_run(j.id).await.unwrap();
    assert_eq!((first.attempt, first.status), (1, RunStatus::Running));
    let err = s.start_run(j.id).await.unwrap_err();
    assert!(
        matches!(err, StoreError::Conflict(_)),
        "one live run: {err}"
    );
    let first = s.end_run(first.id, RunEnd::Interrupted).await.unwrap();
    assert_eq!(first.status, RunStatus::Interrupted);
    assert!(first.ended_at.is_some());
    let err = s.end_run(first.id, RunEnd::Finished).await.unwrap_err();
    assert!(matches!(err, StoreError::Conflict(_)), "ended once: {err}");

    let second = s.start_run(j.id).await.unwrap();
    assert_eq!(second.attempt, 2);
    let running = s.job(j.id).await.unwrap().unwrap();
    assert_eq!((running.status, running.attempts), (JobStatus::Running, 2));
    s.end_run(second.id, RunEnd::Finished).await.unwrap();
    let done = s
        .end_job(j.id, JobEnd::Done(Outcome::StoppedBudget))
        .await
        .unwrap();
    assert_eq!(
        (done.status, done.outcome),
        (JobStatus::Done, Some(Outcome::StoppedBudget))
    );
    let err = s.end_job(j.id, JobEnd::Cancelled).await.unwrap_err();
    assert!(matches!(err, StoreError::Conflict(_)), "ended once: {err}");
    let err = s.start_run(j.id).await.unwrap_err();
    assert!(matches!(err, StoreError::Conflict(_)), "done job: {err}");

    let runs = s.list_runs(j.id, Page::first(10)).await.unwrap();
    let attempts: Vec<_> = runs.items.iter().map(|r| (r.attempt, r.status)).collect();
    assert_eq!(
        attempts,
        [(1, RunStatus::Interrupted), (2, RunStatus::Finished)]
    );
    db.drop().await;
}

#[tokio::test]
async fn an_issue_has_one_live_job_and_each_ledger_one_job() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let r = s.create_repo(&repo("typst")).await.unwrap();
    let first = s.create_job(&job(r.id, 1)).await.unwrap();
    let mut same_issue = job(r.id, 1);
    same_issue.ledger_key = aegis_ledger::IssueId::parse("typst-1-b").unwrap();
    let err = s.create_job(&same_issue).await.unwrap_err();
    assert!(matches!(err, StoreError::Conflict(_)), "{err}");

    s.end_job(first.id, JobEnd::Cancelled).await.unwrap();
    s.create_job(&same_issue).await.unwrap();

    let err = s.create_job(&job(r.id, 1)).await.unwrap_err();
    assert!(
        matches!(err, StoreError::Conflict(_)),
        "ledger reused: {err}"
    );
    let err = s
        .create_job(&job(aegis_store::RepoId(9_999), 2))
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::NotFound(_)), "{err}");
    db.drop().await;
}

#[tokio::test]
async fn jobs_list_newest_first_by_repo_and_status() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let a = s.create_repo(&repo("a")).await.unwrap();
    let b = s.create_repo(&repo("b")).await.unwrap();
    let mut ids = Vec::new();
    for n in 0..4 {
        ids.push(s.create_job(&job(a.id, n)).await.unwrap().id);
    }
    s.create_job(&job(b.id, 10)).await.unwrap();
    s.end_job(ids[1], JobEnd::Failed).await.unwrap();

    let of_a = JobFilter {
        repo_id: Some(a.id),
        status: None,
    };
    let page = s.list_jobs(&of_a, Page::first(3)).await.unwrap();
    let got: Vec<_> = page.items.iter().map(|j| j.id).collect();
    assert_eq!(got, [ids[3], ids[2], ids[1]]);
    let rest = s.list_jobs(&of_a, page.next_page().unwrap()).await.unwrap();
    assert_eq!(
        rest.items.iter().map(|j| j.id).collect::<Vec<_>>(),
        [ids[0]]
    );

    let failed = JobFilter {
        repo_id: None,
        status: Some(JobStatus::Failed),
    };
    let page = s.list_jobs(&failed, Page::first(10)).await.unwrap();
    assert_eq!(
        page.items.iter().map(|j| j.id).collect::<Vec<_>>(),
        [ids[1]]
    );
    assert_eq!(
        s.list_jobs(&JobFilter::default(), Page::first(10))
            .await
            .unwrap()
            .items
            .len(),
        5
    );
    db.drop().await;
}
