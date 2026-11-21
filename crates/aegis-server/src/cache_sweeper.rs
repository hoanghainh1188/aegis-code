//! The worker's cache sweep (#593): every [`EVERY`], on each process that
//! runs jobs, it measures the repos' caches and the jobs' worktrees under
//! the modes' state dirs, sets [`crate::metrics`]' disk gauges, and removes
//! caches by the rules of [`aegis_lifecycle::cache_sweep`].
//!
//! A repo with a running job keeps its caches. The running jobs are read
//! from the queue again just before each removal, and the cache's markers
//! too, since a removal can take minutes; nothing is removed when the jobs
//! cannot be read. A job running on another worker keeps the repo's caches
//! on this one as well: the queue does not say which disk a job uses, and
//! keeping is the safe side.

use std::collections::{BTreeSet, HashSet};
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use aegis_lifecycle::cache_sweep::{self, RepoCache, SweepRules};
use aegis_store::{JobFilter, JobStatus, Page, Store, StoreError};
use tokio_util::sync::CancellationToken;

use crate::metrics::metrics;

/// How often a worker sweeps.
pub const EVERY: Duration = Duration::from_secs(15 * 60);

/// What one sweep saw and did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Swept {
    /// The caches' bytes after the sweep.
    pub cache_bytes: u64,
    pub worktree_bytes: u64,
    /// The slugs of the caches removed.
    pub removed: Vec<String>,
}

/// Sweeps at once, then every [`EVERY`] until `shutdown`. `rules`: `None`
/// only measures.
pub async fn run_sweeper(
    store: Store,
    state_dirs: Vec<PathBuf>,
    rules: Option<SweepRules>,
    shutdown: CancellationToken,
) {
    loop {
        sweep_once(&store, &state_dirs, rules, &shutdown).await;
        tokio::select! {
            () = shutdown.cancelled() => break,
            () = tokio::time::sleep(EVERY) => {}
        }
    }
}

/// One sweep. Errors are logged: a sweep that fails leaves the disk as it
/// was, and the next one tries again. `cancel` stops it between two
/// removals.
pub async fn sweep_once(
    store: &Store,
    state_dirs: &[PathBuf],
    rules: Option<SweepRules>,
    cancel: &CancellationToken,
) -> Swept {
    let dirs = state_dirs.to_vec();
    let disk = match tokio::task::spawn_blocking(move || look(&dirs)).await {
        Ok(disk) => disk,
        Err(e) => {
            tracing::error!(error = %e, "measuring the worker's caches");
            return Swept::default();
        }
    };
    let mut swept = Swept {
        cache_bytes: disk.caches.iter().map(|c| c.bytes).sum::<u64>() + disk.leftover_bytes,
        worktree_bytes: disk.worktree_bytes,
        removed: Vec::new(),
    };
    if let Some(rules) = rules {
        match busy_slugs(store).await {
            Ok(busy) => remove(store, &disk.caches, &busy, &rules, cancel, &mut swept).await,
            Err(e) => tracing::error!(error = %e, "reading the running jobs; no cache removed"),
        }
    }
    let limit = rules.map_or(0, |r| r.max_bytes);
    metrics().workspace_disk(swept.cache_bytes, swept.worktree_bytes, limit);
    if limit > 0 && swept.cache_bytes > limit {
        tracing::warn!(
            cache_bytes = swept.cache_bytes,
            limit_bytes = limit,
            "the caches stay above their limit: the rest are in use or used recently"
        );
    }
    swept
}

async fn remove(
    store: &Store,
    caches: &[RepoCache],
    busy: &HashSet<String>,
    rules: &SweepRules,
    cancel: &CancellationToken,
    swept: &mut Swept,
) {
    for eviction in cache_sweep::plan(caches, busy, rules, SystemTime::now()) {
        if cancel.is_cancelled() {
            return;
        }
        let cache = caches[eviction.index].clone();
        let (slug, bytes) = (cache.slug.clone(), cache.bytes);
        // The plan was made from what the sweep saw; a job may have been
        // claimed since, or the last removal may have taken minutes.
        match busy_slugs(store).await {
            Ok(now_busy) if now_busy.contains(&slug) => continue,
            Ok(_) => {}
            Err(e) => {
                tracing::error!(error = %e, "reading the running jobs; no more caches removed");
                return;
            }
        }
        let grace = rules.grace;
        let evict = move || {
            let recent = cache_sweep::last_marked(&cache.path).is_some_and(|at| {
                SystemTime::now()
                    .duration_since(at)
                    .map_or(true, |idle| idle < grace)
            });
            match recent {
                true => Ok(false),
                false => cache_sweep::evict(&cache).map(|()| true),
            }
        };
        match tokio::task::spawn_blocking(evict).await {
            Ok(Ok(false)) => {}
            Ok(Ok(true)) => {
                tracing::info!(
                    repo = %slug,
                    bytes,
                    reason = eviction.reason.as_str(),
                    "removed a repo's caches"
                );
                metrics().cache_evicted(eviction.reason);
                swept.cache_bytes = swept.cache_bytes.saturating_sub(bytes);
                swept.removed.push(slug);
            }
            Ok(Err(e)) => tracing::error!(repo = %slug, error = %e, "removing a repo's caches"),
            Err(e) => tracing::error!(repo = %slug, error = %e, "removing a repo's caches"),
        }
    }
}

struct Disk {
    caches: Vec<RepoCache>,
    /// Removed caches that could not be deleted: no repo's, still on disk.
    leftover_bytes: u64,
    worktree_bytes: u64,
}

/// Every cache and worktree under `state_dirs`, each dir once (two modes
/// may share one, by another path).
fn look(state_dirs: &[PathBuf]) -> Disk {
    let dirs: BTreeSet<PathBuf> = state_dirs
        .iter()
        .map(|d| d.canonicalize().unwrap_or_else(|_| d.clone()))
        .collect();
    let mut caches = Vec::new();
    let mut leftover_bytes = 0;
    let mut worktree_bytes = 0;
    for dir in &dirs {
        for root in cache_sweep::cache_roots(dir) {
            match cache_sweep::remove_leftovers(&root) {
                Ok(left) => {
                    leftover_bytes += left.bytes;
                    for (path, e) in left.failed {
                        tracing::warn!(path = %path.display(), error = %e, "deleting a removed cache");
                    }
                }
                Err(e) => tracing::warn!(path = %root.display(), error = %e, "reading caches"),
            }
            match cache_sweep::scan(&root) {
                Ok(found) => caches.extend(found),
                Err(e) => tracing::error!(path = %root.display(), error = %e, "reading caches"),
            }
        }
        worktree_bytes += cache_sweep::job_roots(dir)
            .iter()
            .map(|root| cache_sweep::tree_bytes(root))
            .sum::<u64>();
    }
    Disk {
        caches,
        leftover_bytes,
        worktree_bytes,
    }
}

/// The slugs of the repos with a running job, on any worker. A slug is
/// unique across tenants, and names the repo's cache directory.
async fn busy_slugs(store: &Store) -> Result<HashSet<String>, StoreError> {
    let filter = JobFilter {
        repo_id: None,
        status: Some(JobStatus::Running),
    };
    let mut repos = HashSet::new();
    let mut page = Some(Page::first(Page::MAX_LIMIT));
    while let Some(p) = page {
        let jobs = store.list_jobs(&filter, p).await?;
        repos.extend(jobs.items.iter().map(|j| j.repo_id));
        page = jobs.next_page();
    }
    let mut slugs = HashSet::new();
    for id in repos {
        if let Some(repo) = store.repo(id).await? {
            slugs.insert(repo.slug);
        }
    }
    Ok(slugs)
}
