//! #590: every page renders in English, Japanese and Vietnamese, chosen by
//! the `aegis_lang` cookie, the browser's `Accept-Language` or the
//! server's `[web] default_locale`, and the switch remembers a choice.

mod common;

use aegis_ledger::IssueId;
use aegis_pr::Locale;
use aegis_server::api::router;
use aegis_store::testing::test_db;
use aegis_store::{JobSource, ModeName, NewJob, NewRepo, Store};
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::Browser;
use http_body_util::BodyExt;
use serde_json::json;
use tower::ServiceExt;

struct Answer {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    body: String,
}

async fn get(
    app: &Router,
    who: &Browser,
    uri: &str,
    extra: &[(header::HeaderName, &str)],
) -> Answer {
    let mut request = Request::get(uri)
        .header(header::HOST, "localhost:8080")
        .header(header::COOKIE, &who.cookie);
    for (name, value) in extra {
        request = request.header(name, *value);
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let (status, headers) = (response.status(), response.headers().clone());
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    Answer {
        status,
        headers,
        body: String::from_utf8(bytes.to_vec()).unwrap(),
    }
}

fn with_lang(who: &Browser, code: &str) -> Browser {
    Browser {
        user: who.user,
        cookie: format!("{}; aegis_lang={code}", who.cookie),
        csrf: who.csrf.clone(),
    }
}

async fn fixture(s: &Store) -> (i64, i64) {
    let repo = s
        .create_repo(&NewRepo {
            slug: "app".into(),
            url: "https://github.com/x/app".into(),
            default_branch: "main".into(),
            language: "rust".into(),
            mode: ModeName::POC,
            max_concurrent_jobs: 1,
            config: json!({}),
        })
        .await
        .unwrap();
    let job = s
        .create_job(&NewJob {
            repo_id: repo.id,
            issue_ref: "x/app#1".into(),
            ledger_key: IssueId::parse("app-1").unwrap(),
            source: JobSource::Github,
            priority: None,
            sla_deadline: None,
        })
        .await
        .unwrap();
    (repo.id.0, job.id.0)
}

/// The pages an admin reaches with what [`fixture`] made.
fn pages(repo: i64, job: i64, user: i64) -> Vec<String> {
    [
        "/",
        "/ams",
        "/jobs",
        "/admin/ai",
        "/admin/audit",
        "/admin/installations",
        "/admin/pause",
        "/admin/users",
        "/admin/repos",
        "/me/tokens",
    ]
    .iter()
    .map(|p| (*p).to_owned())
    .chain([
        format!("/jobs/{job}"),
        format!("/jobs/{job}/head"),
        format!("/admin/repos/{repo}"),
        format!("/admin/users/{user}"),
        format!("/repos/{repo}/budget"),
    ])
    .collect()
}

#[tokio::test]
async fn every_page_renders_in_every_language() {
    let Some(db) = test_db().await else {
        return;
    };
    let s = &db.store;
    let app = router(s.clone(), common::auth(s));
    common::admin(s, "lan@example.com").await;
    let who = common::sign_in(s, "lan@example.com").await;
    let (repo, job) = fixture(s).await;
    let mut failed = Vec::new();
    for page in pages(repo, job, who.user.0) {
        for (code, nav) in [("en", ">Jobs<"), ("ja", ">ジョブ<"), ("vi", ">Job<")] {
            let a = get(&app, &with_lang(&who, code), &page, &[]).await;
            let full = a.body.contains("<html");
            // Without modes the AI page is the error page, in the language.
            if page == "/admin/ai" && a.status == StatusCode::SERVICE_UNAVAILABLE {
                if !a.body.contains(&format!("<html lang=\"{code}\">")) {
                    failed.push(format!("{page} [{code}]: error page not in the language"));
                }
                continue;
            }
            if a.status != StatusCode::OK
                || (full && !a.body.contains(&format!("<html lang=\"{code}\">")))
                || (full && !a.body.contains(nav))
            {
                failed.push(format!("{page} [{code}]: {}", a.status));
            }
            if code == "ja" && full && a.body.contains(">Sign out<") {
                failed.push(format!("{page} [ja]: English masthead"));
            }
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n"));
    db.drop().await;
}

#[tokio::test]
async fn the_cookie_wins_then_the_browser_then_the_servers_default() {
    let Some(db) = test_db().await else {
        return;
    };
    let s = &db.store;
    let auth = common::auth(s).with_default_locale(Locale::Vi);
    let app = router(s.clone(), auth);
    let who = common::sign_in(s, "lan@example.com").await;
    let lang = |a: &Answer| {
        a.body
            .split("<html lang=\"")
            .nth(1)
            .and_then(|r| r.split('"').next())
            .map(str::to_owned)
    };
    let plain = get(&app, &who, "/jobs", &[]).await;
    assert_eq!(lang(&plain).as_deref(), Some("vi"), "the server's default");
    assert!(plain.body.contains("Đăng xuất"), "{}", plain.body);
    let vary = plain.headers.get(header::VARY).unwrap().to_str().unwrap();
    assert!(vary.contains("Cookie") && vary.contains("Accept-Language"));
    let browser = get(
        &app,
        &who,
        "/jobs",
        &[(header::ACCEPT_LANGUAGE, "ja-JP,ja;q=0.9,en;q=0.5")],
    )
    .await;
    assert_eq!(lang(&browser).as_deref(), Some("ja"));
    let chosen = get(
        &app,
        &with_lang(&who, "en"),
        "/jobs",
        &[(header::ACCEPT_LANGUAGE, "ja")],
    )
    .await;
    assert_eq!(lang(&chosen).as_deref(), Some("en"));
    // The sign-in page has no session, and still speaks the browser's.
    let login = get(
        &app,
        &who,
        "/auth/signed-out",
        &[(header::ACCEPT_LANGUAGE, "ja")],
    )
    .await;
    if login.body.contains("<html") {
        assert_eq!(lang(&login).as_deref(), Some("ja"), "{}", login.body);
    }
    db.drop().await;
}

#[tokio::test]
async fn the_switch_remembers_and_goes_back_to_this_sites_page() {
    let Some(db) = test_db().await else {
        return;
    };
    let s = &db.store;
    let app = router(s.clone(), common::auth(s));
    let who = common::sign_in(s, "lan@example.com").await;
    let back = get(
        &app,
        &who,
        "/lang/ja",
        &[(header::REFERER, "http://localhost:8080/jobs?status=done")],
    )
    .await;
    assert!(back.status.is_redirection(), "{}", back.status);
    assert_eq!(back.headers[header::LOCATION], "/jobs?status=done");
    let cookie = back.headers[header::SET_COOKIE].to_str().unwrap();
    assert!(cookie.starts_with("aegis_lang=ja;"), "{cookie}");
    assert!(cookie.contains("HttpOnly") && cookie.contains("SameSite=Lax"));
    let elsewhere = get(
        &app,
        &who,
        "/lang/vi",
        &[(header::REFERER, "https://evil.example/x")],
    )
    .await;
    assert_eq!(elsewhere.headers[header::LOCATION], "/");
    let unknown = get(&app, &who, "/lang/fr", &[]).await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);
    db.drop().await;
}

#[tokio::test]
async fn the_switch_keeps_its_referrer_and_assets_do_not_vary() {
    let Some(db) = test_db().await else {
        return;
    };
    let s = &db.store;
    let app = router(s.clone(), common::auth(s));
    let who = common::sign_in(s, "lan@example.com").await;
    // Every page sends no referrer; the switch asks for this site's own.
    let page = get(&app, &who, "/jobs", &[]).await;
    assert!(
        page.body
            .contains("href=\"/lang/ja\" referrerpolicy=\"same-origin\""),
        "{}",
        page.body
    );
    let css = get(&app, &who, "/static/app.css", &[]).await;
    assert_eq!(css.status, StatusCode::OK);
    assert!(css.headers.get(header::VARY).is_none());
    db.drop().await;
}
