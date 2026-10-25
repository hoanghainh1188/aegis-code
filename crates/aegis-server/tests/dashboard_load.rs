//! How long `/`'s KPIs take on a large ledger (#170). Ignored: run by hand
//! with `AEGIS_TEST_DATABASE_URL` set and
//! `cargo test -p aegis-server --test dashboard_load -- --ignored --nocapture`.

use std::time::Instant;

use aegis_server::web::dashboard_kpis;
use aegis_store::testing::test_db;

/// `issues` finished jobs over 20 repos, each with 30 model calls, its
/// finished event and its saved record, as a worker leaves them.
fn seed(issues: u32) -> &'static str {
    let sql = format!(
        r#"
INSERT INTO repos (slug, url, default_branch, language, mode)
SELECT 'r' || g, 'https://github.com/x/r' || g, 'main',
       CASE WHEN g % 2 = 0 THEN 'rust' ELSE 'python' END, 'poc'
FROM generate_series(1, 20) g;
INSERT INTO jobs (repo_id, issue_ref, ledger_key, source, status, outcome, created_at)
SELECT (SELECT id FROM repos WHERE slug = 'r' || (g % 20 + 1)), 'x/y#' || g, 'JOB-' || g,
       'cli', 'done', 'pr_opened', now() - g * interval '1 hour'
FROM generate_series(1, {issues}) g;
INSERT INTO ledger_events (ledger_key, event)
SELECT 'JOB-' || g, '{{"event":"model_call","tier":"core","model":"core","tokens":{{"input":10,"output":5,"cache_write_5m":0,"cache_write_1h":0,"cache_read":90}},"cost_usd":0.125}}'::jsonb
FROM generate_series(1, {issues}) g, generate_series(1, 30) c;
INSERT INTO ledger_events (ledger_key, event)
SELECT 'JOB-' || g, '{{"event":"finished","outcome":"pr_opened","wall_time_s":100.0}}'::jsonb
FROM generate_series(1, {issues}) g;
INSERT INTO issue_records (ledger_key, record)
SELECT 'JOB-' || g, jsonb_build_object(
  'issue_id', 'JOB-' || g, 'kind', NULL, 'outcome', 'pr_opened', 'tiers_used', '["core"]'::jsonb,
  'model_calls', 30,
  'tokens', '{{"core":{{"input":300,"output":150,"cache_write_5m":0,"cache_write_1h":0,"cache_read":2700}}}}'::jsonb,
  'cost_usd', 3.75, 'gate_runs', '[]'::jsonb, 'wall_time_s', 100.0, 'review', NULL,
  'secrets_redacted', 0, 'hidden_tests_passed', NULL)
FROM generate_series(1, {issues}) g;
ANALYZE;
"#
    );
    Box::leak(sql.into_boxed_str())
}

#[tokio::test]
#[ignore = "seeds 10,000 issues; run by hand"]
async fn the_dashboard_loads_fast_on_a_large_ledger() {
    for issues in [1_000, 10_000] {
        let Some(db) = test_db().await else { return };
        db.raw(seed(issues)).await.unwrap();
        // The first read after a bulk insert also sets the rows' hint bits,
        // which a worker's inserts, read one page load at a time, never pay
        // in one go.
        let cold = Instant::now();
        dashboard_kpis(&db.store).await.unwrap();
        eprintln!(
            "{issues} issues, first load after seeding: {:?}",
            cold.elapsed()
        );
        let started = Instant::now();
        let d = dashboard_kpis(&db.store).await.unwrap();
        let took = started.elapsed();
        eprintln!("{issues} issues: {took:?}");
        assert_eq!(d.total.runs, issues as usize);
        db.drop().await;
        assert!(took.as_secs_f64() < 1.0, "{issues} issues took {took:?}");
    }
}
