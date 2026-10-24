//! The server's configuration (`config/server.toml`): where the API
//! listens, how the worker paces itself, and the run configuration its
//! jobs use. The database URL is not in it: `AEGIS_DATABASE_URL`.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use aegis_store::RepoId;
use serde::Deserialize;

use crate::worker::WorkerConfig;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    /// The run configuration of the jobs (`config/poc.toml`); one repo
    /// until #24 gives each repo its own.
    pub run_config: PathBuf,
    #[serde(default)]
    pub api: ApiSection,
    #[serde(default)]
    pub worker: WorkerSection,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiSection {
    pub listen: SocketAddr,
}

impl Default for ApiSection {
    fn default() -> Self {
        Self {
            listen: SocketAddr::from(([127, 0, 0, 1], 8080)),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerSection {
    #[serde(default = "default_max_workers")]
    pub max_workers: u32,
    /// Of a repo without its own `[[worker.repos]]` value.
    #[serde(default = "default_max_concurrent_jobs")]
    pub max_concurrent_jobs: u32,
    #[serde(default)]
    pub repos: Vec<RepoLimit>,
    #[serde(default = "default_lease_s")]
    pub lease_s: u64,
    #[serde(default = "default_renew_s")]
    pub renew_s: u64,
    #[serde(default = "default_poll_s")]
    pub poll_s: u64,
    #[serde(default = "default_drain_s")]
    pub drain_s: u64,
    #[serde(default = "default_retry_base_s")]
    pub retry_base_s: u64,
}

impl Default for WorkerSection {
    fn default() -> Self {
        toml::from_str("").expect("every field has a default")
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepoLimit {
    pub repo_id: i64,
    pub max_concurrent_jobs: u32,
}

fn default_max_workers() -> u32 {
    4
}

fn default_max_concurrent_jobs() -> u32 {
    2
}

fn default_lease_s() -> u64 {
    60
}

fn default_renew_s() -> u64 {
    20
}

fn default_poll_s() -> u64 {
    5
}

fn default_drain_s() -> u64 {
    600
}

fn default_retry_base_s() -> u64 {
    30
}

impl ServerConfig {
    pub fn from_toml(text: &str) -> Result<Self, String> {
        let config: Self = toml::from_str(text).map_err(|e| e.to_string())?;
        let w = &config.worker;
        if w.max_workers == 0 || w.max_concurrent_jobs == 0 {
            return Err("worker: max_workers and max_concurrent_jobs must be at least 1".into());
        }
        if w.renew_s == 0 || w.renew_s >= w.lease_s {
            return Err(format!(
                "worker: renew_s ({}) must be at least 1 and below lease_s ({})",
                w.renew_s, w.lease_s
            ));
        }
        if w.poll_s == 0 {
            return Err("worker: poll_s must be at least 1".into());
        }
        Ok(config)
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Self::from_toml(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// The worker's settings; `max_cost_usd` comes from the run
    /// configuration's budget.
    pub fn worker(&self, worker_id: String, max_cost_usd: f64) -> WorkerConfig {
        let w = &self.worker;
        WorkerConfig {
            worker_id,
            max_workers: w.max_workers,
            max_concurrent_jobs: w.max_concurrent_jobs,
            repo_limits: w
                .repos
                .iter()
                .map(|r| (RepoId(r.repo_id), r.max_concurrent_jobs))
                .collect(),
            lease: Duration::from_secs(w.lease_s),
            renew_every: Duration::from_secs(w.renew_s),
            poll_every: Duration::from_secs(w.poll_s),
            drain: Duration::from_secs(w.drain_s),
            retry_base: Duration::from_secs(w.retry_base_s),
            max_cost_usd,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_example_configuration_loads() {
        let config = ServerConfig::from_toml(include_str!("../../../config/server.toml")).unwrap();
        assert_eq!(config.run_config, PathBuf::from("config/poc.toml"));
        assert!(config.api.listen.ip().is_loopback());
        let worker = config.worker("w".into(), 4.0);
        assert!(worker.renew_every < worker.lease);
        assert_eq!(worker.max_cost_usd, 4.0);
    }

    #[test]
    fn a_lease_renewed_too_late_is_refused() {
        let err = ServerConfig::from_toml(
            "run_config = \"x.toml\"\n[worker]\nlease_s = 10\nrenew_s = 10\n",
        )
        .unwrap_err();
        assert!(err.contains("renew_s"), "{err}");
        let config = ServerConfig::from_toml("run_config = \"x.toml\"").unwrap();
        assert_eq!(config.worker.max_workers, 4);
    }
}
