//! #677: a feature's plans, one per revision, the last the one approved.

use aegis_ledger::IssueId;
use aegis_store::testing::test_db;
use aegis_store::{JobSource, MAX_PLAN_BYTES, ModeName, NewJob, NewRepo, StoreError};

#[tokio::test]
async fn each_revision_of_a_plan_is_kept_in_order() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let repo = s
        .create_repo(&NewRepo {
            slug: "shop".into(),
            url: "https://github.com/example/shop".into(),
            default_branch: "main".into(),
            language: "rust".into(),
            mode: ModeName::POC,
            max_concurrent_jobs: 1,
            config: serde_json::json!({}),
        })
        .await
        .unwrap()
        .id;
    let job = s
        .create_job(&NewJob {
            repo_id: repo,
            issue_ref: "example/shop#1".into(),
            ledger_key: IssueId::parse("shop-1").unwrap(),
            source: JobSource::Github,
            priority: None,
            sla_deadline: None,
        })
        .await
        .unwrap();
    assert_eq!(s.feature_plan(job.id).await.unwrap(), None);

    let first = serde_json::json!({ "summary": "one", "sub_jobs": [{ "id": "a" }] });
    let second =
        serde_json::json!({ "summary": "two", "sub_jobs": [{ "id": "a" }, { "id": "b" }] });
    assert_eq!(
        s.save_feature_plan(job.id, &first).await.unwrap().revision,
        1
    );
    assert_eq!(
        s.save_feature_plan(job.id, &second).await.unwrap().revision,
        2
    );
    let last = s.feature_plan(job.id).await.unwrap().unwrap();
    assert_eq!((last.revision, &last.plan), (2, &second));
    let all: Vec<i32> = s
        .feature_plans(job.id)
        .await
        .unwrap()
        .iter()
        .map(|p| p.revision)
        .collect();
    assert_eq!(all, [1, 2]);

    let huge = serde_json::json!({ "summary": "x".repeat(MAX_PLAN_BYTES) });
    let err = s.save_feature_plan(job.id, &huge).await.unwrap_err();
    assert!(
        matches!(err, StoreError::Invalid { field: "plan", .. }),
        "{err}"
    );
    db.drop().await;
}
