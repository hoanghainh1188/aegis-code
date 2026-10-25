//! New Sonar warnings, polled (#27), against a real database, with
//! SonarQube played by a mock server: a poll turns the new warnings of
//! the main branch into one batch job, a warning is never taken twice,
//! each repo keeps its own schedule, and the job reads its warnings back
//! as an issue of kind Sonar for the batching step.

use aegis_ledger::IssueKind;
use aegis_secrets::Secrets;
use aegis_server::runner::Attempt;
use aegis_server::sonar::{poll_due, poll_repo, sonar_issue};
use aegis_sonar_batch::parse_search;
use aegis_store::testing::test_db;
use aegis_store::{Job, JobFilter, JobSource, JobStatus, NewRepo, Page, Repo, RepoMode, Store};
use serde_json::{Value, json};
use wiremock::matchers::{bearer_token, method, path, query_param, query_param_is_missing};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SONAR_TOKEN: &str = "AEGIS_SONAR_TOKEN";

fn secrets() -> Secrets {
    Secrets::fixed([(SONAR_TOKEN, "sonar-tok")])
}

async fn repo(s: &Store, slug: &str, sonar: &MockServer, poll: Option<Value>) -> Repo {
    let mut config = json!({
        "fork": { "owner": "me", "name": slug, "token_env": "GITHUB_TOKEN" },
        "sonar": {
            "url": sonar.uri(), "token_env": SONAR_TOKEN, "organization": "acme",
            "project_key": format!("acme_{slug}")
        },
    });
    if let Some(poll) = poll {
        config["sonar_poll"] = poll;
    }
    s.create_repo(&NewRepo {
        slug: slug.into(),
        url: format!("https://github.com/acme/{slug}"),
        default_branch: "main".into(),
        language: "rust".into(),
        mode: RepoMode::Poc,
        max_concurrent_jobs: 1,
        config,
    })
    .await
    .unwrap()
}

fn warning(project: &str, key: &str, severity: &str) -> Value {
    json!({
        "key": key, "rule": "rust:S1481", "severity": severity,
        "component": format!("{project}:src/lib.rs"), "line": 3,
        "message": format!("warning {key}"),
        "textRange": {"startLine": 3, "endLine": 3}, "flows": []
    })
}

fn answer(issues: Vec<Value>) -> ResponseTemplate {
    let total = issues.len();
    ResponseTemplate::new(200).set_body_json(json!({
        "paging": {"pageIndex": 1, "pageSize": 100, "total": total}, "issues": issues
    }))
}

/// The poll's search of `project`: main branch, open, newest first.
async fn new_warnings(server: &MockServer, project: &str, issues: Vec<Value>) {
    Mock::given(method("GET"))
        .and(path("/api/issues/search"))
        .and(bearer_token("sonar-tok"))
        .and(query_param("organization", "acme"))
        .and(query_param("componentKeys", project))
        .and(query_param("resolved", "false"))
        .and(query_param("severities", "MAJOR,CRITICAL,BLOCKER"))
        .and(query_param_is_missing("issues"))
        .and(query_param_is_missing("branch"))
        .respond_with(answer(issues))
        .mount(server)
        .await;
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
async fn new_warnings_become_one_batch_each_taken_once() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let sonar = MockServer::start().await;
    let app = repo(s, "app", &sonar, Some(json!({ "every_s": 3600 }))).await;
    // Not polled: no `sonar_poll`.
    repo(s, "quiet", &sonar, None).await;
    let secrets = secrets();

    new_warnings(
        &sonar,
        "acme_app",
        vec![
            warning("acme_app", "W2", "CRITICAL"),
            warning("acme_app", "W1", "MAJOR"),
        ],
    )
    .await;
    assert!(poll_due(s, &secrets).await.unwrap().is_empty());
    let queued = jobs(s).await;
    assert_eq!(queued.len(), 1, "{queued:?}");
    let batch = &queued[0];
    assert_eq!(batch.repo_id, app.id);
    assert_eq!(
        (batch.source, batch.status),
        (JobSource::Sonar, JobStatus::Queued)
    );
    assert!(batch.issue_ref.starts_with("sonar-"), "{}", batch.issue_ref);
    assert!(
        batch
            .ledger_key
            .as_str()
            .to_ascii_lowercase()
            .starts_with(&format!("app-{}-sonar-", app.id.0)),
        "{}",
        batch.ledger_key
    );
    assert_eq!(
        s.sonar_batch_warnings(app.id, &batch.issue_ref)
            .await
            .unwrap(),
        ["W1", "W2"]
    );

    // The repo's schedule: not due again within the hour.
    assert!(poll_due(s, &secrets).await.unwrap().is_empty());
    assert_eq!(jobs(s).await.len(), 1);

    // A poll that finds the same warnings, still open: nothing new.
    assert!(poll_repo(s, &secrets, &app).await.unwrap().is_none());
    assert_eq!(jobs(s).await.len(), 1);

    // One new warning among them: a batch of that one only.
    sonar.reset().await;
    new_warnings(
        &sonar,
        "acme_app",
        vec![
            warning("acme_app", "W3", "BLOCKER"),
            warning("acme_app", "W2", "CRITICAL"),
            warning("acme_app", "W1", "MAJOR"),
        ],
    )
    .await;
    // Batches are named by the millisecond.
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    poll_repo(s, &secrets, &app).await.unwrap();
    let all = jobs(s).await;
    assert_eq!(all.len(), 2, "{all:?}");
    let second = &all[1];
    assert_ne!(second.issue_ref, batch.issue_ref);
    assert_eq!(
        s.sonar_batch_warnings(app.id, &second.issue_ref)
            .await
            .unwrap(),
        ["W3"]
    );
    db.drop().await;
}

#[tokio::test]
async fn the_batch_job_reads_its_open_warnings_as_a_sonar_issue() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let sonar = MockServer::start().await;
    let app = repo(s, "app", &sonar, Some(json!({ "every_s": 600 }))).await;
    let secrets = secrets();
    new_warnings(
        &sonar,
        "acme_app",
        vec![
            warning("acme_app", "W1", "MAJOR"),
            warning("acme_app", "W2", "MAJOR"),
        ],
    )
    .await;
    poll_repo(s, &secrets, &app).await.unwrap();
    let job = jobs(s).await.remove(0);

    // W2 was fixed by hand in the meantime: only W1 is still open.
    Mock::given(method("GET"))
        .and(path("/api/issues/search"))
        .and(bearer_token("sonar-tok"))
        .and(query_param("organization", "acme"))
        .and(query_param("componentKeys", "acme_app"))
        .and(query_param("issues", "W1,W2"))
        .and(query_param("resolved", "false"))
        .respond_with(answer(vec![warning("acme_app", "W1", "MAJOR")]))
        .expect(1)
        .mount(&sonar)
        .await;
    let issue = sonar_issue(&job, &app, s, &secrets).await.unwrap();
    assert_eq!(issue.id, job.ledger_key.as_str());
    assert_eq!(issue.kind, Some(IssueKind::Sonar));
    assert_eq!(issue.title, "Sonar: 1 new warnings in acme_app");
    assert!(
        issue
            .url
            .ends_with("/project/issues?id=acme_app&resolved=false"),
        "{}",
        issue.url
    );
    let start = issue.body.find("```json").unwrap() + "```json".len();
    let rest = &issue.body[start..];
    let warnings = parse_search(rest[..rest.find("```").unwrap()].trim()).unwrap();
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].key, "W1");

    // Without the token, the job cannot run: nothing to retry.
    let err = sonar_issue(&job, &app, s, &Secrets::fixed([("OTHER", "x")]))
        .await
        .unwrap_err();
    assert!(
        matches!(&err, Attempt::Rejected(m) if m.contains(SONAR_TOKEN)),
        "{err:?}"
    );
    db.drop().await;
}

#[tokio::test]
async fn a_failing_sonar_skips_the_repo() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let sonar = MockServer::start().await;
    let app = repo(s, "app", &sonar, Some(json!({ "every_s": 600 }))).await;
    Mock::given(path("/api/issues/search"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&sonar)
        .await;
    assert!(poll_repo(s, &secrets(), &app).await.unwrap().is_none());
    // No token in the store: skipped too.
    let none = Secrets::fixed([("OTHER", "x")]);
    assert!(poll_repo(s, &none, &app).await.unwrap().is_none());
    assert!(jobs(s).await.is_empty());
    db.drop().await;
}
