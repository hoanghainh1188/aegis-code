//! The dashboard and the reviewer's pages (#28): KPIs that match the
//! ledger's report, a review form that writes `review` to the ledger with
//! the time it took, and pages that stay on known hosts and behind the CSP.
//! Here as an admin; who may see what is in `auth.rs`.

mod common;

use aegis_ledger::{
    IssueId, Kpi, LedgerEvent, LedgerStore, Outcome, Report, Review, ReviewVerdict, Tier,
    TokenCounts, report,
};
use aegis_server::api::router;
use aegis_server::auth::Signer;
use aegis_server::web::dashboard_kpis;
use aegis_server::web::opened_at_signature;
use aegis_store::testing::test_db;
use aegis_store::{JobEnd, JobId, JobSource, NewJob, NewRepo, RepoId, RepoMode, Store};
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::json;
use tower::ServiceExt;

struct Answer {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    body: String,
}

/// The pages, and an admin's browser.
struct App {
    router: Router,
    browser: common::Browser,
}

async fn app(store: &Store) -> App {
    common::admin(store, "admin@example.test").await;
    App {
        router: router(store.clone(), common::auth(store)),
        browser: common::sign_in(store, "admin@example.test").await,
    }
}

async fn send(app: &App, request: Request<Body>) -> Answer {
    let response = app.router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    Answer {
        status,
        headers,
        body: String::from_utf8(bytes.to_vec()).unwrap(),
    }
}

async fn get(app: &App, uri: &str) -> Answer {
    let request = Request::get(uri)
        .header(header::HOST, "localhost:8080")
        .header(header::COOKIE, &app.browser.cookie)
        .body(Body::empty())
        .unwrap();
    send(app, request).await
}

/// With the session's CSRF token, as `form_guard.html` adds it.
async fn post_form(app: &App, uri: &str, form: &str) -> Answer {
    let request = Request::post(uri)
        .header(header::HOST, "localhost:8080")
        .header(header::ORIGIN, "http://localhost:8080")
        .header(header::COOKIE, &app.browser.cookie)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(format!("{form}&_csrf={}", app.browser.csrf)))
        .unwrap();
    send(app, request).await
}

async fn repo(s: &Store, slug: &str, language: &str) -> RepoId {
    s.create_repo(&NewRepo {
        slug: slug.into(),
        url: format!("https://github.com/x/{slug}"),
        default_branch: "main".into(),
        language: language.into(),
        mode: RepoMode::Poc,
        max_concurrent_jobs: 1,
        config: json!({}),
    })
    .await
    .unwrap()
    .id
}

fn call(cost_usd: f64, tier: Tier) -> LedgerEvent {
    LedgerEvent::ModelCall {
        tier,
        model: "core".into(),
        tokens: TokenCounts {
            input: 10,
            cache_read: 90,
            ..TokenCounts::default()
        },
        cost_usd,
    }
}

/// A job of `repo` whose ledger holds `events`, ended with `outcome`.
async fn job(
    s: &Store,
    repo: RepoId,
    key: &str,
    outcome: Option<Outcome>,
    events: &[LedgerEvent],
) -> JobId {
    let job = s
        .create_job(&NewJob {
            repo_id: repo,
            issue_ref: format!("x/y#{key}"),
            ledger_key: IssueId::parse(key).unwrap(),
            source: JobSource::Cli,
        })
        .await
        .unwrap();
    let ledger = s.ledger();
    for event in events {
        ledger.append(&job.ledger_key, event).await.unwrap();
    }
    if let Some(outcome) = outcome {
        let finished = LedgerEvent::Finished {
            outcome,
            wall_time_s: 120.0,
        };
        ledger.append(&job.ledger_key, &finished).await.unwrap();
        s.end_job(job.id, JobEnd::Done(outcome)).await.unwrap();
    }
    job.id
}

fn reviewed(verdict: ReviewVerdict, lines_edited: u32, minutes: f64) -> LedgerEvent {
    LedgerEvent::Reviewed(Review {
        verdict,
        lines_edited,
        review_minutes: minutes,
        broke_tests: false,
    })
}

#[tokio::test]
async fn the_dashboard_matches_the_ledger_report() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let typst = repo(s, "typst", "Rust").await;
    let scrapy = repo(s, "scrapy", "python").await;
    let accepted = reviewed(ReviewVerdict::Accepted, 0, 2.0);
    let edited = reviewed(ReviewVerdict::AcceptedWithEdits, 12, 4.0);
    job(
        s,
        typst,
        "typst-1",
        Some(Outcome::PrOpened),
        &[call(1.0, Tier::Core), accepted.clone()],
    )
    .await;
    job(
        s,
        typst,
        "typst-2",
        Some(Outcome::StoppedSteps),
        &[call(3.0, Tier::Expert)],
    )
    .await;
    job(s, typst, "typst-3", None, &[call(9.0, Tier::Core)]).await;
    job(
        s,
        scrapy,
        "scrapy-1",
        Some(Outcome::PrOpened),
        &[call(0.5, Tier::Core), edited],
    )
    .await;
    job(
        s,
        scrapy,
        "scrapy-2",
        Some(Outcome::PrOpened),
        &[call(0.5, Tier::Core), accepted],
    )
    .await;
    // A CLI run in the same ledger, outside any job.
    let cli = IssueId::parse("eval-7-r1").unwrap();
    s.ledger()
        .append(&cli, &call(2.0, Tier::Core))
        .await
        .unwrap();
    let end = LedgerEvent::Finished {
        outcome: Outcome::StoppedBudget,
        wall_time_s: 1.0,
    };
    s.ledger().append(&cli, &end).await.unwrap();

    // What `aegis report` prints for this ledger.
    let records = report::records(&s.ledger()).await.unwrap();
    let report = Report::<String>::all(&records).to_string();
    assert!(
        report.contains("| Accepted PRs (≤ 10 lines edited) | 40% (2/5) |"),
        "{report}"
    );
    assert!(
        report.contains("| Cost per issue, failures included | $1.40 |"),
        "{report}"
    );

    let d = dashboard_kpis(s).await.unwrap();
    assert_eq!(d.total, Kpi::from_records(&records));
    let label = |g: &aegis_server::web::Group| g.label.clone();
    assert_eq!(
        d.languages.iter().map(label).collect::<Vec<_>>(),
        ["ngoài job (CLI, eval)", "python", "rust"]
    );
    let rust = &d.languages[2].kpi;
    assert_eq!((rust.runs, rust.accepted, rust.expert_runs), (2, 1, 1));
    let python = &d.languages[1].kpi;
    assert_eq!((python.runs, python.accepted), (2, 1));
    assert_eq!(
        d.repos.iter().map(label).collect::<Vec<_>>()[1..],
        ["scrapy", "typst"]
    );
    let week: usize = d.weeks.iter().map(|g| g.kpi.runs).sum();
    assert_eq!(week, 4, "jobs only, by week");

    let app = app(s).await;
    let page = get(&app, "/").await;
    assert_eq!(page.status, StatusCode::OK);
    for value in [
        "40% (2/5)",
        "$1.40",
        "3.0 min",
        "90%",
        "50% (1/2)",
        "20% (1/5)",
    ] {
        assert!(
            page.body.contains(value),
            "{value} missing from the dashboard"
        );
    }
    db.drop().await;
}

#[tokio::test]
async fn a_review_from_the_form_lands_in_the_ledger() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let typst = repo(s, "typst", "rust").await;
    let hidden = LedgerEvent::HiddenTests {
        passed: false,
        duration_s: 3.0,
    };
    let id = job(
        s,
        typst,
        "typst-9",
        Some(Outcome::PrOpened),
        &[call(1.0, Tier::Core), hidden],
    )
    .await;
    let stopped = job(s, typst, "typst-10", Some(Outcome::StoppedBudget), &[]).await;
    let app = app(s).await;

    let form = get(&app, &format!("/jobs/{}/review", id.0)).await;
    assert_eq!(form.status, StatusCode::OK);
    let csp = form.headers[header::CONTENT_SECURITY_POLICY]
        .to_str()
        .unwrap();
    let nonce = csp
        .split("'nonce-")
        .nth(1)
        .unwrap()
        .split('\'')
        .next()
        .unwrap();
    assert!(
        form.body.contains(&format!("nonce=\"{nonce}\"")),
        "{}",
        form.body
    );
    assert!(
        !csp.contains("unsafe-inline") && !csp.contains("http"),
        "{csp}"
    );
    assert!(!form.body.contains("<script>"), "no inline script");
    assert!(form.body.contains("name=\"opened_at\""));
    // Neither the form nor the job's history tells the hidden test result.
    let history = get(&app, &format!("/jobs/{}", id.0)).await;
    assert_eq!(history.status, StatusCode::OK);
    for page in [&form.body, &history.body] {
        let told = ["HiddenTests", "hidden_tests", "test ẩn", "hidden test"];
        assert!(!told.iter().any(|t| page.contains(t)), "{page}");
    }

    assert!(form.body.contains("name=\"_csrf\""));
    // The form signed a time 90 s ago for this job and this reviewer.
    let opened_at = chrono::Utc::now().timestamp_millis() - 90_000;
    let sig = opened_at_signature(
        &Signer::new(common::KEY.as_bytes()),
        id,
        app.browser.user,
        opened_at,
    );
    let fields = format!(
        "verdict=accepted_with_edits&lines_edited=3&opened_at={opened_at}&opened_sig={sig}"
    );
    let sent = post_form(&app, &format!("/jobs/{}/review", id.0), &fields).await;
    assert_eq!(sent.status, StatusCode::SEE_OTHER, "{}", sent.body);
    assert_eq!(sent.headers[header::LOCATION], format!("/jobs/{}", id.0));
    let record = s
        .record(&IssueId::parse("typst-9").unwrap())
        .await
        .unwrap()
        .unwrap();
    let review = record.review.unwrap();
    assert_eq!(
        (review.verdict, review.lines_edited),
        (ReviewVerdict::AcceptedWithEdits, 3)
    );
    assert!(
        (1.4..1.7).contains(&review.review_minutes),
        "{}",
        review.review_minutes
    );
    assert!(!review.broke_tests);

    let again = post_form(&app, &format!("/jobs/{}/review", id.0), &fields).await;
    assert_eq!(again.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(again.body.contains("đã có review"), "{}", again.body);
    let replace = format!(
        "verdict=rejected&lines_edited=0&broke_tests=on&replace=on&opened_at={opened_at}&opened_sig={sig}"
    );
    let replaced = post_form(&app, &format!("/jobs/{}/review", id.0), &replace).await;
    assert_eq!(replaced.status, StatusCode::SEE_OTHER);
    let record = s
        .record(&IssueId::parse("typst-9").unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        record.review.map(|r| (r.verdict, r.broke_tests)),
        Some((ReviewVerdict::Rejected, true))
    );

    let bad = post_form(
        &app,
        &format!("/jobs/{}/review", id.0),
        &format!("verdict=accepted&lines_edited=x&opened_at={opened_at}&opened_sig={sig}"),
    )
    .await;
    assert_eq!(bad.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(bad.body.contains("số dòng sửa"), "{}", bad.body);
    let no_pr = get(&app, &format!("/jobs/{}/review", stopped.0)).await;
    assert_eq!(no_pr.status, StatusCode::CONFLICT);
    db.drop().await;
}

#[tokio::test]
async fn pages_stay_on_known_hosts_and_refuse_cross_site_forms() {
    let Some(db) = test_db().await else { return };
    let typst = repo(&db.store, "typst", "rust").await;
    let id = job(&db.store, typst, "typst-5", Some(Outcome::PrOpened), &[]).await;
    let app = app(&db.store).await;

    let rebound = Request::get("/")
        .header(header::HOST, "evil.test")
        .body(Body::empty())
        .unwrap();
    assert_eq!(send(&app, rebound).await.status, StatusCode::FORBIDDEN);
    let cross = Request::post(format!("/jobs/{}/review", id.0))
        .header(header::HOST, "localhost:8080")
        .header("sec-fetch-site", "cross-site")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from("verdict=accepted&lines_edited=0&opened_at=0"))
        .unwrap();
    assert_eq!(send(&app, cross).await.status, StatusCode::FORBIDDEN);
    assert!(
        db.store
            .record(&IssueId::parse("typst-5").unwrap())
            .await
            .unwrap()
            .is_none_or(|r| r.review.is_none())
    );

    let script = get(&app, "/static/htmx-2.0.11.min.js").await;
    assert_eq!(script.status, StatusCode::OK);
    assert_eq!(script.headers[header::CONTENT_TYPE], "text/javascript");
    let table = Request::get("/jobs?repo_id=&status=done")
        .header(header::HOST, "127.0.0.1:8080")
        .header(header::COOKIE, &app.browser.cookie)
        .header("hx-request", "true")
        .body(Body::empty())
        .unwrap();
    let table = send(&app, table).await;
    assert_eq!(table.status, StatusCode::OK);
    assert!(
        !table.body.contains("<html") && table.body.contains("typst-5"),
        "{}",
        table.body
    );
    let bad = get(&app, "/jobs?status=nope").await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);
    // The API keeps its JSON envelope.
    assert_eq!(get(&app, "/api/v1/health").await.status, StatusCode::OK);
    db.drop().await;
}

/// #245: a PR's review rounds are listed on its job page with what the PR
/// cost in all, a round is not reviewed on its own, and the dashboard
/// counts a round's cost with its PR, not as a run.
#[tokio::test]
async fn a_prs_review_rounds_show_on_its_job_and_count_with_it() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let demo = repo(s, "demo", "go").await;
    let lease = std::time::Duration::from_secs(60);
    let opened = |pr: &str| aegis_store::AttemptEnd::Done {
        outcome: Outcome::PrOpened,
        reason: None,
        pr_url: Some(pr.into()),
    };
    let finished = LedgerEvent::Finished {
        outcome: Outcome::PrOpened,
        wall_time_s: 60.0,
    };
    let root = job(
        s,
        demo,
        "demo-1",
        None,
        &[call(1.0, Tier::Core), finished.clone()],
    )
    .await;
    let claim = s.claim_job("w", lease, 10).await.unwrap().unwrap();
    let pr = "https://github.com/bot/demo/pull/5";
    s.finish_attempt(&claim.run, "w", &opened(pr))
        .await
        .unwrap();
    s.ledger()
        .append(
            &IssueId::parse("demo-1").unwrap(),
            &reviewed(ReviewVerdict::Accepted, 0, 4.0),
        )
        .await
        .unwrap();
    let round = s
        .review_job_by(root, &aegis_store::Actor::cli())
        .await
        .unwrap();
    for event in [call(0.5, Tier::Core), finished] {
        s.ledger().append(&round.ledger_key, &event).await.unwrap();
    }
    let claim = s.claim_job("w", lease, 10).await.unwrap().unwrap();
    assert_eq!(claim.job.id, round.id);
    s.finish_attempt(&claim.run, "w", &opened(pr))
        .await
        .unwrap();
    let app = app(s).await;

    for id in [root, round.id] {
        let page = get(&app, &format!("/jobs/{}", id.0)).await;
        assert_eq!(page.status, StatusCode::OK);
        assert!(page.body.contains("Vòng sửa theo review"), "{}", page.body);
        assert!(
            page.body
                .contains(&format!("href=\"/jobs/{}\"", round.id.0)),
            "{}",
            page.body
        );
        assert!(
            page.body.contains("$1.50"),
            "the PR's cost in all: {}",
            page.body
        );
    }
    let round_page = get(&app, &format!("/jobs/{}", round.id.0)).await;
    assert!(
        round_page.body.contains(&format!("job #{}</a> mở", root.0)),
        "{}",
        round_page.body
    );
    assert!(!round_page.body.contains("Duyệt PR"), "{}", round_page.body);
    let review = get(&app, &format!("/jobs/{}/review", round.id.0)).await;
    assert_eq!(review.status, StatusCode::CONFLICT, "{}", review.body);

    let d = dashboard_kpis(s).await.unwrap();
    assert_eq!(
        (d.total.runs, d.total.prs_opened, d.total.accepted),
        (1, 1, 1)
    );
    assert_eq!(d.total.mean_cost_usd, Some(1.5));
    assert_eq!(d.total_review_rounds, Some(1.0));
    let home = get(&app, "/").await;
    assert!(home.body.contains("Vòng sửa / PR"), "{}", home.body);
    db.drop().await;
}
