//! Repos found by remote and deliveries written with their job (#25).

use aegis_ledger::IssueId;
use aegis_store::testing::test_db;
use aegis_store::{
    Delivered, JobSource, NewDelivery, NewJob, NewRepo, RepoId, RepoMode, Store, StoreError,
    WebhookSource,
};
use serde_json::json;

async fn repo(s: &Store, slug: &str, url: &str) -> RepoId {
    s.create_repo(&NewRepo {
        slug: slug.into(),
        url: url.into(),
        default_branch: "main".into(),
        language: "rust".into(),
        mode: RepoMode::Poc,
        max_concurrent_jobs: 1,
        config: json!({}),
    })
    .await
    .unwrap()
    .id
}

#[tokio::test]
async fn repos_are_found_by_url_or_path() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let typst = repo(s, "typst", "https://github.com/Typst/typst.git").await;
    repo(s, "app", "https://gitlab.com/group/sub/app/").await;
    let found = |remote: &'static str| async move {
        s.repo_by_remote(remote).await.unwrap().map(|r| r.slug)
    };
    assert_eq!(
        found("https://github.com/typst/typst").await.as_deref(),
        Some("typst")
    );
    assert_eq!(
        found("https://github.com/typst/typst/").await.as_deref(),
        Some("typst")
    );
    assert_eq!(found("typst/typst").await.as_deref(), Some("typst"));
    assert_eq!(found("group/sub/app").await.as_deref(), Some("app"));
    assert_eq!(found("sub/app").await.as_deref(), Some("app"));
    for missing in [
        "https://github.com/typst/typst2",
        "ypst/typst",
        "",
        "typst",
        "/",
    ] {
        assert_eq!(found(missing).await, None, "{missing}");
    }
    // `_` and `%` are not patterns.
    assert_eq!(found("typst/typs_").await, None);

    repo(s, "typst-mirror", "https://git.example/typst/typst").await;
    assert!(matches!(
        s.repo_by_remote("typst/typst").await,
        Err(StoreError::Conflict(_))
    ));
    assert_eq!(
        s.repo_by_remote("https://github.com/typst/typst")
            .await
            .unwrap()
            .map(|r| r.id),
        Some(typst)
    );
    db.drop().await;
}

#[tokio::test]
async fn a_delivery_makes_one_job_however_often_it_comes() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let repo_id = repo(s, "typst", "https://github.com/typst/typst").await;
    let delivery = |id: &str| NewDelivery {
        source: WebhookSource::Github,
        delivery_id: id.into(),
        event: "issues".into(),
    };
    let job = |key: &str| NewJob {
        repo_id,
        issue_ref: "typst/typst#1".into(),
        ledger_key: IssueId::parse(key).unwrap(),
        source: JobSource::Github,
    };

    let Delivered::Queued(first) = s.deliver_job(&delivery("a"), &job("k-1")).await.unwrap() else {
        panic!("not queued");
    };
    assert_eq!(
        s.deliver_job(&delivery("a"), &job("k-2")).await.unwrap(),
        Delivered::Duplicate
    );
    // Another delivery ID is GitLab's to reuse.
    let gitlab = NewDelivery {
        source: WebhookSource::Gitlab,
        ..delivery("a")
    };
    assert_eq!(
        s.deliver_job(&gitlab, &job("k-3")).await.unwrap(),
        Delivered::AlreadyLive(first.id)
    );
    assert!(matches!(
        s.deliver_job(&delivery(""), &job("k-4")).await,
        Err(StoreError::Invalid { .. })
    ));
    // A job that cannot be written leaves no delivery behind.
    let nowhere = NewJob {
        repo_id: RepoId(9999),
        issue_ref: "x/y#1".into(),
        ..job("k-5")
    };
    assert!(matches!(
        s.deliver_job(&delivery("b"), &nowhere).await,
        Err(StoreError::NotFound(_))
    ));
    assert!(matches!(
        s.deliver_job(&delivery("b"), &job("k-6")).await.unwrap(),
        Delivered::AlreadyLive(_)
    ));
    db.drop().await;
}

#[tokio::test]
async fn a_polled_ticket_is_taken_once() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let app = repo(s, "app", "https://github.com/me/app").await;
    for (source, job_source) in [
        (WebhookSource::Jira, JobSource::Jira),
        (WebhookSource::Backlog, JobSource::Backlog),
    ] {
        let delivery = NewDelivery {
            source,
            delivery_id: format!("{}:APP-7", app.0),
            event: "poll".into(),
        };
        let job = NewJob {
            repo_id: app,
            issue_ref: "APP-7".into(),
            ledger_key: IssueId::parse(format!("APP-7-{}", source.as_str())).unwrap(),
            source: job_source,
        };
        let first = s.deliver_job(&delivery, &job).await.unwrap();
        let Delivered::Queued(queued) = first else {
            panic!("{first:?}")
        };
        assert_eq!(queued.source, job_source);
        assert_eq!(
            s.deliver_job(&delivery, &job).await.unwrap(),
            Delivered::Duplicate
        );
        // The job ends: the ticket still matches, but is not taken again.
        end_job(s, queued.id).await;
        assert_eq!(
            s.deliver_job(&delivery, &job).await.unwrap(),
            Delivered::Duplicate
        );
    }
}

async fn end_job(s: &Store, job: aegis_store::JobId) {
    s.request_cancel(job).await.unwrap();
}
