//! The server's configuration (`config/server.toml`): where the API
//! listens, how the worker paces itself, and the configuration of each
//! mode. A job runs with its repo's mode and own settings, both read from
//! the `repos` table (#24); its limit there is `max_concurrent_jobs`. The
//! database URL is not in it: `AEGIS_DATABASE_URL`.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

use crate::worker::WorkerConfig;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    /// The configuration of each mode; a mode left out runs no jobs, and
    /// its repos' jobs stop with a reason.
    #[serde(default)]
    pub modes: ModesSection,
    #[serde(default)]
    pub api: ApiSection,
    #[serde(default)]
    pub worker: WorkerSection,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModesSection {
    /// `config/poc.toml`.
    #[serde(default)]
    pub poc: Option<PathBuf>,
    /// `config/customer-jp.toml`; refused at start unless it keeps every
    /// call in Japan (design §7).
    #[serde(default)]
    pub customer_jp: Option<PathBuf>,
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
    /// The most a job may spend over all its attempts, whatever its repo.
    #[serde(default = "default_max_cost_usd")]
    pub max_cost_usd: f64,
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

fn default_max_workers() -> u32 {
    4
}

fn default_max_cost_usd() -> f64 {
    4.0
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
        if w.max_workers == 0 {
            return Err("worker: max_workers must be at least 1".into());
        }
        if !(w.max_cost_usd.is_finite() && w.max_cost_usd > 0.0) {
            return Err(format!(
                "worker: max_cost_usd {} must be above 0",
                w.max_cost_usd
            ));
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

    pub fn worker(&self, worker_id: String) -> WorkerConfig {
        let w = &self.worker;
        WorkerConfig {
            worker_id,
            max_workers: w.max_workers,
            lease: Duration::from_secs(w.lease_s),
            renew_every: Duration::from_secs(w.renew_s),
            poll_every: Duration::from_secs(w.poll_s),
            drain: Duration::from_secs(w.drain_s),
            retry_base: Duration::from_secs(w.retry_base_s),
            max_cost_usd: w.max_cost_usd,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_example_configuration_loads() {
        let config = ServerConfig::from_toml(include_str!("../../../config/server.toml")).unwrap();
        assert_eq!(config.modes.poc, Some(PathBuf::from("config/poc.toml")));
        assert_eq!(config.modes.customer_jp, None);
        assert!(config.api.listen.ip().is_loopback());
        let worker = config.worker("w".into());
        assert!(worker.renew_every < worker.lease);
        assert_eq!(worker.max_cost_usd, 4.0);
    }

    #[test]
    fn bad_worker_settings_are_refused() {
        for (bad, why) in [
            ("[worker]\nlease_s = 10\nrenew_s = 10\n", "renew_s"),
            ("[worker]\nmax_cost_usd = 0.0\n", "max_cost_usd"),
            ("[worker]\nrepos = []\n", "unknown field"),
        ] {
            let err = ServerConfig::from_toml(bad).unwrap_err();
            assert!(err.contains(why), "{bad}: {err}");
        }
        let config = ServerConfig::from_toml("").unwrap();
        assert_eq!(config.worker.max_workers, 4);
        assert!(config.modes.poc.is_none());
    }
}
