//! Where secrets are read from.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

/// A read of one secret: its value, `None` when the source has no such
/// secret, or why the source failed (never holding the value).
pub type Fetch<'a> = Pin<Box<dyn Future<Output = Result<Option<String>, String>> + Send + 'a>>;

pub trait SecretSource: Send + Sync {
    /// What the source is, for logs and errors: no value, no credential.
    fn describe(&self) -> String;

    fn fetch<'a>(&'a self, name: &'a str) -> Fetch<'a>;
}

/// The process environment, secret `X` being variable `X`. For dev and
/// tests only: in a deployment the values would sit in the environment
/// for the life of the process.
#[derive(Debug, Clone, Copy)]
pub struct EnvSource {
    _explicit: (),
}

impl EnvSource {
    /// Named so that no deployment reads its secrets from the environment
    /// by accident.
    pub fn for_dev_and_tests() -> Self {
        Self { _explicit: () }
    }
}

impl SecretSource for EnvSource {
    fn describe(&self) -> String {
        "the environment (dev and tests only)".into()
    }

    fn fetch<'a>(&'a self, name: &'a str) -> Fetch<'a> {
        Box::pin(async move { Ok(std::env::var(name).ok()) })
    }
}

/// No secret at all.
#[derive(Debug, Clone, Copy)]
pub struct NoSource;

impl SecretSource for NoSource {
    fn describe(&self) -> String {
        "no secret source".into()
    }

    fn fetch<'a>(&'a self, _name: &'a str) -> Fetch<'a> {
        Box::pin(async { Ok(None) })
    }
}

/// Values held in memory, changed by the test while the process runs: a
/// stand-in for Secrets Manager. Clones share their values.
#[derive(Debug, Clone, Default)]
pub struct MemorySource {
    values: Arc<Mutex<HashMap<String, String>>>,
    failing: Arc<AtomicBool>,
    fetches: Arc<AtomicUsize>,
}

impl MemorySource {
    pub fn new<'a>(values: impl IntoIterator<Item = (&'a str, &'a str)>) -> Self {
        let source = Self::default();
        for (name, value) in values {
            source.set(name, value);
        }
        source
    }

    fn values(&self) -> std::sync::MutexGuard<'_, HashMap<String, String>> {
        self.values.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Rotates `name`, or adds it.
    pub fn set(&self, name: &str, value: &str) {
        self.values().insert(name.to_owned(), value.to_owned());
    }

    pub fn remove(&self, name: &str) {
        self.values().remove(name);
    }

    /// Every read fails while set, as when the source cannot be reached.
    pub fn fail(&self, failing: bool) {
        self.failing.store(failing, Ordering::SeqCst);
    }

    /// Reads so far.
    pub fn fetches(&self) -> usize {
        self.fetches.load(Ordering::SeqCst)
    }
}

impl SecretSource for MemorySource {
    fn describe(&self) -> String {
        "memory".into()
    }

    fn fetch<'a>(&'a self, name: &'a str) -> Fetch<'a> {
        self.fetches.fetch_add(1, Ordering::SeqCst);
        let value = if self.failing.load(Ordering::SeqCst) {
            Err("unreachable".to_owned())
        } else {
            Ok(self.values().get(name).cloned())
        };
        Box::pin(async move { value })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_environment_source_reads_the_variable_of_the_same_name() {
        let source = EnvSource::for_dev_and_tests();
        assert!(source.fetch("PATH").await.unwrap().is_some());
        assert_eq!(
            source.fetch("AEGIS_TEST_SURELY_UNSET_SECRET").await,
            Ok(None)
        );
    }
}
