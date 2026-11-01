//! #329: an admin sees who has which role in each repo and changes it from
//! the pages, through the store's `set_repo_role` like `aegis-server
//! grant|revoke`, audited as the admin. `/admin/*` does not exist for
//! anyone else. #330: an admin adds and changes repos from the pages,
//! checked as `aegis-server repo add|update` checks them, the clone made
//! in the background and shown on the repo's page.

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use aegis_ledger::IssueId;
use aegis_lifecycle::{ModeConfig, Modes};
use aegis_secrets::Secrets;
use aegis_server::api::{router, router_with};
use aegis_server::web::RepoAdmin;
use aegis_store::testing::test_db;
use aegis_store::{
    ActorKind, CloneState, JobSource, NewJob, NewRepo, Page, RepoClone, RepoId, RepoMode, RepoRole,
    Store,
};
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::Browser;
use http_body_util::BodyExt;
use serde_json::json;
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

async fn get(app: &Router, who: &Browser, uri: &str) -> Answer {
    let request = Request::get(uri)
        .header(header::HOST, "localhost:8080")
        .header(header::COOKIE, &who.cookie)
        .body(Body::empty())
        .unwrap();
    send(app, request).await
}

async fn post(app: &Router, who: &Browser, uri: &str, fields: &str) -> Answer {
    let request = Request::post(uri)
        .header(header::HOST, "localhost:8080")
        .header(header::ORIGIN, "http://localhost:8080")
        .header(header::COOKIE, &who.cookie)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(format!("{fields}&_csrf={}", who.csrf)))
        .unwrap();
    send(app, request).await
}

async fn repo(s: &Store, slug: &str) -> RepoId {
    s.create_repo(&NewRepo {
        slug: slug.into(),
        url: format!("https://github.com/x/{slug}"),
        default_branch: "main".into(),
        language: "rust".into(),
        mode: RepoMode::Poc,
        max_concurrent_jobs: 2,
        config: json!({}),
    })
    .await
    .unwrap()
    .id
}

/// The last audit entry: `(kind, name, action, repo)`.
async fn last_audit(s: &Store) -> (ActorKind, String, String, Option<i64>) {
    let e = s
        .audit_log(None, Page::first(1))
        .await
        .unwrap()
        .items
        .remove(0);
    (e.actor.kind, e.actor.name, e.action, e.repo_id.map(|r| r.0))
}

#[tokio::test]
async fn only_an_admin_sees_admin_pages() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let a = repo(s, "repo-a").await;
    common::admin(s, "admin@x.test").await;
    common::grant(s, "maint@x.test", a, RepoRole::Maintainer).await;
    let admin = common::sign_in(s, "admin@x.test").await;
    let maint = common::sign_in(s, "maint@x.test").await;
    let app = router(s.clone(), common::auth(s));

    // The masthead offers the admin pages to an admin only.
    assert!(
        get(&app, &admin, "/jobs")
            .await
            .body
            .contains("href=\"/admin/repos\"")
    );
    assert!(
        !get(&app, &maint, "/jobs")
            .await
            .body
            .contains("/admin/repos")
    );

    // Anyone else: the pages and the forms do not exist.
    for uri in ["/admin/repos".to_owned(), format!("/admin/repos/{}", a.0)] {
        assert_eq!(
            get(&app, &maint, &uri).await.status,
            StatusCode::NOT_FOUND,
            "{uri}"
        );
    }
    let grant = format!("/admin/repos/{}/roles", a.0);
    let sent = post(&app, &maint, &grant, "who=x%40x.test&role=viewer").await;
    assert_eq!(sent.status, StatusCode::NOT_FOUND);

    // The admin's list holds every repo, with its settings.
    let list = get(&app, &admin, "/admin/repos").await;
    assert_eq!(list.status, StatusCode::OK);
    assert!(
        list.body.contains("repo-a")
            && list.body.contains("https://github.com/x/repo-a")
            && list
                .body
                .contains(&format!("href=\"/admin/repos/{}\"", a.0)),
        "{}",
        list.body
    );
    db.drop().await;
}

#[tokio::test]
async fn an_admin_grants_and_revokes_roles_at_once() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let a = repo(s, "repo-a").await;
    s.create_job(&NewJob {
        repo_id: a,
        issue_ref: "x/y#1".into(),
        ledger_key: IssueId::parse("a-1").unwrap(),
        source: JobSource::Github,
    })
    .await
    .unwrap();
    common::admin(s, "admin@x.test").await;
    let admin = common::sign_in(s, "admin@x.test").await;
    let new = common::sign_in(s, "new@x.test").await;
    let app = router(s.clone(), common::auth(s));
    let page = format!("/admin/repos/{}", a.0);

    // No role: the repo's jobs are not there for them.
    assert!(!get(&app, &new, "/jobs").await.body.contains("x/y#1"));

    let sent = post(
        &app,
        &admin,
        &format!("{page}/roles"),
        "who=New%40X.test&role=reviewer",
    )
    .await;
    assert_eq!(sent.status, StatusCode::SEE_OTHER, "{}", sent.body);
    assert_eq!(sent.location, format!("{page}?done=granted"));
    assert_eq!(
        last_audit(s).await,
        (
            ActorKind::User,
            "admin@x.test".into(),
            "role.grant".into(),
            Some(a.0)
        )
    );
    let shown = get(&app, &admin, &sent.location).await;
    assert!(
        shown.body.contains("new@x.test") && shown.body.contains("reviewer"),
        "{}",
        shown.body
    );
    assert!(shown.body.contains("role=\"status\""), "{}", shown.body);
    // At once, for their next request.
    assert!(get(&app, &new, "/jobs").await.body.contains("x/y#1"));

    let sent = post(
        &app,
        &admin,
        &format!("{page}/roles/revoke"),
        "who=new%40x.test",
    )
    .await;
    assert_eq!(sent.status, StatusCode::SEE_OTHER, "{}", sent.body);
    assert_eq!(last_audit(s).await.2, "role.revoke");
    assert!(!get(&app, &new, "/jobs").await.body.contains("x/y#1"));
    assert!(!get(&app, &admin, &page).await.body.contains("new@x.test"));

    // What the store refuses, said plainly.
    for fields in ["who=&role=viewer", "who=a%40b.test&role=owner"] {
        let sent = post(&app, &admin, &format!("{page}/roles"), fields).await;
        assert_eq!(
            sent.status,
            StatusCode::BAD_REQUEST,
            "{fields}: {}",
            sent.body
        );
    }
    let missing = post(
        &app,
        &admin,
        "/admin/repos/999/roles",
        "who=a%40b.test&role=viewer",
    )
    .await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    db.drop().await;
}

const POC: &str = include_str!("../../../config/poc.toml");

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
        "aegis-web-admin-{name}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    Scratch(dir)
}

/// An upstream repo with one commit on `main`.
fn upstream(dir: &Path) -> PathBuf {
    let up = dir.join("upstream");
    std::fs::create_dir_all(&up).unwrap();
    std::fs::write(up.join("a.txt"), "one").unwrap();
    for args in [
        &["init", "-q", "-b", "main"][..],
        &["add", "-A"],
        &["commit", "-qm", "one"],
    ] {
        let out = Command::new("git")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com", "-C"])
            .arg(&up)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
    }
    up
}

/// The poc mode, its state under `dir`; no `customer-jp` mode.
fn repo_admin(dir: &Path) -> RepoAdmin {
    let mut poc = ModeConfig::from_toml(POC).unwrap();
    poc.run.state_dir = dir.join("state");
    RepoAdmin::new(Modes::new(Some(poc), None).unwrap(), Secrets::none())
}

/// `fields` as a form body, each value percent-encoded.
fn form(fields: &[(&str, &str)]) -> String {
    let encode = |v: &str| {
        v.bytes()
            .map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    (b as char).to_string()
                }
                _ => format!("%{b:02X}"),
            })
            .collect::<String>()
    };
    fields
        .iter()
        .map(|(k, v)| format!("{k}={}", encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

fn demo<'a>(url: &'a str, mode: &'a str, jobs: &'a str) -> Vec<(&'static str, &'a str)> {
    vec![
        ("slug", "demo"),
        ("url", url),
        ("default_branch", "main"),
        ("language", "go"),
        ("mode", mode),
        ("max_concurrent_jobs", jobs),
        ("fork_platform", "github"),
        ("fork_owner", "aegis-bot"),
        ("fork_name", "demo"),
        ("token_env", "AEGIS_TEST_FORK_TOKEN"),
        ("conventions", ""),
        ("settings", ""),
    ]
}

/// The repo's clone once it is no longer being made.
async fn clone_ended(s: &Store, repo: RepoId) -> RepoClone {
    for _ in 0..200 {
        let clone = s.repo_clone(repo).await.unwrap().unwrap();
        if clone.state != CloneState::Cloning {
            return clone;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("the clone of repo {} did not end", repo.0);
}

/// The repo `/admin/repos/<id>?done=…` names.
fn repo_in(location: &str) -> RepoId {
    let id = location
        .trim_start_matches("/admin/repos/")
        .split('?')
        .next()
        .unwrap();
    RepoId(id.parse().unwrap())
}

#[tokio::test]
async fn an_admin_adds_a_repo_follows_its_clone_and_changes_it() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let tmp = scratch("add");
    let up = upstream(&tmp.0);
    common::admin(s, "admin@x.test").await;
    let admin = common::sign_in(s, "admin@x.test").await;
    let app = router_with(s.clone(), common::auth(s), Some(repo_admin(&tmp.0)));

    let list = get(&app, &admin, "/admin/repos").await;
    assert!(
        list.body.contains("href=\"/admin/repos/new\""),
        "{}",
        list.body
    );
    let page = get(&app, &admin, "/admin/repos/new").await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    // No field takes a secret: the fork's token is named, never given.
    assert!(!page.body.contains("type=\"password\""), "{}", page.body);
    let start = page
        .body
        .find("<form method=\"post\" action=\"/admin/repos\"")
        .unwrap();
    let end = start + page.body[start..].find("</form>").unwrap();
    let names: Vec<&str> = page.body[start..end]
        .split("name=\"")
        .skip(1)
        .filter_map(|rest| rest.split('"').next())
        .collect();
    for name in &names {
        assert!(
            [
                "_csrf",
                "slug",
                "url",
                "default_branch",
                "language",
                "mode",
                "max_concurrent_jobs",
                "fork_platform",
                "fork_owner",
                "fork_name",
                "token_env",
                "conventions",
                "settings",
                "htmx-config",
            ]
            .contains(name),
            "a field `{name}` the issue does not allow"
        );
    }
    assert!(names.contains(&"token_env"), "{names:?}");

    let url = up.display().to_string();
    let sent = post(&app, &admin, "/admin/repos", &form(&demo(&url, "poc", "1"))).await;
    assert_eq!(sent.status, StatusCode::SEE_OTHER, "{}", sent.body);
    assert!(sent.location.ends_with("?done=added"), "{}", sent.location);
    let id = repo_in(&sent.location);
    assert_eq!(
        last_audit(s).await,
        (
            ActorKind::User,
            "admin@x.test".into(),
            "repo.register".into(),
            Some(id.0)
        )
    );
    assert_eq!(clone_ended(s, id).await.state, CloneState::Done);
    assert!(tmp.0.join("state/repos/demo").exists());
    let shown = get(&app, &admin, &format!("/admin/repos/{}", id.0)).await;
    assert!(shown.body.contains("Clone ready"), "{}", shown.body);
    assert!(
        shown.body.contains("slug = &#34;demo&#34;") || shown.body.contains("slug = \"demo\""),
        "{}",
        shown.body
    );
    assert!(
        shown.body.contains("AEGIS_TEST_FORK_TOKEN"),
        "{}",
        shown.body
    );

    // Changed: the form starts from the repo, the slug stays.
    let edit = get(&app, &admin, &format!("/admin/repos/{}/edit", id.0)).await;
    assert_eq!(edit.status, StatusCode::OK, "{}", edit.body);
    assert!(edit.body.contains("value=\"aegis-bot\""), "{}", edit.body);
    let mut fields = demo(&url, "poc", "3");
    fields[0] = ("slug", "renamed");
    let sent = post(
        &app,
        &admin,
        &format!("/admin/repos/{}/edit", id.0),
        &form(&fields),
    )
    .await;
    assert_eq!(sent.status, StatusCode::SEE_OTHER, "{}", sent.body);
    assert_eq!(last_audit(s).await.2, "repo.update");
    let repo = s.repo(id).await.unwrap().unwrap();
    assert_eq!((repo.slug.as_str(), repo.max_concurrent_jobs), ("demo", 3));
    assert_eq!(clone_ended(s, id).await.state, CloneState::Done);
    db.drop().await;
}

#[tokio::test]
async fn a_wrong_repo_is_refused_and_a_failed_clone_shows_why() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let tmp = scratch("refuse");
    common::admin(s, "admin@x.test").await;
    let admin = common::sign_in(s, "admin@x.test").await;
    let app = router_with(s.clone(), common::auth(s), Some(repo_admin(&tmp.0)));
    let url = tmp.0.join("upstream").display().to_string();

    // A mode the server has no configuration for, or none at all.
    for (mode, why) in [
        ("customer-jp", "no configuration for mode `customer-jp`"),
        ("nope", "unknown mode `nope`"),
    ] {
        let sent = post(&app, &admin, "/admin/repos", &form(&demo(&url, mode, "1"))).await;
        assert_eq!(
            sent.status,
            StatusCode::BAD_REQUEST,
            "{mode}: {}",
            sent.body
        );
        assert!(sent.body.contains(why), "{mode}: {}", sent.body);
        // The form comes back as it was sent.
        assert!(sent.body.contains("value=\"aegis-bot\""), "{}", sent.body);
    }
    assert!(s.repo_by_slug("demo").await.unwrap().is_none());

    // No upstream there: registered, but its clone fails, and says why.
    let sent = post(&app, &admin, "/admin/repos", &form(&demo(&url, "poc", "1"))).await;
    assert_eq!(sent.status, StatusCode::SEE_OTHER, "{}", sent.body);
    let id = repo_in(&sent.location);
    let clone = clone_ended(s, id).await;
    assert_eq!(clone.state, CloneState::Failed);
    let log = clone.log_tail.unwrap();
    assert!(
        log.contains("does not exist") || log.contains("not found"),
        "{log}"
    );
    let shown = get(&app, &admin, &format!("/admin/repos/{}", id.0)).await;
    assert!(shown.body.contains("Clone failed"), "{}", shown.body);
    assert!(
        shown.body.contains("does not exist") || shown.body.contains("not found"),
        "{}",
        shown.body
    );
    let part = get(&app, &admin, &format!("/admin/repos/{}/clone", id.0)).await;
    assert!(
        part.body.contains("Clone failed") && !part.body.contains("hx-trigger"),
        "{}",
        part.body
    );
    db.drop().await;
}

#[tokio::test]
async fn only_an_admin_adds_repos_and_only_where_modes_are_loaded() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let tmp = scratch("who");
    let a = repo(s, "repo-a").await;
    common::admin(s, "admin@x.test").await;
    common::grant(s, "maint@x.test", a, RepoRole::Maintainer).await;
    let admin = common::sign_in(s, "admin@x.test").await;
    let maint = common::sign_in(s, "maint@x.test").await;
    let app = router_with(s.clone(), common::auth(s), Some(repo_admin(&tmp.0)));
    let url = tmp.0.join("upstream").display().to_string();

    for uri in [
        "/admin/repos/new".to_owned(),
        format!("/admin/repos/{}/edit", a.0),
    ] {
        assert_eq!(
            get(&app, &maint, &uri).await.status,
            StatusCode::NOT_FOUND,
            "{uri}"
        );
    }
    let sent = post(&app, &maint, "/admin/repos", &form(&demo(&url, "poc", "1"))).await;
    assert_eq!(sent.status, StatusCode::NOT_FOUND);

    // A server that did not load its modes shows repos, adds none.
    let bare = router(s.clone(), common::auth(s));
    assert!(
        !get(&bare, &admin, "/admin/repos")
            .await
            .body
            .contains("/admin/repos/new")
    );
    let page = get(&bare, &admin, "/admin/repos/new").await;
    assert_eq!(
        page.status,
        StatusCode::SERVICE_UNAVAILABLE,
        "{}",
        page.body
    );
    assert!(page.body.contains("repo add"), "{}", page.body);
    db.drop().await;
}
