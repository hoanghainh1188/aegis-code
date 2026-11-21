//! Keeps the build caches on a worker's disk within a size (#593).
//!
//! Each repo's caches live under `<state_dir>/cache/<slug>/` (and under
//! `<state_dir>/tenants/<id>/cache/<slug>/` for a tenant's repo, #525):
//! the build cache the gate seeds every tree from (`build/`, #104), the
//! dependency directories mounted into the sandbox, one set per part of
//! the repo (#71). Nothing removed them, and a Rust repo's `target/` alone
//! grows to 30–40 GB.
//!
//! A sweep looks at each repo's caches as one unit: its size on disk, and
//! when a job last used it, from the newest [`LAST_USED`] marker in it
//! (written by the gate's build cache and by each job's sandbox). It then
//! removes, never touching a repo that has a running job nor one used in
//! the last [`SweepRules::grace`]:
//!
//! 1. every cache not used for [`SweepRules::ttl`];
//! 2. when the caches together are above [`SweepRules::max_bytes`], the
//!    least recently used, until they are at most [`TARGET_PERCENT`] of it.
//!
//! A removed cache costs the repo's next job a cold build, nothing else:
//! the gate starts a tree without a cache, and the base copy writes a new
//! one. Removal renames the directory first, so a job never sees half of
//! one, and makes read-only directories writable before deleting them: Go
//! writes its module cache read-only.
//!
//! The running jobs are those in the queue. A CLI run (`aegis eval`,
//! `check-repo`) has no job row: a worker's `state_dir` must not be shared
//! with one, or the sweep may remove a cache under it.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

pub use aegis_gate::LAST_USED;

/// Caches shared by every repo of a state dir, never removed by a sweep:
/// the Sonar scanner's (`cache/sonar`). A repo whose slug is `sonar`
/// shares that directory, so its caches are never removed either.
pub const SHARED: &[&str] = &["sonar"];

/// Above the limit, a sweep removes caches until they are at most this
/// share of it, so the next jobs do not start another sweep at once.
pub const TARGET_PERCENT: u64 = 85;

/// The prefix of a cache being removed: renamed, then deleted.
const REMOVING: &str = ".removing-";

/// One repo's caches on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoCache {
    /// The directory's name: the repo's slug, or a [`SHARED`] cache.
    pub slug: String,
    pub path: PathBuf,
    /// Its size on disk.
    pub bytes: u64,
    /// The newest [`LAST_USED`] marker in it; for a cache from before the
    /// markers, the newest time of anything in it.
    pub last_used: SystemTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SweepRules {
    /// What every cache may take together; 0 for no limit.
    pub max_bytes: u64,
    /// A cache not used for this long is removed; `None` keeps them all.
    pub ttl: Option<Duration>,
    /// A cache used this recently is kept whatever its size: a margin for a
    /// job that marked it after the sweep looked (the caller reads the
    /// running jobs and the markers again just before each removal).
    pub grace: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// Not used for the TTL.
    Expired,
    /// Least recently used, while the caches were above the limit.
    OverLimit,
}

impl Reason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Expired => "expired",
            Self::OverLimit => "over_limit",
        }
    }
}

/// A cache a sweep removes: its index in the caches it was given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Eviction {
    pub index: usize,
    pub reason: Reason,
}

/// What a sweep of `caches` removes. `busy`: the slugs of the repos with a
/// running job; their caches, and [`SHARED`] ones, are never removed.
pub fn plan(
    caches: &[RepoCache],
    busy: &HashSet<String>,
    rules: &SweepRules,
    now: SystemTime,
) -> Vec<Eviction> {
    let idle = |c: &RepoCache| now.duration_since(c.last_used).unwrap_or(Duration::ZERO);
    let removable = |c: &RepoCache| {
        !busy.contains(&c.slug) && !SHARED.contains(&c.slug.as_str()) && idle(c) >= rules.grace
    };
    let mut total: u64 = caches.iter().map(|c| c.bytes).sum();
    let mut evictions: Vec<Eviction> = caches
        .iter()
        .enumerate()
        .filter(|(_, c)| removable(c) && rules.ttl.is_some_and(|ttl| idle(c) > ttl))
        .map(|(index, _)| Eviction {
            index,
            reason: Reason::Expired,
        })
        .collect();
    total -= evictions.iter().map(|e| caches[e.index].bytes).sum::<u64>();

    if rules.max_bytes == 0 || total <= rules.max_bytes {
        return evictions;
    }
    let target = rules.max_bytes / 100 * TARGET_PERCENT;
    let mut oldest_first: Vec<usize> = (0..caches.len())
        .filter(|&i| removable(&caches[i]) && !evictions.iter().any(|e| e.index == i))
        .collect();
    oldest_first.sort_by_key(|&i| caches[i].last_used);
    for index in oldest_first {
        if total <= target {
            break;
        }
        total -= caches[index].bytes;
        evictions.push(Eviction {
            index,
            reason: Reason::OverLimit,
        });
    }
    evictions
}

/// The cache directories of a mode's `state_dir`: its own repos', and
/// each tenant's. Only those that exist.
pub fn cache_roots(state_dir: &Path) -> Vec<PathBuf> {
    within_tenants(state_dir, "cache")
}

/// The directories of the jobs' worktrees, as [`cache_roots`].
pub fn job_roots(state_dir: &Path) -> Vec<PathBuf> {
    within_tenants(state_dir, "jobs")
}

fn within_tenants(state_dir: &Path, name: &str) -> Vec<PathBuf> {
    let tenants = std::fs::read_dir(state_dir.join("tenants"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path().join(name));
    std::iter::once(state_dir.join(name))
        .chain(tenants)
        .filter(|p| p.is_dir())
        .collect()
}

/// Each repo's caches under a cache root, with its size and last use.
/// Links are not followed. Caches being removed are left out, and so is an
/// entry gone or unreadable while scanning.
pub fn scan(root: &Path) -> std::io::Result<Vec<RepoCache>> {
    let mut caches = Vec::new();
    for entry in std::fs::read_dir(root)?.flatten() {
        let slug = entry.file_name().to_string_lossy().into_owned();
        if slug.starts_with('.') || !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let path = entry.path();
        let mut walk = Walk::default();
        walk.visit(&path);
        let Some(last_used) = walk.newest_marker.or(walk.newest_any) else {
            continue;
        };
        caches.push(RepoCache {
            slug,
            path,
            bytes: walk.bytes,
            last_used,
        });
    }
    caches.sort_by(|a, b| a.slug.cmp(&b.slug));
    Ok(caches)
}

/// When a job last marked the caches at `path` ([`LAST_USED`]), read
/// where jobs write the markers: the repo's caches and build cache, and
/// each part's. Cheap, unlike a [`scan`]: for a check just before
/// removing.
pub fn last_marked(path: &Path) -> Option<SystemTime> {
    let parts = std::fs::read_dir(path.join("parts"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path());
    std::iter::once(path.to_path_buf())
        .chain(parts)
        .flat_map(|dir| [dir.join(LAST_USED), dir.join("build").join(LAST_USED)])
        .filter_map(|marker| std::fs::symlink_metadata(marker).ok()?.modified().ok())
        .max()
}

/// Removes `cache`: renamed out of the repo's place first, so a job
/// starting now finds no cache rather than part of one.
pub fn evict(cache: &RepoCache) -> std::io::Result<()> {
    let Some(root) = cache.path.parent() else {
        return Ok(());
    };
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let removing = root.join(format!("{REMOVING}{}-{nanos}", cache.slug));
    std::fs::rename(&cache.path, &removing)?;
    remove_tree(&removing)
}

/// What [`remove_leftovers`] could not remove.
#[derive(Debug, Default)]
pub struct Leftovers {
    /// Their size on disk: still taken, though no repo's cache.
    pub bytes: u64,
    pub failed: Vec<(PathBuf, std::io::Error)>,
}

/// Removes what an interrupted or failed [`evict`] left under a cache
/// root, each one whatever became of the others.
pub fn remove_leftovers(root: &Path) -> std::io::Result<Leftovers> {
    let mut left = Leftovers::default();
    for entry in std::fs::read_dir(root)?.flatten() {
        if !entry.file_name().to_string_lossy().starts_with(REMOVING) {
            continue;
        }
        let path = entry.path();
        if let Err(e) = remove_tree(&path) {
            left.bytes += tree_bytes(&path);
            left.failed.push((path, e));
        }
    }
    Ok(left)
}

/// `remove_dir_all`, after giving the owner write access to every
/// directory in it: entries of a read-only directory (Go's module cache)
/// cannot be unlinked otherwise. Links are not followed.
fn remove_tree(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut stack = vec![path.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let Ok(meta) = std::fs::symlink_metadata(&dir) else {
                continue;
            };
            if !meta.is_dir() {
                continue;
            }
            let mode = meta.permissions().mode();
            if mode & 0o700 != 0o700 {
                // A directory left as it was fails the removal below,
                // which reports it.
                let _ =
                    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(mode | 0o700));
            }
            let entries = std::fs::read_dir(&dir).into_iter().flatten().flatten();
            stack.extend(entries.map(|e| e.path()));
        }
    }
    std::fs::remove_dir_all(path)
}

/// The size on disk of everything under `path`, links not followed.
pub fn tree_bytes(path: &Path) -> u64 {
    let mut walk = Walk::default();
    walk.visit(path);
    walk.bytes
}

#[derive(Default)]
struct Walk {
    bytes: u64,
    newest_marker: Option<SystemTime>,
    newest_any: Option<SystemTime>,
}

impl Walk {
    /// Iterative, so a deep `node_modules` cannot overflow the stack. A
    /// file gone while walking (a job's own) is skipped.
    fn visit(&mut self, start: &Path) {
        let mut stack = vec![start.to_path_buf()];
        while let Some(path) = stack.pop() {
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            self.bytes += on_disk(&meta);
            self.newest_any = self.newest_any.max(meta.modified().ok());
            if meta.is_dir() {
                let entries = std::fs::read_dir(&path).into_iter().flatten().flatten();
                stack.extend(entries.map(|e| e.path()));
            } else if meta.is_file()
                && path.file_name().is_some_and(|n| n == LAST_USED)
                && let Ok(time) = meta.modified()
            {
                self.newest_marker = self.newest_marker.max(Some(time));
            }
        }
    }
}

/// Blocks allocated, which is what fills a disk; a sparse file counts what
/// it holds. A reflinked clone (a tree started from the cache) counts in
/// full, though it shares blocks: the sweep errs toward removing early.
#[cfg(unix)]
fn on_disk(meta: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    meta.blocks() * 512
}

#[cfg(not(unix))]
fn on_disk(meta: &std::fs::Metadata) -> u64 {
    meta.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: Duration = Duration::from_secs(86_400);
    const GB: u64 = 1 << 30;

    fn now() -> SystemTime {
        SystemTime::UNIX_EPOCH + 1000 * DAY
    }

    fn cache(slug: &str, gb: u64, days_ago: u64) -> RepoCache {
        RepoCache {
            slug: slug.into(),
            path: PathBuf::from("/state/cache").join(slug),
            bytes: gb * GB,
            last_used: now() - DAY * days_ago as u32,
        }
    }

    fn rules(max_gb: u64) -> SweepRules {
        SweepRules {
            max_bytes: max_gb * GB,
            ttl: Some(14 * DAY),
            grace: Duration::from_secs(600),
        }
    }

    fn evicted(caches: &[RepoCache], busy: &[&str], rules: &SweepRules) -> Vec<(String, Reason)> {
        let busy = busy.iter().map(|s| s.to_string()).collect();
        plan(caches, &busy, rules, now())
            .into_iter()
            .map(|e| (caches[e.index].slug.clone(), e.reason))
            .collect()
    }

    #[test]
    fn above_the_limit_the_least_recently_used_go_until_below_the_target() {
        let caches = [
            cache("newest", 20, 1),
            cache("oldest", 20, 5),
            cache("middle", 20, 3),
        ];
        // 60 GB over a 50 GB limit: down to 42.5 GB, so the oldest goes.
        assert_eq!(
            evicted(&caches, &[], &rules(50)),
            [("oldest".into(), Reason::OverLimit)]
        );
        // Over 45 GB: down to 38.25 GB, so the two oldest go.
        assert_eq!(
            evicted(&caches, &[], &rules(45)),
            [
                ("oldest".into(), Reason::OverLimit),
                ("middle".into(), Reason::OverLimit),
            ]
        );
        // Under the limit nothing goes, though above the target.
        assert_eq!(evicted(&caches, &[], &rules(60)), []);
    }

    #[test]
    fn a_repo_with_a_running_job_keeps_its_cache_whatever_its_age() {
        let caches = [cache("running", 40, 30), cache("idle", 20, 2)];
        assert_eq!(
            evicted(&caches, &["running"], &rules(50)),
            [("idle".into(), Reason::OverLimit)]
        );
        // Even when nothing else can go, and even past the TTL.
        assert_eq!(evicted(&caches[..1], &["running"], &rules(10)), []);
    }

    #[test]
    fn a_cache_unused_for_the_ttl_goes_whatever_the_size() {
        let caches = [cache("stale", 1, 15), cache("fresh", 1, 13)];
        assert_eq!(
            evicted(&caches, &[], &rules(0)),
            [("stale".into(), Reason::Expired)]
        );
        let keep_all = SweepRules {
            ttl: None,
            ..rules(0)
        };
        assert_eq!(evicted(&caches, &[], &keep_all), []);
    }

    #[test]
    fn expired_caches_count_toward_getting_under_the_limit() {
        let caches = [cache("stale", 30, 20), cache("recent", 30, 1)];
        assert_eq!(
            evicted(&caches, &[], &rules(50)),
            [("stale".into(), Reason::Expired)]
        );
    }

    #[test]
    fn a_cache_used_within_the_grace_and_shared_caches_are_kept() {
        let mut just_used = cache("just-used", 40, 0);
        just_used.last_used = now() - Duration::from_secs(60);
        let caches = [just_used, cache("sonar", 40, 30)];
        assert_eq!(evicted(&caches, &[], &rules(10)), []);
    }

    fn temp_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("aegis-sweep-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        root
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn touch_at(path: &Path, time: SystemTime) {
        write(path, "");
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(time)
            .unwrap();
    }

    #[test]
    fn a_scan_sizes_each_repo_and_dates_it_by_its_newest_marker() {
        let state = temp_root("scan");
        let cache = state.join("cache");
        let old = SystemTime::UNIX_EPOCH + 100 * DAY;
        let newer = SystemTime::UNIX_EPOCH + 200 * DAY;
        write(
            &cache.join("typst/build/target/lib.rlib"),
            &"x".repeat(64 * 1024),
        );
        touch_at(&cache.join("typst/build").join(LAST_USED), old);
        touch_at(&cache.join("typst/parts/web").join(LAST_USED), newer);
        write(&cache.join("scrapy/root_.venv/x.py"), "pass");
        std::fs::create_dir_all(cache.join(".removing-gone-1")).unwrap();
        write(&state.join("tenants/7/cache/gin/build/x"), "go");
        write(&state.join("tenants/7/jobs/gin-12/main.go"), "go");

        let caches = scan(&cache).unwrap();
        let slugs: Vec<&str> = caches.iter().map(|c| c.slug.as_str()).collect();
        assert_eq!(slugs, ["scrapy", "typst"]);
        let typst = &caches[1];
        assert!(typst.bytes >= 64 * 1024, "{}", typst.bytes);
        assert_eq!(typst.last_used, newer);
        assert!(caches[0].last_used > newer, "no marker: its newest entry");
        assert_eq!(last_marked(&cache.join("typst")), Some(newer));
        assert_eq!(last_marked(&cache.join("scrapy")), None);

        assert_eq!(
            cache_roots(&state),
            [cache.clone(), state.join("tenants/7/cache")]
        );
        assert_eq!(job_roots(&state), [state.join("tenants/7/jobs")]);
        assert!(tree_bytes(&state.join("tenants/7/jobs")) > 0);
        let _ = std::fs::remove_dir_all(&state);
    }

    #[cfg(unix)]
    fn read_only(dir: &Path) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_read_only_cache_like_gos_module_cache_is_removed_too() {
        let state = temp_root("read-only");
        let cache = state.join("cache");
        let module = cache.join("gin/go_pkg_mod/github.com/x@v1.0.0");
        write(&module.join("x.go"), "package x");
        read_only(&module);
        read_only(module.parent().unwrap());
        let left = cache.join(".removing-gone-1/go_pkg_mod/m@v1");
        write(&left.join("m.go"), "package m");
        read_only(&left);

        let gin = scan(&cache).unwrap().remove(0);
        evict(&gin).unwrap();
        let leftovers = remove_leftovers(&cache).unwrap();
        assert!(leftovers.failed.is_empty(), "{leftovers:?}");
        assert_eq!(std::fs::read_dir(&cache).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(&state);
    }

    #[test]
    fn evicting_removes_the_whole_cache_and_leftovers_are_cleared() {
        let state = temp_root("evict");
        let cache = state.join("cache");
        write(&cache.join("typst/build/target/lib.rlib"), "built");
        write(&cache.join("scrapy/root_.venv/x.py"), "pass");
        std::fs::create_dir_all(cache.join(".removing-gin-1/build")).unwrap();

        let caches = scan(&cache).unwrap();
        let typst = caches.iter().find(|c| c.slug == "typst").unwrap();
        evict(typst).unwrap();
        remove_leftovers(&cache).unwrap();

        let left: Vec<String> = std::fs::read_dir(&cache)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(left, ["scrapy"]);
        let _ = std::fs::remove_dir_all(&state);
    }
}
