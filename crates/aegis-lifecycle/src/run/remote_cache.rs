//! The repo's caches in an object store (#665): one archive per repo part
//! of what `state_dir/cache/<repo>/` keeps for it (the build cache, the
//! mounted caches outside the tree, the fetched dependencies).
//!
//! A worker or execution host that starts without them (a fresh pod)
//! pulls the archive before the job; a worker whose gate refreshed them
//! pushes it after. The worker moves the archives, never a container, so
//! the gate's steps keep `--network none`.
//!
//! What goes up is upstream's only, as for the local cache (#104): the
//! base copy writes the build cache, the fetch writes the base's
//! dependencies, and the containers that run the patch see both read-only.
//! Both directions are best effort: a store that cannot be reached costs
//! the job a cold build, never its outcome.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

use aegis_gate::{LAST_USED, REFRESHED};
use aegis_lang::{ProfilePart, RepoProfiles};
use aws_sdk_s3::Client;
use aws_sdk_s3::config::{
    BehaviorVersion, Region, RequestChecksumCalculation, ResponseChecksumValidation,
};
use aws_sdk_s3::primitives::ByteStream;
use tokio::io::AsyncWriteExt;

use super::gating::part_cache;
use crate::{RemoteCache, RunConfig};

/// A file in a part's cache directory whose time is when it was last the
/// same as the store's archive: pushed from here, or pulled.
pub(crate) const IN_SYNC: &str = ".aegis-remote-in-sync";

/// The other parts' caches, inside the root part's directory.
const PARTS: &str = "parts";

/// One `PutObject` takes at most 5 GiB.
const MAX_ARCHIVE: u64 = 5 << 30;

/// What [`RemoteStore::pull`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Pulled {
    /// The archive is now the part's cache.
    Fetched,
    /// The store has no archive for the part yet.
    Missing,
    /// The part has a cache here already, which a job keeps.
    LocalKept,
}

/// What [`RemoteStore::push`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Pushed {
    Sent,
    /// Nothing upstream changed the cache since it was in sync.
    Unchanged,
    /// It changed, but went up less than `push_interval_s` ago.
    TooSoon,
}

pub(crate) struct RemoteStore {
    client: Client,
    bucket: String,
    prefix: String,
    push_interval: Duration,
}

impl RemoteStore {
    /// The store of the run's `[run.remote_cache]`, with credentials from
    /// the AWS chain.
    pub(crate) async fn connect(cache: &RemoteCache) -> Result<Self, String> {
        let shared = aws_config::defaults(BehaviorVersion::latest())
            .region(Region::new(cache.region.clone()))
            .load()
            .await;
        let mut config = aws_sdk_s3::config::Builder::from(&shared)
            // MinIO and other S3 APIs take plain bodies; AWS does too.
            .request_checksum_calculation(RequestChecksumCalculation::WhenRequired)
            .response_checksum_validation(ResponseChecksumValidation::WhenRequired);
        if let Some(endpoint) = &cache.endpoint {
            config = config.endpoint_url(endpoint).force_path_style(true);
        }
        Self::with_client(Client::from_conf(config.build()), cache)
    }

    pub(crate) fn with_client(client: Client, cache: &RemoteCache) -> Result<Self, String> {
        let (bucket, prefix) = cache.bucket_and_prefix()?;
        Ok(Self {
            client,
            bucket: bucket.to_owned(),
            prefix: prefix.to_owned(),
            push_interval: Duration::from_secs(cache.push_interval_s),
        })
    }

    /// The archive's key: by repo, part and language, so a part whose
    /// language changes does not start from another toolchain's cache.
    pub(crate) fn key(&self, repo: &str, part: &ProfilePart) -> String {
        let part_name = match part.dir.is_empty() {
            true => "root".to_owned(),
            false => format!("part-{}", part.dir.replace('/', "_")),
        };
        let name = format!(
            "{repo}/{part_name}-{}.tar.gz",
            part.profile.profile.language.id()
        );
        match self.prefix.is_empty() {
            true => name,
            false => format!("{}/{name}", self.prefix),
        }
    }

    /// Makes the archive at `key` the cache in `dir`, when `dir` has none
    /// of its own. `scratch` holds the download.
    pub(crate) async fn pull(
        &self,
        key: &str,
        dir: &Path,
        scratch: &Path,
    ) -> Result<Pulled, String> {
        if !is_empty(dir) {
            return Ok(Pulled::LocalKept);
        }
        let object = match self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
        {
            Ok(object) => object,
            Err(e) if e.as_service_error().is_some_and(|e| e.is_no_such_key()) => {
                return Ok(Pulled::Missing);
            }
            Err(e) => {
                return Err(format!(
                    "getting {key}: {}",
                    aws_sdk_s3::error::DisplayErrorContext(e)
                ));
            }
        };
        let archive = scratch_file(scratch, key)?;
        let mut file = tokio::fs::File::create(&archive)
            .await
            .map_err(|e| format!("{}: {e}", archive.display()))?;
        let mut body = object.body.into_async_read();
        tokio::io::copy(&mut body, &mut file)
            .await
            .map_err(|e| format!("downloading {key}: {e}"))?;
        file.flush()
            .await
            .map_err(|e| format!("{}: {e}", archive.display()))?;
        drop(file);
        let (dir, from) = (dir.to_owned(), archive.clone());
        let extracted = blocking(move || extract(&from, &dir)).await;
        let _ = std::fs::remove_file(&archive);
        extracted?;
        Ok(Pulled::Fetched)
    }

    /// Sends `dir` to `key`, when upstream refreshed it since it was last
    /// in sync and the last push is `push_interval` old.
    pub(crate) async fn push(
        &self,
        key: &str,
        dir: &Path,
        scratch: &Path,
    ) -> Result<Pushed, String> {
        let refreshed = [dir.join(REFRESHED), dir.join("build").join(REFRESHED)]
            .iter()
            .filter_map(|m| modified(m))
            .max();
        let in_sync = modified(&dir.join(IN_SYNC));
        match (refreshed, in_sync) {
            (None, _) => return Ok(Pushed::Unchanged),
            (Some(r), Some(s)) if r <= s => return Ok(Pushed::Unchanged),
            (_, Some(s)) if s.elapsed().is_ok_and(|age| age < self.push_interval) => {
                return Ok(Pushed::TooSoon);
            }
            _ => {}
        }
        let archive = scratch_file(scratch, key)?;
        let (from, to) = (dir.to_owned(), archive.clone());
        let packed = blocking(move || pack(&from, &to)).await;
        let sent = match packed {
            Ok(size) if size > MAX_ARCHIVE => {
                Err(format!("{key} is {size} bytes, more than one upload takes"))
            }
            Ok(_) => self.put(key, &archive).await,
            Err(e) => Err(e),
        };
        let _ = std::fs::remove_file(&archive);
        sent?;
        std::fs::write(dir.join(IN_SYNC), b"").map_err(|e| format!("{}: {e}", dir.display()))?;
        Ok(Pushed::Sent)
    }

    async fn put(&self, key: &str, archive: &Path) -> Result<(), String> {
        let body = ByteStream::from_path(archive)
            .await
            .map_err(|e| format!("{}: {e}", archive.display()))?;
        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(key)
            .content_type("application/gzip")
            .body(body)
            .send()
            .await
            .map_err(|e| {
                format!(
                    "putting {key}: {}",
                    aws_sdk_s3::error::DisplayErrorContext(e)
                )
            })?;
        Ok(())
    }

    /// Before the job: every covered part without a cache here starts
    /// from the store's.
    pub(crate) async fn pull_parts(
        &self,
        cfg: &RunConfig,
        profiles: &RepoProfiles,
        scratch: &Path,
    ) {
        for part in covered(cfg, profiles) {
            let (key, dir) = (self.key(&cfg.repo.key, part), part_cache(cfg, part));
            let started = Instant::now();
            match self.pull(&key, &dir, scratch).await {
                Ok(Pulled::Fetched) => tracing::info!(
                    %key,
                    seconds = started.elapsed().as_secs_f64(),
                    "the part's caches came from the remote cache"
                ),
                Ok(_) => {}
                Err(e) => tracing::warn!(%key, error = %e, "pulling the remote cache"),
            }
        }
    }

    /// After the job: every covered part whose caches upstream refreshed.
    pub(crate) async fn push_parts(
        &self,
        cfg: &RunConfig,
        profiles: &RepoProfiles,
        scratch: &Path,
    ) {
        for part in covered(cfg, profiles) {
            let (key, dir) = (self.key(&cfg.repo.key, part), part_cache(cfg, part));
            let started = Instant::now();
            match self.push(&key, &dir, scratch).await {
                Ok(Pushed::Sent) => tracing::info!(
                    %key,
                    seconds = started.elapsed().as_secs_f64(),
                    "the part's caches went to the remote cache"
                ),
                Ok(_) => {}
                Err(e) => tracing::warn!(%key, error = %e, "pushing the remote cache"),
            }
        }
    }
}

/// The run's store, if it has one; `None` with a warning when its client
/// cannot be made, so the job goes on with its local caches.
pub(crate) async fn for_run(cfg: &RunConfig) -> Option<RemoteStore> {
    let cache = cfg.run.remote_cache.as_ref()?;
    match RemoteStore::connect(cache).await {
        Ok(store) => Some(store),
        Err(e) => {
            tracing::warn!(error = %e, "the remote cache is not usable; the job uses its local caches");
            None
        }
    }
}

fn covered<'a>(
    cfg: &'a RunConfig,
    profiles: &'a RepoProfiles,
) -> impl Iterator<Item = &'a ProfilePart> {
    let cache = cfg.run.remote_cache.as_ref();
    profiles
        .parts()
        .iter()
        .filter(move |p| cache.is_some_and(|c| c.covers(p.profile.profile.language)))
}

/// A cache directory with nothing of its own: missing, or only markers and
/// (for the root part) the other parts' directories.
fn is_empty(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return true;
    };
    entries.flatten().all(|e| {
        let name = e.file_name();
        [PARTS, LAST_USED, REFRESHED, IN_SYNC]
            .iter()
            .any(|skip| name == *skip)
    })
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

fn scratch_file(scratch: &Path, key: &str) -> Result<PathBuf, String> {
    std::fs::create_dir_all(scratch).map_err(|e| format!("{}: {e}", scratch.display()))?;
    Ok(scratch.join(format!("remote-cache-{}", key.replace('/', "_"))))
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| format!("the archive did not finish: {e}"))?
}

fn tar(args: &[&std::ffi::OsStr]) -> Result<(), String> {
    let out = Command::new("tar")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("tar: {e}"))?;
    match out.status.success() {
        true => Ok(()),
        false => Err(format!(
            "tar: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )),
    }
}

/// `dir` as a gzip'd tar at `archive`, without the other parts' caches and
/// the sync marker; its size.
fn pack(dir: &Path, archive: &Path) -> Result<u64, String> {
    let (parts, sync) = (
        format!("--exclude=./{PARTS}"),
        format!("--exclude=./{IN_SYNC}"),
    );
    tar(&[
        "-czf".as_ref(),
        archive.as_os_str(),
        parts.as_ref(),
        sync.as_ref(),
        "-C".as_ref(),
        dir.as_os_str(),
        ".".as_ref(),
    ])?;
    std::fs::metadata(archive)
        .map(|m| m.len())
        .map_err(|e| format!("{}: {e}", archive.display()))
}

/// `archive` into `dir`, then `dir` in sync. A failed extraction leaves
/// `dir` with nothing of its own, as before.
fn extract(archive: &Path, dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let done = tar(&[
        "-xzf".as_ref(),
        archive.as_os_str(),
        "-C".as_ref(),
        dir.as_os_str(),
    ]);
    if let Err(e) = done {
        clear(dir);
        return Err(e);
    }
    std::fs::write(dir.join(IN_SYNC), b"").map_err(|e| format!("{}: {e}", dir.display()))
}

fn clear(dir: &Path) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        if entry.file_name() == PARTS {
            continue;
        }
        let path = entry.path();
        let _ = match path.is_dir() && !path.is_symlink() {
            true => std::fs::remove_dir_all(&path),
            false => std::fs::remove_file(&path),
        };
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use aws_sdk_s3::config::Credentials;
    use wiremock::matchers::any;
    use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

    use super::*;

    /// An S3 bucket in memory, path style: `PUT` and `GET /<bucket>/<key>`.
    #[derive(Clone, Default)]
    struct Bucket(Arc<Mutex<HashMap<String, Vec<u8>>>>);

    impl Respond for Bucket {
        fn respond(&self, request: &Request) -> ResponseTemplate {
            let path = request.url.path().to_owned();
            let mut objects = self.0.lock().unwrap();
            match request.method.as_str() {
                "PUT" => {
                    objects.insert(path, request.body.clone());
                    ResponseTemplate::new(200).insert_header("ETag", "\"1\"")
                }
                "GET" => match objects.get(&path) {
                    Some(body) => ResponseTemplate::new(200).set_body_bytes(body.clone()),
                    None => ResponseTemplate::new(404)
                        .insert_header("Content-Type", "application/xml")
                        .set_body_string(
                            "<?xml version=\"1.0\"?><Error><Code>NoSuchKey</Code>\
                             <Message>no</Message></Error>",
                        ),
                },
                _ => ResponseTemplate::new(405),
            }
        }
    }

    async fn store(server: &MockServer, interval: u64) -> RemoteStore {
        store_at(&server.uri(), interval)
    }

    fn store_at(uri: &str, interval: u64) -> RemoteStore {
        let config = aws_sdk_s3::Config::builder()
            .behavior_version(BehaviorVersion::latest())
            .region(Region::new("us-east-1"))
            .endpoint_url(uri)
            .force_path_style(true)
            .credentials_provider(Credentials::new("test", "test", None, None, "test"))
            .request_checksum_calculation(RequestChecksumCalculation::WhenRequired)
            .response_checksum_validation(ResponseChecksumValidation::WhenRequired)
            .build();
        let cache = RemoteCache {
            url: "s3://aegis-cache/builds".into(),
            endpoint: Some(uri.to_owned()),
            region: "us-east-1".into(),
            languages: vec![],
            push_interval_s: interval,
        };
        RemoteStore::with_client(Client::from_conf(config), &cache).unwrap()
    }

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("aegis-remote-cache-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[tokio::test]
    async fn a_fresh_host_starts_from_the_caches_another_one_refreshed() {
        let server = MockServer::start().await;
        let bucket = Bucket::default();
        Mock::given(any())
            .respond_with(bucket.clone())
            .mount(&server)
            .await;
        let store = store(&server, 0).await;
        let key = "builds/repo/root-rust.tar.gz";
        let root = scratch("fresh");

        // The first worker: its gate's base copy built, and its fetch ran.
        let warm = root.join("warm/cache");
        write(&warm.join("build/target/debug/lib.rlib"), "built");
        write(&warm.join("usr_local_cargo_registry/index"), "crates");
        write(
            &warm.join("parts/web/build/node_modules/x"),
            "another part's",
        );
        aegis_gate::mark_refreshed(&warm.join("build")).unwrap();
        assert_eq!(
            store.push(key, &warm, &root.join("job1")).await,
            Ok(Pushed::Sent)
        );
        assert!(
            bucket
                .0
                .lock()
                .unwrap()
                .contains_key("/aegis-cache/builds/repo/root-rust.tar.gz")
        );
        // Nothing upstream changed since: nothing goes up again.
        assert_eq!(
            store.push(key, &warm, &root.join("job1")).await,
            Ok(Pushed::Unchanged)
        );

        // A fresh host has no cache: it gets the first worker's.
        let fresh = root.join("fresh/cache");
        assert_eq!(
            store.pull(key, &fresh, &root.join("job2")).await,
            Ok(Pulled::Fetched)
        );
        let read = |p: &str| std::fs::read_to_string(fresh.join(p)).ok();
        assert_eq!(
            read("build/target/debug/lib.rlib").as_deref(),
            Some("built")
        );
        assert_eq!(
            read("usr_local_cargo_registry/index").as_deref(),
            Some("crates")
        );
        assert_eq!(
            read("parts/web/build/node_modules/x"),
            None,
            "each part has its own archive"
        );
        // In sync once pulled: its next job sends nothing up unless its own
        // gate refreshes the cache.
        assert_eq!(
            store.push(key, &fresh, &root.join("job2")).await,
            Ok(Pushed::Unchanged)
        );
        assert!(
            std::fs::read_dir(root.join("job2"))
                .unwrap()
                .next()
                .is_none(),
            "no archive is left"
        );

        // A host with a cache of its own keeps it.
        write(&warm.join("build/target/debug/lib.rlib"), "newer here");
        assert_eq!(
            store.pull(key, &warm, &root.join("job3")).await,
            Ok(Pulled::LocalKept)
        );
        assert_eq!(
            std::fs::read_to_string(warm.join("build/target/debug/lib.rlib")).unwrap(),
            "newer here"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn a_store_without_the_archive_or_out_of_reach_leaves_the_cache_cold() {
        let server = MockServer::start().await;
        Mock::given(any())
            .respond_with(Bucket::default())
            .mount(&server)
            .await;
        let root = scratch("missing");
        let store = store(&server, 0).await;
        let dir = root.join("cache");
        assert_eq!(
            store.pull("builds/repo/root-go.tar.gz", &dir, &root).await,
            Ok(Pulled::Missing)
        );
        assert!(is_empty(&dir));

        // Nothing listens on port 1.
        let store = store_at("http://127.0.0.1:1", 0);
        let err = store
            .pull("builds/repo/root-go.tar.gz", &dir, &root)
            .await
            .unwrap_err();
        assert!(err.contains("root-go"), "{err}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn a_refreshed_cache_waits_for_the_push_interval() {
        let server = MockServer::start().await;
        let bucket = Bucket::default();
        Mock::given(any())
            .respond_with(bucket.clone())
            .mount(&server)
            .await;
        let store = store(&server, 3600).await;
        let root = scratch("interval");
        let dir = root.join("cache");
        let key = "builds/repo/root-python.tar.gz";
        // Never refreshed by upstream (only a patched tree's run): nothing.
        write(&dir.join("deps/.venv/lib"), "venv");
        assert_eq!(store.push(key, &dir, &root).await, Ok(Pushed::Unchanged));
        aegis_gate::mark_refreshed(&dir).unwrap();
        assert_eq!(store.push(key, &dir, &root).await, Ok(Pushed::Sent));
        std::thread::sleep(Duration::from_millis(20));
        aegis_gate::mark_refreshed(&dir).unwrap();
        assert_eq!(store.push(key, &dir, &root).await, Ok(Pushed::TooSoon));
        assert_eq!(bucket.0.lock().unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_url_names_a_bucket_and_plain_prefix() {
        let cache = |url: &str| RemoteCache {
            url: url.into(),
            endpoint: None,
            region: "eu-west-1".into(),
            languages: vec![aegis_lang::Language::Rust],
            push_interval_s: 0,
        };
        assert_eq!(
            cache("s3://aegis-cache").bucket_and_prefix(),
            Ok(("aegis-cache", ""))
        );
        assert_eq!(
            cache("s3://aegis-cache/tenant-a/builds/").bucket_and_prefix(),
            Ok(("aegis-cache", "tenant-a/builds"))
        );
        for bad in [
            "https://aegis-cache",
            "s3://Bad_Bucket",
            "s3://ab",
            "s3://aegis-cache/../x",
            "s3://aegis-cache/a//b",
        ] {
            assert!(cache(bad).check().is_err(), "{bad}");
        }
        let mut endpoint = cache("s3://aegis-cache");
        endpoint.endpoint = Some("minio:9000".into());
        assert!(endpoint.check().is_err());
        assert!(cache("s3://aegis-cache").covers(aegis_lang::Language::Rust));
        assert!(!cache("s3://aegis-cache").covers(aegis_lang::Language::Go));
    }
}
