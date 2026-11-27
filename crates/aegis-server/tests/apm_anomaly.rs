//! Error spikes found before any ticket (#706), against a real database,
//! with Sentry played by a mock server: an error type ten times its
//! baseline right after a release queues exactly one diagnosis job, which
//! reads its spike back as an issue of kind Anomaly; stable counts queue
//! none; a spike seen again is not diagnosed twice.

use aegis_ledger::IssueKind;
use aegis_secrets::Secrets;
use aegis_server::anomaly::{apm_issue, check_due, check_repo_at};
use aegis_store::testing::test_db;
use aegis_store::{Job, JobFilter, JobSource, JobStatus, ModeName, NewRepo, Page, Repo, Store};
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::macros::datetime;
use wiremock::matchers::{bearer_token, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SENTRY_TOKEN: &str = "AEGIS_SENTRY_TOKEN";
const NOW: OffsetDateTime = datetime!(2026-11-20 10:00 UTC);

fn secrets() -> Secrets {
    Secrets::fixed([(SENTRY_TOKEN, "sentry-tok")])
}

async fn repo(s: &Store, slug: &str, sentry: &MockServer, anomaly: Value) -> Repo {
    s.create_repo(&NewRepo {
        slug: slug.into(),
        url: format!("https://github.com/acme/{slug}"),
        default_branch: "main".into(),
        language: "rust".into(),
        mode: ModeName::POC,
        max_concurrent_jobs: 1,
        config: json!({
            "fork": { "owner": "me", "name": slug, "token_env": "GITHUB_TOKEN" },
            "apm": {
                "kind": "sentry", "url": sentry.uri(), "organization": "acme",
                "project": slug, "token_env": SENTRY_TOKEN
            },
            "apm_anomaly": anomaly,
        }),
    })
    .await
    .unwrap()
}

/// Sentry's Discover counts of the window starting at `start`.
async fn counts(server: &MockServer, start: &str, rows: Value) {
    Mock::given(method("GET"))
        .and(path("/api/0/organizations/acme/events/"))
        .and(bearer_token("sentry-tok"))
        .and(query_param("start", start))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": rows })))
        .mount(server)
        .await;
}

async fn release(server: &MockServer, at: &str) {
    Mock::given(method("GET"))
        .and(path("/api/0/organizations/acme/releases/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
            "version": "checkout@1.4.0", "dateReleased": at, "lastCommit": {"id": "cafe1234"}
        }])))
        .mount(server)
        .await;
}

fn row(issue: &str, count: u64) -> Value {
    json!({"issue": issue, "title": format!("{issue}: TypeError"), "count()": count})
}

async fn jobs(s: &Store) -> Vec<Job> {
    let mut jobs = s
        .list_jobs(&JobFilter::default(), Page::first(100))
        .await
        .unwrap()
        .items;
    jobs.sort_by_key(|j| j.id.0);
    jobs
}

#[tokio::test]
async fn a_tenfold_spike_after_a_release_is_one_diagnosis_job() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let sentry = MockServer::start().await;
    // The last hour against the same hour a week before.
    counts(
        &sentry,
        "2026-11-20T09:00:00Z",
        json!([row("CHECKOUT-1A", 240), row("CHECKOUT-7", 33)]),
    )
    .await;
    counts(
        &sentry,
        "2026-11-13T09:00:00Z",
        json!([row("CHECKOUT-1A", 24), row("CHECKOUT-7", 30)]),
    )
    .await;
    release(&sentry, "2026-11-20T09:10:00Z").await;
    let app = repo(s, "checkout", &sentry, json!({})).await;

    let refused = check_repo_at(s, &secrets(), &app, NOW).await.unwrap();
    assert!(refused.is_empty());
    let queued = jobs(s).await;
    assert_eq!(queued.len(), 1, "{queued:?}");
    let job = &queued[0];
    assert_eq!(
        (job.source, job.status),
        (JobSource::Apm, JobStatus::Queued)
    );
    assert!(job.issue_ref.starts_with("apm-"), "{}", job.issue_ref);
    assert!(job.pr_url.is_none());

    let issue = apm_issue(job, &app, s).await.unwrap();
    assert_eq!(issue.kind, Some(IssueKind::Anomaly));
    assert_eq!(issue.source.as_deref(), Some("apm"));
    assert_eq!(issue.id, job.ledger_key.as_str());
    assert_eq!(issue.title, "Error spike: CHECKOUT-1A: TypeError");
    assert!(
        issue
            .body
            .contains("240 errors in the last hour, 24 in the baseline window (10.0×)"),
        "{}",
        issue.body
    );
    assert!(issue.body.contains("`checkout@1.4.0`"), "{}", issue.body);
    assert!(
        issue.body.contains("last commit `cafe1234`"),
        "{}",
        issue.body
    );

    // The next check finds the same spike: it is not diagnosed again.
    check_repo_at(s, &secrets(), &app, NOW).await.unwrap();
    assert_eq!(jobs(s).await.len(), 1);
    db.drop().await;
}

#[tokio::test]
async fn stable_counts_queue_no_job() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let sentry = MockServer::start().await;
    counts(
        &sentry,
        "2026-11-20T09:00:00Z",
        json!([row("CHECKOUT-1A", 26), row("CHECKOUT-7", 33)]),
    )
    .await;
    counts(
        &sentry,
        "2026-11-13T09:00:00Z",
        json!([row("CHECKOUT-1A", 24), row("CHECKOUT-7", 30)]),
    )
    .await;
    release(&sentry, "2026-11-19T09:00:00Z").await;
    let app = repo(s, "checkout", &sentry, json!({})).await;
    check_repo_at(s, &secrets(), &app, NOW).await.unwrap();
    assert!(jobs(s).await.is_empty());
    db.drop().await;
}

#[tokio::test]
async fn the_cap_and_the_settings_hold() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let sentry = MockServer::start().await;
    // Three new error types, a cap of two a day.
    counts(
        &sentry,
        "2026-11-20T09:00:00Z",
        json!([row("A", 90), row("B", 80), row("C", 70)]),
    )
    .await;
    counts(&sentry, "2026-11-13T09:00:00Z", json!([])).await;
    release(&sentry, "2026-11-01T00:00:00Z").await;
    let app = repo(s, "capped", &sentry, json!({"max_jobs_per_day": 2})).await;
    check_repo_at(s, &secrets(), &app, NOW).await.unwrap();
    let titles: Vec<String> = {
        let mut out = Vec::new();
        for job in jobs(s).await {
            out.push(apm_issue(&job, &app, s).await.unwrap().title);
        }
        out
    };
    assert_eq!(
        titles,
        ["New error: A: TypeError", "New error: B: TypeError"]
    );

    // Bad settings, or no secret: the repo is skipped, nothing is queued.
    let bad = repo(s, "bad", &sentry, json!({"every_s": 1})).await;
    check_repo_at(s, &secrets(), &bad, NOW).await.unwrap();
    let other = repo(s, "nosecret", &sentry, json!({})).await;
    check_repo_at(s, &Secrets::fixed([("OTHER", "x")]), &other, NOW)
        .await
        .unwrap();
    assert_eq!(jobs(s).await.len(), 2);

    // The checker claims the due repos at the time it runs, which Sentry
    // has no counts for here: nothing more is queued.
    assert!(check_due(s, &secrets()).await.unwrap().is_empty());
    assert_eq!(jobs(s).await.len(), 2);
    db.drop().await;
}
