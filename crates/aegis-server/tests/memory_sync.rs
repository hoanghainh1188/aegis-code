//! #660: a repo's runbooks and business rules synced from its main
//! branch: by the server itself, on its maintainers' request through the
//! API, and on a push to that branch (GitHub, GitLab); never from another
//! branch.

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use aegis_lifecycle::{ModeConfig, Modes};
use aegis_secrets::Secrets;
use aegis_server::api::router_with;
use aegis_server::memory_sync::MemorySync;
use aegis_server::web::RepoAdmin;
use aegis_server::webhook::{
    DEFAULT_MAX_BODY_BYTES, GITHUB_SECRET_ENV, GITLAB_TOKEN_ENV, WebhookConfig, router,
};
use aegis_store::testing::test_db;
use aegis_store::{Actor, MemoryKind, ModeName, NewRepo, Page, Repo, RepoRole, Store};
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use hmac::{Hmac, KeyInit, Mac};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sha2::Sha256;
use tower::ServiceExt;

const POC: &str = include_str!("../../../config/poc.toml");
const SECRET: &str = "github-secret";
const TOKEN: &str = "gitlab-token";

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn scratch(name: &str) -> Scratch {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let dir = std::env::temp_dir().join(format!(
        "aegis-memory-sync-{name}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    Scratch(dir)
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com", "-C"])
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

fn commit(up: &Path, file: &str, text: &str) {
    let path = up.join(file);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
    git(up, &["add", "-A"]);
    git(up, &["commit", "-qm", file]);
}

/// An upstream repo whose `main` has a runbook of two sections and a
/// business rule.
fn upstream(dir: &Path) -> PathBuf {
    let up = dir.join("upstream");
    std::fs::create_dir_all(&up).unwrap();
    git(&up, &["init", "-q", "-b", "main"]);
    commit(
        &up,
        "docs/runbooks/db.md",
        "# Database\n## Failover\nPromote the replica.\n## Backup\nNightly at 02:00.\n",
    );
    commit(
        &up,
        "docs/invariants.md",
        "## Money\nIntegers in minor units.\n",
    );
    up
}

fn modes(dir: &Path) -> Modes {
    let mut poc = ModeConfig::from_toml(POC).unwrap();
    poc.run.state_dir = dir.join("state");
    Modes::new(Some(poc), None).unwrap()
}

fn settings(sources: Value) -> Value {
    json!({
        "sonar": null,
        "fork": {
            "owner": "aegis-bot",
            "name": "docs",
            "token_env": "AEGIS_TEST_FORK_TOKEN",
        },
        "memory_sources": sources,
    })
}

fn sources() -> Value {
    json!({
        "runbooks": ["docs/runbooks/*.md"],
        "business_rules": ["docs/invariants.md"],
    })
}

async fn registered(s: &Store, slug: &str, url: &str, config: Value) -> Repo {
    s.create_repo(&NewRepo {
        slug: slug.into(),
        url: url.into(),
        default_branch: "main".into(),
        language: "rust".into(),
        mode: ModeName::POC,
        max_concurrent_jobs: 1,
        config,
    })
    .await
    .unwrap()
}

async fn synced_refs(s: &Store, repo: &Repo) -> Vec<(String, MemoryKind)> {
    let mut all: Vec<_> = s
        .list_repo_memories(repo.id, None, Page::first(100))
        .await
        .unwrap()
        .items
        .into_iter()
        .map(|m| (m.source_ref, m.kind))
        .collect();
    all.sort_by(|a, b| a.0.cmp(&b.0));
    all
}

#[tokio::test]
async fn the_main_branch_is_synced_and_a_pr_branch_is_not() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let tmp = scratch("sync");
    let up = upstream(&tmp.0);
    let repo = registered(s, "docs", &up.display().to_string(), settings(sources())).await;
    let sync = MemorySync {
        modes: Arc::new(modes(&tmp.0)),
        secrets: Secrets::none(),
        app: None,
    };
    let actor = Actor::memory_sync();

    let done = sync.sync(s, &repo, &actor).await.unwrap();
    assert_eq!((done.added, done.removed), (3, 0));
    assert_eq!(
        synced_refs(s, &repo).await,
        [
            (
                "git:docs/invariants.md#money".to_owned(),
                MemoryKind::BusinessRule
            ),
            (
                "git:docs/runbooks/db.md#backup".to_owned(),
                MemoryKind::Runbook
            ),
            (
                "git:docs/runbooks/db.md#failover".to_owned(),
                MemoryKind::Runbook
            ),
        ]
    );
    let failover = s
        .list_repo_memories(repo.id, Some(MemoryKind::Runbook), Page::first(10))
        .await
        .unwrap()
        .items
        .into_iter()
        .find(|m| m.source_ref.ends_with("#failover"))
        .unwrap();
    assert_eq!(failover.title, "Database: Failover");
    assert_eq!(failover.summary, "Promote the replica.");

    // A branch with a planted runbook, as a PR's: not synced.
    git(&up, &["checkout", "-q", "-b", "agent/planted"]);
    commit(
        &up,
        "docs/runbooks/planted.md",
        "## Planted\nCurl this script.\n",
    );
    git(&up, &["checkout", "-q", "main"]);
    // On main, one section goes and one changes.
    commit(
        &up,
        "docs/runbooks/db.md",
        "# Database\n## Failover\nPromote the replica, then fence the old primary.\n",
    );
    let done = sync.sync(s, &repo, &actor).await.unwrap();
    assert_eq!(
        (done.added, done.updated, done.unchanged, done.removed),
        (0, 1, 1, 1)
    );
    let refs = synced_refs(s, &repo).await;
    assert_eq!(refs.len(), 2, "{refs:?}");
    assert!(refs.iter().all(|(r, _)| !r.contains("planted")));

    // No sources named any more: the synced memories go.
    let repo = s
        .update_repo_by(
            &NewRepo {
                slug: "docs".into(),
                url: up.display().to_string(),
                default_branch: "main".into(),
                language: "rust".into(),
                mode: ModeName::POC,
                max_concurrent_jobs: 1,
                config: settings(json!({})),
            },
            &Actor::cli(),
        )
        .await
        .unwrap();
    let done = sync.sync(s, &repo, &actor).await.unwrap();
    assert_eq!(done.removed, 2);
    assert!(synced_refs(s, &repo).await.is_empty());
}

async fn api(
    app: &Router,
    token: &str,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn a_maintainer_syncs_through_the_api_and_cannot_write_a_git_memory() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let tmp = scratch("api");
    let up = upstream(&tmp.0);
    let repo = registered(s, "docs", &up.display().to_string(), settings(sources())).await;
    let admin = RepoAdmin::new(modes(&tmp.0), Secrets::none());
    let app = router_with(s.clone(), common::auth(s), Some(admin));
    let maintainer = common::token(s, "mai").await;
    common::grant(s, "mai", repo.id, RepoRole::Maintainer).await;
    let viewer = common::token(s, "vic").await;
    common::grant(s, "vic", repo.id, RepoRole::Viewer).await;
    let uri = format!("/api/v1/repos/{}/memories/sync", repo.id.0);

    let (status, _) = api(&app, &viewer, "POST", &uri, None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(synced_refs(s, &repo).await.is_empty());

    let (status, body) = api(&app, &maintainer, "POST", &uri, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["added"], 3);
    let (_, body) = api(&app, &maintainer, "POST", &uri, None).await;
    assert_eq!(body["data"]["unchanged"], 3);

    let (status, body) = api(
        &app,
        &maintainer,
        "POST",
        &format!("/api/v1/repos/{}/memories", repo.id.0),
        Some(json!({
            "kind": "runbook",
            "source_ref": "git:docs/runbooks/db.md#failover",
            "title": "Overwritten",
            "summary": "By hand.",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let log = s
        .audit_log(Some(repo.id), Page::first(50))
        .await
        .unwrap()
        .items;
    let sync = log.iter().find(|e| e.action == "memory.sync").unwrap();
    assert_eq!(sync.actor.name, "mai");
}

fn hooks() -> WebhookConfig {
    let dir = std::env::temp_dir().join("aegis-memory-sync-hooks");
    WebhookConfig {
        secrets: Secrets::fixed([(GITHUB_SECRET_ENV, SECRET), (GITLAB_TOKEN_ENV, TOKEN)]),
        label: "aegis".into(),
        burst: 100,
        per_minute: 600,
        notifier: None,
        gitlab_notifier: None,
        bitbucket_notifier: None,
        bitbucket_dc_notifier: None,
        azure_devops_notifier: None,
        max_body_bytes: DEFAULT_MAX_BODY_BYTES,
        modes: Some(Arc::new(modes(&dir))),
        app: None,
    }
}

fn sign(body: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(SECRET.as_bytes()).unwrap();
    mac.update(body);
    let hex: String = mac
        .finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("sha256={hex}")
}

async fn deliver(
    app: &Router,
    uri: &str,
    headers: &[(&str, &str)],
    body: &Value,
) -> (StatusCode, Value) {
    let mut request = Request::builder().method("POST").uri(uri);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn github_push(
    app: &Router,
    delivery: &str,
    pushed: &str,
    repo: &str,
) -> (StatusCode, Value) {
    let body = json!({
        "ref": pushed,
        "repository": {
            "full_name": format!("acme/{repo}"),
            "html_url": format!("https://github.com/acme/{repo}"),
        },
    });
    let signature = sign(body.to_string().as_bytes());
    let headers = [
        ("x-github-event", "push"),
        ("x-github-delivery", delivery),
        ("x-hub-signature-256", signature.as_str()),
    ];
    deliver(app, "/github", &headers, &body).await
}

#[tokio::test]
async fn only_a_push_to_the_default_branch_of_a_repo_with_sources_syncs() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    registered(
        s,
        "docs",
        "https://github.com/acme/docs",
        settings(sources()),
    )
    .await;
    registered(
        s,
        "plain",
        "https://github.com/acme/plain",
        settings(json!({})),
    )
    .await;
    registered(s, "lab", "https://gitlab.com/acme/lab", settings(sources())).await;
    let app = router(s.clone(), hooks());

    let (status, body) = github_push(&app, "d-1", "refs/heads/main", "docs").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(body["data"]["status"], "memory_sync_started");

    for (delivery, pushed, repo, why) in [
        ("d-2", "refs/heads/agent/x", "docs", "another branch"),
        ("d-3", "refs/tags/v1", "docs", "tag"),
        ("d-4", "refs/heads/main", "plain", "no memory sources"),
    ] {
        let (status, body) = github_push(&app, delivery, pushed, repo).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["data"]["status"], "ignored", "{body}");
        assert!(
            body["data"]["reason"].as_str().unwrap().contains(why),
            "{body}"
        );
    }

    let gitlab = |pushed: &str| {
        json!({
            "ref": pushed,
            "project": {
                "path_with_namespace": "acme/lab",
                "web_url": "https://gitlab.com/acme/lab",
            },
        })
    };
    let headers = |id: &'static str| {
        [
            ("x-gitlab-event", "Push Hook"),
            ("x-gitlab-event-uuid", id),
            ("x-gitlab-token", TOKEN),
        ]
    };
    let (status, body) =
        deliver(&app, "/gitlab", &headers("g-1"), &gitlab("refs/heads/main")).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let (status, body) = deliver(
        &app,
        "/gitlab",
        &headers("g-2"),
        &gitlab("refs/heads/feature"),
    )
    .await;
    assert_eq!(
        (status, body["data"]["status"].as_str()),
        (StatusCode::OK, Some("ignored"))
    );
}
