//! #312: the patch a job's PR holds, file by file, on the job's page and
//! on the PR's review page, where the reviewer reads the PR as it is now:
//! the latest review round's patch when the PR had rounds.

mod common;

use aegis_ledger::{IssueId, LedgerEvent, LedgerStore, Outcome};
use aegis_server::api::router;
use aegis_store::testing::test_db;
use aegis_store::{
    Actor, AttemptEnd, JobId, JobSource, NewJob, NewRepo, Patch, RepoId, RepoMode, RepoRole, Store,
};
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::Browser;
use http_body_util::BodyExt;
use serde_json::json;
use std::time::Duration;
use tower::ServiceExt;

async fn get(app: &Router, who: &Browser, uri: &str) -> (StatusCode, String) {
    let request = Request::get(uri)
        .header(header::HOST, "localhost:8080")
        .header(header::COOKIE, &who.cookie)
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

async fn repo(s: &Store, slug: &str) -> RepoId {
    s.create_repo(&NewRepo {
        slug: slug.into(),
        url: format!("https://github.com/x/{slug}"),
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

const PR: &str = "https://github.com/bot/demo/pull/5";

/// Claims the next job and ends it with a PR holding `diff`.
async fn opened(s: &Store, diff: &str, truncated: bool) -> JobId {
    let claim = s
        .claim_job("w", Duration::from_secs(60), 10)
        .await
        .unwrap()
        .unwrap();
    let key = claim.job.ledger_key.clone();
    s.ledger()
        .append(
            &key,
            &LedgerEvent::Finished {
                outcome: Outcome::PrOpened,
                wall_time_s: 60.0,
            },
        )
        .await
        .unwrap();
    let end = AttemptEnd::Done {
        outcome: Outcome::PrOpened,
        reason: None,
        pr_url: Some(PR.into()),
        patch: Some(Patch {
            diff: diff.into(),
            truncated,
        }),
    };
    s.finish_attempt(&claim.run, "w", &end).await.unwrap();
    claim.job.id
}

const FIRST: &str = "\
diff --git a/src/lib.rs b/src/lib.rs
index 1111111..2222222 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -21,7 +21,7 @@ impl Cart {
     pub fn add(&mut self, price: u32) -> Result<(), CartError> {
-        if self.items.len() > MAX_ITEMS {
+        if self.items.len() >= MAX_ITEMS {
             return Err(CartError::Full);
diff --git a/tests/fourth_item.rs b/tests/fourth_item.rs
new file mode 100644
--- /dev/null
+++ b/tests/fourth_item.rs
@@ -0,0 +1,2 @@
+// <script>alert(1)</script>
+#[test] fn fourth_item_is_refused() {}
";

#[tokio::test]
async fn the_job_and_its_review_show_the_patch_file_by_file() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let (a, b) = (repo(s, "repo-a").await, repo(s, "repo-b").await);
    common::grant(s, "rev@x.test", a, RepoRole::Reviewer).await;
    let rev = common::sign_in(s, "rev@x.test").await;
    let app = router(s.clone(), common::auth(s));
    s.create_job(&NewJob {
        repo_id: a,
        issue_ref: "x/y#1".into(),
        ledger_key: IssueId::parse("a-1").unwrap(),
        source: JobSource::Github,
    })
    .await
    .unwrap();
    let root = opened(s, FIRST, false).await;

    for uri in [
        format!("/jobs/{}", root.0),
        format!("/jobs/{}/review", root.0),
    ] {
        let (status, page) = get(&app, &rev, &uri).await;
        assert_eq!(status, StatusCode::OK, "{uri}");
        // One section per file, named, with its added and removed lines.
        assert!(
            page.contains("src/lib.rs") && page.contains("tests/fourth_item.rs"),
            "{uri}"
        );
        assert!(page.contains("class=\"add\""), "{uri}: {page}");
        assert!(page.contains("class=\"del\""), "{uri}");
        assert!(page.contains("class=\"hunk\""), "{uri}");
        // Escaped as any text.
        assert!(page.contains("&#60;script&#62;alert(1)"), "{uri}");
        assert!(!page.contains("<script>alert(1)"), "{uri}");
    }

    // A review round's newer patch is what the review page shows.
    let round = s.review_job_by(root, &Actor::cli()).await.unwrap();
    let second = FIRST.replace("fourth_item_is_refused", "fourth_and_fifth_are_refused");
    assert_eq!(opened(s, &second, true).await, round.id);
    let (_, review) = get(&app, &rev, &format!("/jobs/{}/review", root.0)).await;
    assert!(review.contains("fourth_and_fifth_are_refused"), "{review}");
    assert!(!review.contains("fourth_item_is_refused"), "{review}");
    // Cut patches say so and point to the PR.
    assert!(review.contains("truncated"), "{review}");

    // Another repo's job is not there.
    s.create_job(&NewJob {
        repo_id: b,
        issue_ref: "x/z#2".into(),
        ledger_key: IssueId::parse("b-1").unwrap(),
        source: JobSource::Github,
    })
    .await
    .unwrap();
    let other = opened(s, FIRST, false).await;
    let (status, _) = get(&app, &rev, &format!("/jobs/{}", other.0)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    db.drop().await;
}
