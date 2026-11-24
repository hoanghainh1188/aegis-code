//! #658: a job's page shows the repo memories it was given, and its
//! repo's maintainers mark one obsolete there (a memory shared with the
//! tenant takes the tenant's admin), audited; recall then leaves it out.

mod common;

use aegis_ledger::IssueId;
use aegis_server::api::router;
use aegis_store::testing::test_db;
use aegis_store::{
    Job, JobSource, MemoryKind, MemoryScope, ModeName, NewJob, NewRepo, NewRepoMemory, Page,
    RepoId, RepoMemory, RepoRole, Store,
};
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::Browser;
use http_body_util::BodyExt;
use tower::ServiceExt;

struct Answer {
    status: StatusCode,
    location: String,
    body: String,
}

async fn send(app: &Router, request: Request<Body>) -> Answer {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let location = response
        .headers()
        .get(header::LOCATION)
        .map(|l| l.to_str().unwrap().to_owned())
        .unwrap_or_default();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    Answer {
        status,
        location,
        body: String::from_utf8(bytes.to_vec()).unwrap(),
    }
}

async fn page(app: &Router, who: &Browser, uri: &str) -> Answer {
    let request = Request::get(uri)
        .header(header::HOST, "localhost:8080")
        .header(header::COOKIE, &who.cookie)
        .body(Body::empty())
        .unwrap();
    send(app, request).await
}

async fn post(app: &Router, who: &Browser, uri: &str) -> Answer {
    let request = Request::post(uri)
        .header(header::HOST, "localhost:8080")
        .header(header::ORIGIN, "http://localhost:8080")
        .header(header::COOKIE, &who.cookie)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(format!("_csrf={}", who.csrf)))
        .unwrap();
    send(app, request).await
}

async fn setup(s: &Store) -> (RepoId, Job, RepoMemory, RepoMemory, RepoMemory) {
    let repo = s
        .create_repo(&NewRepo {
            slug: "pay".into(),
            url: "https://github.com/acme/pay".into(),
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
            issue_ref: "acme/pay#1".into(),
            ledger_key: IssueId::parse("pay-1").unwrap(),
            source: JobSource::Github,
            priority: None,
            sla_deadline: None,
        })
        .await
        .unwrap();
    let memory = |source_ref: &str, title: &str, scope| NewRepoMemory {
        repo_id: repo,
        kind: MemoryKind::MergedPr,
        source_ref: source_ref.into(),
        title: title.into(),
        summary: "Duplicate charges on retry.".into(),
        paths: vec!["src/pay/charge.rs".into()],
        keywords: "payment retry duplicate charge".into(),
        job_id: None,
        scope: Some(scope),
    };
    let given = s
        .upsert_repo_memory(&memory(
            "pr/1",
            "Idempotency key <on> charge",
            MemoryScope::Repo,
        ))
        .await
        .unwrap();
    let shared = s
        .upsert_repo_memory(&memory(
            "pr/2",
            "Tenant-wide charge rule",
            MemoryScope::Tenant,
        ))
        .await
        .unwrap();
    let not_given = s
        .upsert_repo_memory(&memory("pr/3", "Not given to the job", MemoryScope::Repo))
        .await
        .unwrap();
    s.record_recalled_memories(job.id, &[given.id, shared.id])
        .await
        .unwrap();
    (repo, job, given, shared, not_given)
}

#[tokio::test]
async fn a_maintainer_marks_a_memory_the_job_was_given_obsolete() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let (repo, job, given, shared, not_given) = setup(s).await;
    let app = router(s.clone(), common::auth(s));
    let maintainer = common::sign_in(s, "mai@example.com").await;
    common::grant(s, "mai@example.com", repo, RepoRole::Maintainer).await;
    let viewer = common::sign_in(s, "vic@example.com").await;
    common::grant(s, "vic@example.com", repo, RepoRole::Viewer).await;
    let job_page = format!("/jobs/{}", job.id.0);
    let mark = |m: &RepoMemory| format!("/jobs/{}/memories/{}/obsolete", job.id.0, m.id.0);

    // Both see what the job was given, its untrusted text escaped; only
    // the maintainer may mark the repo's own memory.
    let seen = page(&app, &viewer, &job_page).await;
    assert_eq!(seen.status, StatusCode::OK);
    assert!(
        seen.body.contains("Idempotency key &#60;on&#62; charge"),
        "{}",
        seen.body
    );
    assert!(seen.body.contains("Tenant-wide charge rule"));
    assert!(!seen.body.contains("Not given to the job"));
    assert!(!seen.body.contains("Mark obsolete"));
    let seen = page(&app, &maintainer, &job_page).await;
    assert!(seen.body.contains(&mark(&given)));
    assert!(
        !seen.body.contains(&mark(&shared)),
        "a shared memory takes the tenant's admin"
    );

    assert_eq!(
        post(&app, &viewer, &mark(&given)).await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        post(&app, &maintainer, &mark(&shared)).await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        post(&app, &maintainer, &mark(&not_given)).await.status,
        StatusCode::NOT_FOUND
    );

    let done = post(&app, &maintainer, &mark(&given)).await;
    assert_eq!(done.status, StatusCode::SEE_OTHER);
    assert_eq!(done.location, format!("{job_page}?done=memory_obsolete"));
    let seen = page(&app, &maintainer, &done.location).await;
    assert!(seen.body.contains("Memory marked obsolete"));
    assert!(
        seen.body.contains("mai@example.com"),
        "who marked it is shown"
    );
    assert!(!seen.body.contains(&mark(&given)));

    let recalled = s
        .recall_repo_memories(repo, "duplicate charge on payment retry", "", 10)
        .await
        .unwrap();
    assert!(recalled.iter().all(|m| m.id != given.id));
    let log = s
        .audit_log(Some(repo), Page::first(20))
        .await
        .unwrap()
        .items;
    let entry = log.iter().find(|e| e.action == "memory.obsolete").unwrap();
    assert_eq!(entry.actor.name, "mai@example.com");
    assert_eq!(entry.job_id, Some(job.id));

    // The platform's admin marks the shared one.
    let admin = common::sign_in(s, "root@example.com").await;
    common::admin(s, "root@example.com").await;
    assert_eq!(
        post(&app, &admin, &mark(&shared)).await.status,
        StatusCode::SEE_OTHER
    );
}
