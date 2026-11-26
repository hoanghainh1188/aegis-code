//! #700: the pilot's report of a repo over a period, from sample ledger
//! data. `aegis report --repo <id> --since <date>` prints it from the API,
//! with every line, and the dashboard's page shows the same figures.

mod common;

use std::time::Duration;

use aegis_ledger::{
    IssueId, LedgerEvent, LedgerStore, Outcome, Review, ReviewVerdict, Tier, TokenCounts, Triage,
    TriageCategory,
};
use aegis_server::api::router;
use aegis_store::testing::test_db;
use aegis_store::{Actor, AttemptEnd, JobSource, ModeName, NewJob, NewRepo, RepoId, Store};
use axum::body::Body;
use axum::http::{Request, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

fn new_repo(slug: &str, shadow: bool) -> NewRepo {
    NewRepo {
        slug: slug.into(),
        url: format!("https://github.com/acme/{slug}"),
        default_branch: "main".into(),
        language: "rust".into(),
        mode: ModeName::POC,
        max_concurrent_jobs: 4,
        config: json!({ "shadow": shadow }),
    }
}

struct Sample {
    key: &'static str,
    outcome: Outcome,
    cost_usd: f64,
    triage: Option<TriageCategory>,
    review: Option<(ReviewVerdict, f64)>,
}

/// A job of `repo` that ran and ended as `sample` says, with its ledger.
async fn job(s: &Store, repo: RepoId, sample: Sample) {
    s.create_job(&NewJob {
        repo_id: repo,
        issue_ref: format!("acme/x#{}", sample.key),
        ledger_key: IssueId::parse(sample.key).unwrap(),
        source: JobSource::Github,
        priority: None,
        sla_deadline: None,
    })
    .await
    .unwrap();
    let claim = s
        .claim_job("w", Duration::from_secs(60), 10)
        .await
        .unwrap()
        .unwrap();
    let key = claim.job.ledger_key.clone();
    let ledger = s.ledger();
    let mut events = vec![LedgerEvent::ModelCall {
        tier: Tier::Core,
        model: "core".into(),
        tokens: TokenCounts::default(),
        cost_usd: sample.cost_usd,
    }];
    if let Some(category) = sample.triage {
        events.push(LedgerEvent::Triaged {
            triage: Triage {
                category,
                confidence: 0.8,
                reason: "r".into(),
            },
        });
    }
    events.push(LedgerEvent::Finished {
        outcome: sample.outcome,
        wall_time_s: 60.0,
    });
    if let Some((verdict, minutes)) = sample.review {
        events.push(LedgerEvent::Reviewed(Review {
            verdict,
            lines_edited: 0,
            review_minutes: minutes,
            broke_tests: false,
        }));
    }
    for e in &events {
        ledger.append(&key, e).await.unwrap();
    }
    let end = AttemptEnd::Done {
        outcome: sample.outcome,
        reason: None,
        pr_url: (sample.outcome == Outcome::PrOpened)
            .then(|| "https://github.com/acme/x/pull/1".into()),
        patch: None,
        diagnosis: None,
    };
    s.finish_attempt(&claim.run, "w", &end).await.unwrap();
}

/// The `shop` repo's sample: this week two jobs that write and one shadow
/// job, the week before one job; another repo's job besides.
async fn seed(db: &aegis_store::testing::TestDb) -> RepoId {
    let s = &db.store;
    let shop = s.create_repo(&new_repo("shop", false)).await.unwrap().id;
    let other = s.create_repo(&new_repo("other", false)).await.unwrap().id;
    let sample = |key, outcome, cost_usd, triage, review| Sample {
        key,
        outcome,
        cost_usd,
        triage,
        review,
    };
    job(
        s,
        shop,
        sample(
            "SHOP-1",
            Outcome::PrOpened,
            1.0,
            Some(TriageCategory::CodeBug),
            Some((ReviewVerdict::Accepted, 4.0)),
        ),
    )
    .await;
    job(
        s,
        shop,
        sample(
            "SHOP-2",
            Outcome::StoppedBudget,
            3.0,
            Some(TriageCategory::OpsOrConfig),
            None,
        ),
    )
    .await;
    job(
        s,
        shop,
        sample(
            "SHOP-OLD",
            Outcome::PrOpened,
            2.0,
            None,
            Some((ReviewVerdict::Rejected, 20.0)),
        ),
    )
    .await;
    job(
        s,
        other,
        sample(
            "OTHER-1",
            Outcome::PrOpened,
            9.0,
            None,
            Some((ReviewVerdict::Accepted, 1.0)),
        ),
    )
    .await;
    // A shadow job of the same repo: in shadow mode while it is queued.
    let actor = Actor::system();
    s.update_repo_by(&new_repo("shop", true), &actor)
        .await
        .unwrap();
    job(
        s,
        shop,
        sample(
            "SHOP-3",
            Outcome::Shadowed,
            0.5,
            None,
            Some((ReviewVerdict::Accepted, 6.0)),
        ),
    )
    .await;
    s.update_repo_by(&new_repo("shop", false), &actor)
        .await
        .unwrap();
    // When each came in: the old one the week before.
    db.raw("UPDATE jobs SET created_at = now() - interval '9 days' WHERE ledger_key = 'SHOP-OLD'")
        .await
        .unwrap();
    db.raw("UPDATE jobs SET created_at = now() - interval '2 hours' WHERE ledger_key = 'SHOP-1'")
        .await
        .unwrap();
    db.raw("UPDATE jobs SET created_at = now() - interval '4 hours' WHERE ledger_key = 'SHOP-2'")
        .await
        .unwrap();
    shop
}

fn week_ago() -> String {
    (time::OffsetDateTime::now_utc().date() - time::Duration::days(6)).to_string()
}

/// `aegis <args>` against `server`, logged in with `token`.
async fn aegis(server: String, token: String, args: Vec<String>) -> String {
    tokio::task::spawn_blocking(move || {
        let client =
            aegis_client::api::Client::new(aegis_client::config::Login { server, token }).unwrap();
        let mut out = Vec::new();
        let (command, rest) = args.split_first().unwrap();
        aegis_client::cli::run_with(&client, command, rest.to_vec(), false, &mut out).unwrap();
        String::from_utf8(out).unwrap()
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn the_cli_prints_the_report_the_dashboard_shows() {
    let Some(db) = test_db().await else { return };
    let shop = seed(&db).await;
    let s = &db.store;
    common::admin(s, "admin@x.test").await;
    let browser = common::sign_in(s, "admin@x.test").await;
    common::admin(s, "pilot-cli").await;
    let token = common::token(s, "pilot-cli").await;
    let app = router(s.clone(), common::auth(s));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(axum::serve(listener, app.clone()).into_future());

    let since = week_ago();
    let args = |extra: &[&str]| {
        let mut a: Vec<String> = ["report", "--repo", &shop.0.to_string(), "--since", &since]
            .iter()
            .map(|s| s.to_string())
            .collect();
        a.extend(extra.iter().map(|s| s.to_string()));
        a
    };
    let markdown = aegis(server.clone(), token.clone(), args(&[])).await;
    for line in [
        "# Pilot report: shop",
        "| Tickets | 3 | 1 | tracked | — |",
        "| Jobs | 3 | 1 | tracked | — |",
        "| Triaged code_bug | 33% (1/3) | 0% (0/1) | tracked | — |",
        "| Triaged ops_or_config | 33% (1/3) | 0% (0/1) | tracked | — |",
        "| Triaged needs_info | 0% (0/3) | 0% (0/1) | tracked | — |",
        "| Accepted PRs (≤ 10 lines edited) | 50% (1/2) | 0% (0/1) | ≥ 40% | ✔ |",
        "| PRs opened | 1 | 1 | tracked | — |",
        "| Stopped: budget | 50% (1/2) | 0% (0/1) | tracked | — |",
        "| Stopped: steps | 0% (0/2) | 0% (0/1) | tracked | — |",
        "| Cost per job | $2.00 | $2.00 | ≤ $4 | ✔ |",
        "| Cost of the period | $4.50 | $2.00 | tracked | — |",
        "| Review time per PR (median) | 4.0 min | 20.0 min | ≤ 15 min | ✔ |",
        "| MTTR | 3.0 h |",
        "| Shadow jobs | 1 | 0 | tracked | — |",
        "| Shadow jobs a reviewer would merge | 100% (1/1) | — | tracked | — |",
        "Monthly budget: $",
    ] {
        assert!(markdown.contains(line), "no `{line}` in\n{markdown}");
    }
    // The other repo is not in it.
    assert!(
        !markdown.contains("$9.00") && !markdown.contains("$10"),
        "{markdown}"
    );
    let csv = aegis(server.clone(), token.clone(), args(&["--csv"])).await;
    assert!(
        csv.contains("Accepted PRs (≤ 10 lines edited),50% (1/2),0% (0/1),≥ 40%,yes\n"),
        "{csv}"
    );

    // The dashboard's page: the same figures, line by line.
    let request = Request::get(
        "/api/v1/reports/pilot?since=".to_owned() + &since + "&repo_id=" + &shop.0.to_string(),
    )
    .header("authorization", format!("Bearer {token}"))
    .body(Body::empty())
    .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let api: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(api["data"]["markdown"].as_str().unwrap(), markdown);
    let request = Request::get(format!("/reports/pilot?repo_id={}&since={since}", shop.0))
        .header(header::HOST, "localhost:8080")
        .header(header::COOKIE, &browser.cookie)
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let page = String::from_utf8(bytes.to_vec()).unwrap();
    let metrics = api["data"]["report"]["metrics"].as_array().unwrap();
    assert!(metrics.len() > 15);
    for m in metrics {
        let row = format!(
            "<th scope=\"row\">{}</th>\n        <td class=\"num\">{}</td>\n        <td class=\"num\">{}</td>",
            html_escape(m["name"].as_str().unwrap()),
            html_escape(m["current"].as_str().unwrap()),
            html_escape(m["previous"].as_str().unwrap()),
        );
        assert!(page.contains(&row), "no `{row}` in\n{page}");
    }
    db.drop().await;
}

/// As askama escapes text.
fn html_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

#[tokio::test]
async fn a_report_is_refused_without_a_start_or_for_a_repo_not_seen() {
    let Some(db) = test_db().await else { return };
    let shop = seed(&db).await;
    let s = &db.store;
    let theirs = s.create_repo(&new_repo("theirs", false)).await.unwrap().id;
    common::grant(s, "lan@x.test", shop, aegis_store::RepoRole::Viewer).await;
    let token = common::token(s, "admin-cli").await;
    let app = router(s.clone(), common::auth(s));
    let get = |uri: String, token: String| {
        let app = app.clone();
        async move {
            let request = Request::get(uri)
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap();
            let response = app.oneshot(request).await.unwrap();
            response.status().as_u16()
        }
    };
    assert_eq!(
        get("/api/v1/reports/pilot".into(), token.clone()).await,
        422
    );
    assert_eq!(
        get(
            "/api/v1/reports/pilot?since=2026-13-01".into(),
            token.clone()
        )
        .await,
        422
    );
    // A viewer of `shop` gets its report, not another repo's.
    let browser = common::sign_in(s, "lan@x.test").await;
    let page = |uri: String| {
        let app = app.clone();
        let cookie = browser.cookie.clone();
        async move {
            let request = Request::get(uri)
                .header(header::HOST, "localhost:8080")
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .unwrap();
            app.oneshot(request).await.unwrap().status().as_u16()
        }
    };
    let since = week_ago();
    assert_eq!(
        page(format!("/reports/pilot?repo_id={}&since={since}", shop.0)).await,
        200
    );
    assert_eq!(
        page(format!("/reports/pilot?repo_id={}&since={since}", theirs.0)).await,
        422
    );
    assert_eq!(
        page(format!(
            "/reports/pilot.csv?repo_id={}&since={since}",
            shop.0
        ))
        .await,
        200
    );
    db.drop().await;
}
