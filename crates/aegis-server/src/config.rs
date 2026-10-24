//! The server's configuration (`config/server.toml`): where the API
//! listens, how the worker paces itself, and the configuration of each
//! mode. A job runs with its repo's mode and own settings, both read from
//! the `repos` table (#24); its limit there is `max_concurrent_jobs`. No
//! secret is in it, only where they are read: `[secrets]` (#30).

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use openidconnect::url::Url;
use serde::{Deserialize, Deserializer};

use crate::secrets::SecretsSection;
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
    #[serde(default)]
    pub auth: AuthSection,
    /// Required to start; optional here so the file parses without it.
    #[serde(default)]
    pub secrets: Option<SecretsSection>,
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

/// Sign-in (#29). The dashboard needs `oidc`; the API needs only tokens.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthSection {
    /// Where people open the dashboard, `https://aegis.example.com`. Pages
    /// answer to its host and to loopback only; the SSO sends people back
    /// to `<public_url>/auth/callback`.
    #[serde(default, deserialize_with = "url")]
    pub public_url: Option<Url>,
    #[serde(default = "default_session_hours")]
    pub session_hours: u32,
    /// Per service token, per API process.
    #[serde(default = "default_api_requests_per_minute")]
    pub api_requests_per_minute: u32,
    #[serde(default)]
    pub oidc: Option<OidcSection>,
}

impl Default for AuthSection {
    fn default() -> Self {
        toml::from_str("").expect("every field has a default")
    }
}

/// The company SSO. Its client secret is `AEGIS_OIDC_CLIENT_SECRET`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OidcSection {
    /// `https://login.example.com/realms/acme`: discovery is read from
    /// `<issuer>/.well-known/openid-configuration`.
    pub issuer: String,
    pub client_id: String,
}

fn url<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Url>, D::Error> {
    let text = String::deserialize(d)?;
    let url = Url::parse(&text).map_err(serde::de::Error::custom)?;
    if url.host_str().is_none() || url.path() != "/" || url.query().is_some() {
        return Err(serde::de::Error::custom(format!(
            "`{text}`: a scheme and a host, no path"
        )));
    }
    // The session cookie is `Secure`: browsers keep it over HTTPS only,
    // and over plain HTTP on loopback.
    let loopback = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    if url.scheme() != "https" && !loopback {
        return Err(serde::de::Error::custom(format!(
            "`{text}`: https, unless on loopback"
        )));
    }
    Ok(Some(url))
}

fn default_session_hours() -> u32 {
    12
}

fn default_api_requests_per_minute() -> u32 {
    120
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
        let a = &config.auth;
        if a.session_hours == 0 || a.api_requests_per_minute == 0 {
            return Err(
                "auth: session_hours and api_requests_per_minute must be at least 1".into(),
            );
        }
        if a.oidc.is_some() && a.public_url.is_none() {
            return Err("auth: oidc needs public_url, where the SSO sends people back".into());
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
        assert!(config.auth.oidc.is_none());
        assert_eq!(config.auth.session_hours, 12);
        let worker = config.worker("w".into());
        assert!(worker.renew_every < worker.lease);
        assert_eq!(worker.max_cost_usd, 4.0);
        let secrets = config.secrets.expect("[secrets] is in the example");
        assert_eq!(secrets.source, crate::secrets::SourceKind::Env);
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
        for (bad, why) in [
            (
                "[auth.oidc]\nissuer = \"https://sso\"\nclient_id = \"a\"\n",
                "public_url",
            ),
            (
                "[auth]\npublic_url = \"https://aegis.test/dash\"\n",
                "no path",
            ),
            ("[auth]\nsession_hours = 0\n", "session_hours"),
        ] {
            let err = ServerConfig::from_toml(bad).unwrap_err();
            assert!(err.contains(why), "{bad}: {err}");
        }
        let config = ServerConfig::from_toml("").unwrap();
        assert_eq!(config.worker.max_workers, 4);
        assert!(config.modes.poc.is_none());
    }
}
