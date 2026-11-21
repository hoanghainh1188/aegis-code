//! The worker's cache sweep (#593) against a real queue: a repo with a
//! running job keeps its caches, the others go oldest first. Runs with
//! `AEGIS_TEST_DATABASE_URL` set, skipped otherwise.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use aegis_ledger::IssueId;
use aegis_lifecycle::cache_sweep::{LAST_USED, SweepRules};
use aegis_server::cache_sweeper::sweep_once;
use aegis_store::testing::test_db;
use aegis_store::{JobSource, ModeName, NewJob, NewRepo, RepoId, Store};

const DAY: Duration = Duration::from_secs(86_400);

async fn repo(s: &Store, slug: &str) -> RepoId {
    s.create_repo(&NewRepo {
        slug: slug.into(),
        url: format!("https://github.com/example/{slug}"),
        default_branch: "main".into(),
        language: "rust".into(),
        mode: ModeName::POC,
        max_concurrent_jobs: 1,
        config: serde_json::json!({}),
    })
    .await
    .unwrap()
    .id
}

async fn job(s: &Store, repo: RepoId, issue_ref: &str) {
    s.create_job(&NewJob {
        repo_id: repo,
        issue_ref: issue_ref.into(),
        ledger_key: IssueId::parse(issue_ref.replace(['/', '#'], "-")).unwrap(),
        source: JobSource::Github,
        priority: None,
        sla_deadline: None,
    })
    .await
    .unwrap();
}

/// A repo's cache of `bytes`, last used `days_ago`.
fn cache(state: &Path, slug: &str, bytes: usize, days_ago: u32) {
    let build = state.join("cache").join(slug).join("build");
    std::fs::create_dir_all(build.join("target")).unwrap();
    std::fs::write(build.join("target/lib.rlib"), vec![1u8; bytes]).unwrap();
    let marker = build.join(LAST_USED);
    std::fs::write(&marker, b"").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&marker)
        .unwrap()
        .set_modified(SystemTime::now() - DAY * days_ago)
        .unwrap();
}

fn state_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("aegis-cache-sweeper-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

#[tokio::test]
async fn a_repo_with_a_running_job_keeps_its_cache_and_the_oldest_other_goes() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let state = state_dir();
    let mib = 1 << 20;
    // The busy repo's cache is the oldest and largest: by age alone it
    // would go first.
    cache(&state, "busy", 4 * mib, 5);
    cache(&state, "old", 2 * mib, 3);
    cache(&state, "recent", 2 * mib, 1);
    std::fs::create_dir_all(state.join("jobs/recent-7")).unwrap();
    std::fs::write(state.join("jobs/recent-7/main.rs"), vec![0u8; mib]).unwrap();

    let busy = repo(s, "busy").await;
    job(s, busy, "example/busy#1").await;
    let claim = s
        .claim_job("w1", Duration::from_secs(60), 4)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claim.job.repo_id, busy);

    let cancel = tokio_util::sync::CancellationToken::new();
    // 8 MiB over a 7.5 MiB limit: down to 85% of it (6.4 MiB), so one
    // 2 MiB cache is enough.
    let rules = SweepRules {
        max_bytes: 15 * mib as u64 / 2,
        ttl: Some(14 * DAY),
        grace: Duration::from_secs(600),
    };
    let swept = sweep_once(s, std::slice::from_ref(&state), Some(rules), &cancel).await;
    assert_eq!(swept.removed, ["old"]);
    assert!(state.join("cache/busy/build/target/lib.rlib").is_file());
    assert!(state.join("cache/recent/build/target/lib.rlib").is_file());
    assert!(!state.join("cache/old").exists());
    assert!(swept.cache_bytes <= rules.max_bytes, "{swept:?}");
    assert!(swept.worktree_bytes >= mib as u64, "{swept:?}");

    // Only measuring: nothing more goes, whatever the size.
    let measured = sweep_once(s, std::slice::from_ref(&state), None, &cancel).await;
    assert!(measured.removed.is_empty());
    assert_eq!(measured.cache_bytes, swept.cache_bytes);

    let _ = std::fs::remove_dir_all(&state);
    db.drop().await;
}
