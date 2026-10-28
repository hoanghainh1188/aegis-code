//! The one place the platform reads secrets (ADR 0003 §6, #30): model API
//! keys, webhook secrets, Git and Sonar tokens, the database URL.
//!
//! [`Secrets`] keeps the values it has read, by name, in memory only: never
//! in a file or the process environment, never in a log. The source is AWS
//! Secrets Manager ([`AwsSource`], feature `aws`) in deployments on AWS, or
//! a directory of secret files ([`FileSource`]) on premises; reading the
//! environment ([`EnvSource`]) is for dev and tests and must be chosen by
//! name.
//!
//! - At start, [`Secrets::require`] reads what the process cannot run
//!   without and names every one that is missing, never a value.
//! - While running, [`Secrets::refresh`] reads every known secret again
//!   (periodically), and [`Secrets::refresh_after_auth_failure`] reads one
//!   again when a call with it was refused. A call that already took a
//!   value keeps it; the next [`Secrets::get`] sees the new one.

mod file;
mod source;

#[cfg(feature = "aws")]
mod aws;

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::{Duration, Instant};

#[cfg(feature = "aws")]
pub use aws::AwsSource;
pub use file::FileSource;
pub use source::{EnvSource, Fetch, MemorySource, NoSource, SecretSource};

/// After a refusal, a secret is read again at most this often: a peer
/// sending bad signatures must not make us hammer the source.
pub const DEFAULT_MIN_FORCED_GAP: Duration = Duration::from_secs(30);

/// A secret's value. Printed as `Secret(…)`; there is no `Display`.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(Arc<str>);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(Arc::from(value.into()))
    }

    /// The value, for the one call that needs it.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(…)")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SecretError {
    /// Every required secret the source does not have, by name.
    #[error("missing secrets in {store}: {}", names.join(", "))]
    Missing { store: String, names: Vec<String> },
    #[error("reading secret {name} from {store}: {message}")]
    Source {
        store: String,
        name: String,
        message: String,
    },
}

struct Inner {
    source: Box<dyn SecretSource>,
    values: RwLock<HashMap<String, Secret>>,
    /// Every name asked for, found or not: a refresh reads them all, so a
    /// secret added to the source later is found then.
    wanted: RwLock<BTreeSet<String>>,
    forced: Mutex<HashMap<String, Instant>>,
    min_forced_gap: Duration,
}

/// The secrets read so far, shared by every part of the process.
#[derive(Clone)]
pub struct Secrets {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Secrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Secrets")
            .field("source", &self.inner.source.describe())
            .field("names", &self.names())
            .finish()
    }
}

impl Secrets {
    pub fn new(source: impl SecretSource + 'static) -> Self {
        Self::with_min_forced_gap(source, DEFAULT_MIN_FORCED_GAP)
    }

    pub fn with_min_forced_gap(source: impl SecretSource + 'static, gap: Duration) -> Self {
        Self {
            inner: Arc::new(Inner {
                source: Box::new(source),
                values: RwLock::default(),
                wanted: RwLock::default(),
                forced: Mutex::default(),
                min_forced_gap: gap,
            }),
        }
    }

    /// No source: every secret is missing.
    pub fn none() -> Self {
        Self::new(NoSource)
    }

    /// These values, already read, from a [`MemorySource`] holding the
    /// same: for tests that need no store.
    pub fn fixed<'a>(values: impl IntoIterator<Item = (&'a str, &'a str)>) -> Self {
        let values: Vec<_> = values.into_iter().collect();
        let secrets = Self::new(MemorySource::new(values.iter().copied()));
        for (name, value) in values {
            secrets.want(name);
            secrets.store(name, Secret::new(value));
        }
        secrets
    }

    /// What the secrets come from, for logs and errors.
    pub fn source(&self) -> String {
        self.inner.source.describe()
    }

    /// The names asked for so far, found or not, sorted.
    pub fn names(&self) -> Vec<String> {
        self.inner
            .wanted
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .cloned()
            .collect()
    }

    fn want(&self, name: &str) {
        let mut wanted = self
            .inner
            .wanted
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        if !wanted.contains(name) {
            wanted.insert(name.to_owned());
        }
    }

    fn values(&self) -> std::sync::RwLockReadGuard<'_, HashMap<String, Secret>> {
        self.inner
            .values
            .read()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn store(&self, name: &str, value: Secret) -> bool {
        let mut values = self
            .inner
            .values
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        values.insert(name.to_owned(), value.clone()) != Some(value)
    }

    /// From the source now; an empty value is no value.
    async fn fetch(&self, name: &str) -> Result<Option<Secret>, SecretError> {
        let value = self
            .inner
            .source
            .fetch(name)
            .await
            .map_err(|message| SecretError::Source {
                store: self.source(),
                name: name.to_owned(),
                message,
            })?;
        Ok(value.filter(|v| !v.trim().is_empty()).map(Secret::new))
    }

    /// The value read last, without asking the source.
    pub fn get(&self, name: &str) -> Option<Secret> {
        self.values().get(name).cloned()
    }

    /// Reads `name` now and keeps it; `None` when the source has none.
    pub async fn load(&self, name: &str) -> Result<Option<Secret>, SecretError> {
        self.want(name);
        let value = self.fetch(name).await?;
        if let Some(value) = &value {
            self.store(name, value.clone());
        }
        Ok(value)
    }

    /// The value read last, or read now the first time.
    pub async fn get_or_load(&self, name: &str) -> Result<Option<Secret>, SecretError> {
        match self.get(name) {
            Some(value) => Ok(Some(value)),
            None => self.load(name).await,
        }
    }

    /// Reads every one of `names`; fails naming all those missing.
    pub async fn require(&self, names: &[&str]) -> Result<(), SecretError> {
        let mut missing = Vec::new();
        for name in names {
            if self.load(name).await?.is_none() {
                missing.push((*name).to_owned());
            }
        }
        missing.sort();
        missing.dedup();
        if missing.is_empty() {
            Ok(())
        } else {
            Err(SecretError::Missing {
                store: self.source(),
                names: missing,
            })
        }
    }

    /// Reads every known secret again; the names whose value changed.
    /// A secret the source fails on or no longer has keeps its value.
    pub async fn refresh(&self) -> Vec<String> {
        let mut changed = Vec::new();
        for name in self.names() {
            match self.fetch(&name).await {
                Ok(Some(value)) => {
                    if self.store(&name, value) {
                        tracing::info!(secret = %name, "secret rotated");
                        changed.push(name);
                    }
                }
                Ok(None) if self.get(&name).is_some() => {
                    tracing::warn!(secret = %name, "secret gone from the source: keeping the last value");
                }
                Ok(None) => {}
                Err(e) => tracing::warn!(error = %e, "refreshing a secret"),
            }
        }
        changed
    }

    /// After a call with `name` was refused: reads it again, unless that
    /// was done less than the minimum gap ago. The new value when it
    /// changed, so the caller may retry once.
    pub async fn refresh_after_auth_failure(&self, name: &str) -> Option<Secret> {
        {
            let mut forced = self
                .inner
                .forced
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let now = Instant::now();
            if forced
                .get(name)
                .is_some_and(|last| now.duration_since(*last) < self.inner.min_forced_gap)
            {
                return None;
            }
            forced.insert(name.to_owned(), now);
        }
        match self.fetch(name).await {
            Ok(Some(value)) if self.store(name, value.clone()) => {
                tracing::info!(secret = %name, "secret rotated after a refused call");
                Some(value)
            }
            Ok(_) => None,
            Err(e) => {
                tracing::warn!(error = %e, "refreshing a secret after a refused call");
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_missing_secret_fails_the_start_by_name_only() {
        let source = MemorySource::new([("PRESENT", "s3cr3t-value"), ("EMPTY", " ")]);
        let secrets = Secrets::new(source);
        let err = secrets
            .require(&["PRESENT", "B_MISSING", "EMPTY", "A_MISSING"])
            .await
            .unwrap_err();
        let text = err.to_string();
        assert_eq!(
            text,
            "missing secrets in memory: A_MISSING, B_MISSING, EMPTY"
        );
        assert!(!text.contains("s3cr3t"));
        assert_eq!(secrets.get("PRESENT").unwrap().expose(), "s3cr3t-value");
    }

    #[tokio::test]
    async fn a_rotated_value_is_seen_by_the_next_call_not_the_running_one() {
        let source = MemorySource::new([("KEY", "old")]);
        let secrets = Secrets::new(source.clone());
        secrets.require(&["KEY"]).await.unwrap();
        let running = secrets.get("KEY").unwrap();

        source.set("KEY", "new");
        assert_eq!(secrets.get("KEY").unwrap().expose(), "old", "not yet read");
        assert_eq!(secrets.refresh().await, vec!["KEY".to_owned()]);
        assert_eq!(running.expose(), "old");
        assert_eq!(secrets.get("KEY").unwrap().expose(), "new");
        assert!(secrets.refresh().await.is_empty(), "nothing changed since");
    }

    #[tokio::test]
    async fn a_secret_that_fails_or_goes_keeps_its_value() {
        let source = MemorySource::new([("KEY", "v1")]);
        let secrets = Secrets::new(source.clone());
        secrets.require(&["KEY"]).await.unwrap();
        source.fail(true);
        assert!(secrets.refresh().await.is_empty());
        source.fail(false);
        source.remove("KEY");
        assert!(secrets.refresh().await.is_empty());
        assert_eq!(secrets.get("KEY").unwrap().expose(), "v1");
    }

    #[tokio::test]
    async fn a_refused_call_reads_the_secret_again_at_most_once_per_gap() {
        let source = MemorySource::new([("KEY", "v1")]);
        let secrets = Secrets::with_min_forced_gap(source.clone(), Duration::from_secs(3600));
        secrets.require(&["KEY"]).await.unwrap();
        source.set("KEY", "v2");
        let fetched = source.fetches();
        assert_eq!(
            secrets
                .refresh_after_auth_failure("KEY")
                .await
                .unwrap()
                .expose(),
            "v2"
        );
        source.set("KEY", "v3");
        assert!(secrets.refresh_after_auth_failure("KEY").await.is_none());
        assert_eq!(source.fetches(), fetched + 1, "the second one was not read");
        assert_eq!(secrets.get("KEY").unwrap().expose(), "v2");
    }

    #[tokio::test]
    async fn values_are_never_printed_nor_put_in_the_environment() {
        let secrets = Secrets::new(MemorySource::new([("AEGIS_TEST_PRINTED", "hunter2")]));
        secrets.require(&["AEGIS_TEST_PRINTED"]).await.unwrap();
        let printed = format!("{secrets:?} {:?}", secrets.get("AEGIS_TEST_PRINTED"));
        assert!(!printed.contains("hunter2"), "{printed}");
        assert!(printed.contains("AEGIS_TEST_PRINTED"), "{printed}");
        assert!(std::env::vars().all(|(_, v)| !v.contains("hunter2")));
    }

    #[tokio::test]
    async fn a_secret_missing_at_start_is_found_by_a_later_refresh() {
        let source = MemorySource::default();
        let secrets = Secrets::new(source.clone());
        assert_eq!(secrets.load("HOOK").await, Ok(None));
        source.set("HOOK", "added");
        assert_eq!(secrets.refresh().await, vec!["HOOK".to_owned()]);
        assert_eq!(secrets.get("HOOK").unwrap().expose(), "added");
    }

    #[test]
    fn fixed_values_are_read_already() {
        let secrets = Secrets::fixed([("A", "1")]);
        assert_eq!(secrets.get("A").unwrap().expose(), "1");
        assert_eq!(secrets.names(), vec!["A".to_owned()]);
    }

    #[tokio::test]
    async fn no_source_has_no_secret() {
        let err = Secrets::none().require(&["X"]).await.unwrap_err();
        assert!(matches!(err, SecretError::Missing { .. }), "{err}");
    }
}
