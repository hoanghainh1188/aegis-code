//! The server's configuration (`config/server.toml`): where the API
//! listens, how the worker paces itself, and the configuration of each
//! mode. A job runs with its repo's mode and own settings, both read from
//! the `repos` table (#24); its limits there are `max_concurrent_jobs`
//! and the monthly budget, whose month `[budget]` sets (#31). No
//! secret is in it, only where they are read: `[secrets]` (#30).

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use aegis_lifecycle::cache_sweep::SweepRules;
use aegis_lifecycle::{ModeConfig, Modes};
use aegis_store::{BudgetRules, ModeName};
use openidconnect::url::Url;
use serde::{Deserialize, Deserializer};

use crate::chatops::ChatopsSection;
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
    #[serde(default)]
    pub metrics: MetricsSection,
    #[serde(default)]
    pub budget: BudgetSection,
    #[serde(default)]
    pub trackers: TrackersSection,
    #[serde(default)]
    pub sonar: SonarSection,
    #[serde(default)]
    pub reviews: ReviewsSection,
    #[serde(default)]
    pub web: WebSection,
    /// Slack and Teams told of jobs' ends (#594).
    #[serde(default)]
    pub chatops: ChatopsSection,
    /// Required to start; optional here so the file parses without it.
    #[serde(default)]
    pub secrets: Option<SecretsSection>,
    /// The instance's GitHub App (ADR 0017); without it the instance runs
    /// as v0.14.0 and refuses repos on the app path.
    #[serde(default)]
    pub github_app: Option<GithubAppSection>,
}

/// `[github_app]` (ADR 0017 §3): the app's id, its API root, and the names
/// of its secrets in the store; never a secret itself.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GithubAppSection {
    /// The app's id, or its client id (GitHub's preferred JWT issuer):
    /// exactly one.
    #[serde(default)]
    pub app_id: Option<u64>,
    #[serde(default)]
    pub client_id: Option<String>,
    /// The API root; GitHub Enterprise Server's `https://<host>/api/v3`.
    #[serde(default = "default_github_api")]
    pub api: String,
    #[serde(default = "default_app_key_env")]
    pub private_key_env: String,
    #[serde(default = "default_app_webhook_env")]
    pub webhook_secret_env: String,
}

fn default_github_api() -> String {
    "https://api.github.com".into()
}

fn default_app_key_env() -> String {
    "AEGIS_GITHUB_APP_PRIVATE_KEY".into()
}

fn default_app_webhook_env() -> String {
    "AEGIS_GITHUB_APP_WEBHOOK_SECRET".into()
}

impl GithubAppSection {
    /// The JWT's issuer: the app id or the client id.
    pub fn issuer(&self) -> Result<String, String> {
        match (&self.app_id, &self.client_id) {
            (Some(id), None) => Ok(id.to_string()),
            (None, Some(id)) if !id.trim().is_empty() => Ok(id.trim().to_owned()),
            (None, Some(_)) => Err("github_app: client_id is empty".into()),
            (Some(_), Some(_)) => Err("github_app: name app_id or client_id, not both".into()),
            (None, None) => Err("github_app: needs app_id or client_id".into()),
        }
    }
}

/// `[modes]`: each mode's run configuration, by name (#592), e.g.
/// `poc = "config/poc.toml"`, `customer-eu = "config/customer-eu.toml"`.
/// A name is a `repos.mode` one; `_` and `-` are the same, so the
/// `customer_jp = "…"` of an older server.toml is `customer-jp`, whose
/// configuration is refused at start unless it keeps every call in Japan
/// (design §7).
#[derive(Debug, Clone, Default)]
pub struct ModesSection(BTreeMap<ModeName, PathBuf>);

impl ModesSection {
    /// Each mode's name and the path of its configuration.
    pub fn iter(&self) -> impl Iterator<Item = (&ModeName, &PathBuf)> {
        self.0.iter()
    }

    /// The path of `mode`'s configuration, under either spelling.
    pub fn get(&self, mode: &str) -> Option<&PathBuf> {
        let key = ModeName::parse(mode).ok()?.key();
        self.0
            .iter()
            .find(|(name, _)| name.key() == key)
            .map(|(_, path)| path)
    }

    pub fn contains(&self, mode: &str) -> bool {
        self.get(mode).is_some()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl<'de> Deserialize<'de> for ModesSection {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = BTreeMap::<String, PathBuf>::deserialize(deserializer)?;
        let mut modes = BTreeMap::new();
        let mut keys = BTreeMap::new();
        for (name, path) in raw {
            let mode = ModeName::parse(&name)
                .map_err(|e| serde::de::Error::custom(format!("[modes]: mode name {e}")))?;
            if let Some(first) = keys.insert(mode.key(), name.clone()) {
                return Err(serde::de::Error::custom(format!(
                    "[modes]: `{first}` and `{name}` are the same mode: `_` and `-` are not told apart"
                )));
            }
            modes.insert(mode, path);
        }
        Ok(Self(modes))
    }
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

/// The Jira and Backlog filters of the repos (#26), polled by the `api`
/// role, one process per period (ADR 0003 §4).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrackersSection {
    /// Seconds between two polls; 0 polls none.
    pub poll_s: u64,
}

impl Default for TrackersSection {
    fn default() -> Self {
        Self { poll_s: 300 }
    }
}

/// The Sonar projects of the repos (#27), each polled on its own
/// `sonar_poll.every_s` by the `api` role, one process per period.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SonarSection {
    /// Seconds between two looks at which repos are due; 0 polls none.
    pub check_s: u64,
}

impl Default for SonarSection {
    fn default() -> Self {
        Self { check_s: 60 }
    }
}

/// The pages (#376).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebSection {
    /// Whether an admin may make a repo's webhooks from its connection
    /// page, with a token given for that once and never kept: off unless
    /// turned on, as that token can do more than the agent's.
    #[serde(default)]
    pub make_webhooks: bool,
    /// The language of PRs, replies, diagnoses and issue comments for a
    /// repo whose settings and tenant name none (#589): `en`, `ja` or
    /// `vi`; `en` when unset.
    #[serde(default)]
    pub default_locale: aegis_pr::Locale,
}

/// Review rounds on the agent's PRs (#243).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewsSection {
    /// Rounds a PR may have after the job that opened it.
    pub max_rounds: u32,
}

impl Default for ReviewsSection {
    fn default() -> Self {
        Self {
            max_rounds: aegis_store::DEFAULT_MAX_REVIEW_ROUNDS,
        }
    }
}

/// `/metrics` (#32), on its own listener in every role: for the private
/// network only, never the API's address ([`crate::metrics`]).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricsSection {
    pub listen: SocketAddr,
}

impl Default for MetricsSection {
    fn default() -> Self {
        Self {
            listen: SocketAddr::from(([127, 0, 0, 1], 9464)),
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
    /// Requests to the API without a live token, per client address, per
    /// API process; past it the address is refused before any database
    /// query (#173).
    #[serde(default = "default_api_failures_per_minute")]
    pub api_failures_per_minute: u32,
    /// How many proxies in front of the API append to `X-Forwarded-For`:
    /// 1 behind the ALB of `deploy/tofu`. The client's address is the one
    /// the outermost of them saw; 0 takes the TCP peer and ignores the
    /// header, which anyone can write.
    #[serde(default)]
    pub trusted_proxies: u8,
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

fn default_api_failures_per_minute() -> u32 {
    20
}

/// Repos' monthly budgets (#31); each repo's cap is set through the API.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BudgetSection {
    /// Where a month begins, `UTC` by default: `Asia/Tokyo`.
    #[serde(default = "default_time_zone")]
    pub time_zone: String,
    /// The share of a cap from which the dashboard warns.
    #[serde(default = "default_warn_ratio")]
    pub warn_ratio: f64,
}

impl Default for BudgetSection {
    fn default() -> Self {
        toml::from_str("").expect("every field has a default")
    }
}

fn default_time_zone() -> String {
    "UTC".into()
}

fn default_warn_ratio() -> f64 {
    0.8
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
    /// Queued jobs are claimed by SLA level, `P1` first (#524); each this
    /// long a job has waited lifts it one level, so none waits for ever.
    #[serde(default = "default_sla_aging_s")]
    pub sla_aging_s: u64,
    /// What the repos' caches on this worker may take together, in GB
    /// (#593): above it, the sweep removes the least recently used. 0: no
    /// limit.
    #[serde(default = "default_max_cache_disk_gb")]
    pub max_cache_disk_gb: u64,
    /// A repo's caches no job has used for this many days are removed
    /// (#593). 0: kept however old.
    #[serde(default = "default_cache_ttl_days")]
    pub cache_ttl_days: u64,
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

fn default_sla_aging_s() -> u64 {
    3600
}

fn default_max_cache_disk_gb() -> u64 {
    60
}

fn default_cache_ttl_days() -> u64 {
    14
}

const GB: u64 = 1 << 30;

/// A cache used this recently is never removed: it may be a job's that was
/// claimed after the sweep read the running jobs (#593).
const CACHE_GRACE: Duration = Duration::from_secs(600);
const DAY: Duration = Duration::from_secs(86_400);

fn default_drain_s() -> u64 {
    600
}

fn default_retry_base_s() -> u64 {
    30
}

impl ServerConfig {
    /// The configuration of each mode in `[modes]`, relative paths in
    /// them taken from `base`: each checked against its `[residency]`
    /// (#592) and `customer-jp` against its guard, so a worker whose mode
    /// would send code where it may not go does not start.
    pub fn load_modes(&self, base: &Path) -> Result<Modes, String> {
        if self.modes.is_empty() {
            return Err("[modes]: a worker needs at least one mode, e.g. `poc`".into());
        }
        let configs = self
            .modes
            .iter()
            .map(|(name, path)| {
                let path = base.join(path);
                ModeConfig::load(&path, base)
                    .map(|mode| (name.clone(), mode))
                    .map_err(|e| format!("{}: {e}", path.display()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let modes = Modes::from_configs(configs).map_err(|e| format!("[modes]: {e}"))?;
        Ok(modes
            .with_github_app(self.github_app.is_some())
            .with_default_locale(self.web.default_locale))
    }

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
        if w.max_cache_disk_gb.checked_mul(GB).is_none() {
            return Err(format!(
                "worker: max_cache_disk_gb {} is too large",
                w.max_cache_disk_gb
            ));
        }
        if u32::try_from(w.cache_ttl_days).is_err() {
            return Err(format!(
                "worker: cache_ttl_days {} is too large",
                w.cache_ttl_days
            ));
        }
        if let Some(app) = &config.github_app {
            app.issuer()?;
            for name in [&app.private_key_env, &app.webhook_secret_env] {
                if name.trim().is_empty() {
                    return Err("github_app: a secret's name is empty".into());
                }
            }
        }
        if config.metrics.listen == config.api.listen {
            return Err(format!(
                "metrics: listen must not be the API's address ({}): /metrics is \
                 for the private network only",
                config.api.listen
            ));
        }
        let a = &config.auth;
        if a.session_hours == 0 || a.api_requests_per_minute == 0 || a.api_failures_per_minute == 0
        {
            return Err("auth: session_hours, api_requests_per_minute and \
                 api_failures_per_minute must be at least 1"
                .into());
        }
        if a.oidc.is_some() && a.public_url.is_none() {
            return Err("auth: oidc needs public_url, where the SSO sends people back".into());
        }
        config.chatops.check()?;
        let b = &config.budget;
        if !(b.warn_ratio > 0.0 && b.warn_ratio <= 1.0) {
            return Err(format!(
                "budget: warn_ratio {} must be above 0 and at most 1",
                b.warn_ratio
            ));
        }
        Ok(config)
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Self::from_toml(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// A repo with less than a job's cap left of its month runs no job.
    pub fn budget_rules(&self) -> BudgetRules {
        BudgetRules {
            time_zone: self.budget.time_zone.clone(),
            job_reserve_usd: self.worker.max_cost_usd,
            warn_ratio: self.budget.warn_ratio,
        }
    }

    /// How the worker keeps its caches (#593); `None` when both the limit
    /// and the TTL are off, and no sweep removes anything.
    pub fn cache_rules(&self) -> Option<SweepRules> {
        let w = &self.worker;
        let max_bytes = w.max_cache_disk_gb.saturating_mul(GB);
        let days = u32::try_from(w.cache_ttl_days).unwrap_or(u32::MAX);
        let ttl = (days > 0).then(|| DAY * days);
        (max_bytes > 0 || ttl.is_some()).then_some(SweepRules {
            max_bytes,
            ttl,
            grace: CACHE_GRACE,
        })
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
    fn the_github_app_names_one_issuer_and_secret_names_only() {
        assert_eq!(ServerConfig::from_toml("").unwrap().github_app, None);
        let app = ServerConfig::from_toml("[github_app]\napp_id = 123456\n")
            .unwrap()
            .github_app
            .unwrap();
        assert_eq!(app.issuer().unwrap(), "123456");
        assert_eq!(app.api, "https://api.github.com");
        assert_eq!(app.private_key_env, "AEGIS_GITHUB_APP_PRIVATE_KEY");
        assert_eq!(app.webhook_secret_env, "AEGIS_GITHUB_APP_WEBHOOK_SECRET");
        let by_client = ServerConfig::from_toml(
            "[github_app]\nclient_id = \"Iv23abc\"\napi = \"https://ghe.acme.test/api/v3\"\n",
        )
        .unwrap()
        .github_app
        .unwrap();
        assert_eq!(by_client.issuer().unwrap(), "Iv23abc");
        for (bad, says) in [
            ("[github_app]\n", "app_id or client_id"),
            ("[github_app]\napp_id = 1\nclient_id = \"x\"\n", "not both"),
            (
                "[github_app]\napp_id = 1\nprivate_key_env = \" \"\n",
                "empty",
            ),
            (
                "[github_app]\napp_id = 1\nprivate_key = \"-----BEGIN\"\n",
                "unknown field",
            ),
        ] {
            let err = ServerConfig::from_toml(bad).unwrap_err();
            assert!(err.contains(says), "{bad}: {err}");
        }
    }

    /// #592: any mode by name; the older `customer_jp` key still names
    /// `customer-jp`.
    #[test]
    fn modes_take_any_name_and_the_old_spelling() {
        let config = ServerConfig::from_toml(
            "[modes]\npoc = \"config/poc.toml\"\ncustomer_jp = \"config/customer-jp.toml\"\n\
             customer-eu = \"config/customer-eu.toml\"\nlocal-airgap = \"config/airgap.toml\"\n",
        )
        .unwrap();
        let names: Vec<&str> = config.modes.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["customer-eu", "customer_jp", "local-airgap", "poc"]);
        assert_eq!(
            config.modes.get("customer-jp"),
            Some(&PathBuf::from("config/customer-jp.toml"))
        );
        assert!(config.modes.contains("customer_eu"));
        for (bad, says) in [
            ("[modes]\nCustomer = \"x.toml\"\n", "mode name"),
            ("[modes]\n\"\" = \"x.toml\"\n", "mode name"),
            (
                "[modes]\ncustomer_jp = \"a.toml\"\ncustomer-jp = \"b.toml\"\n",
                "same mode",
            ),
        ] {
            let err = ServerConfig::from_toml(bad).unwrap_err();
            assert!(err.contains(says), "{bad}: {err}");
        }
    }

    #[test]
    fn the_example_configuration_loads() {
        let config = ServerConfig::from_toml(include_str!("../../../config/server.toml")).unwrap();
        assert_eq!(
            config.modes.get("poc"),
            Some(&PathBuf::from("config/poc.toml"))
        );
        assert!(!config.modes.contains("customer-jp"));
        assert!(config.api.listen.ip().is_loopback());
        assert_eq!(config.metrics.listen.port(), 9464);
        assert!(config.auth.oidc.is_none());
        assert_eq!(config.auth.session_hours, 12);
        assert_eq!(config.sonar.check_s, 60);
        let worker = config.worker("w".into());
        assert!(worker.renew_every < worker.lease);
        assert_eq!(worker.max_cost_usd, 4.0);
        let secrets = config.secrets.expect("[secrets] is in the example");
        assert_eq!(secrets.source, crate::secrets::SourceKind::Env);
    }

    #[test]
    fn chat_channels_are_read_and_checked() {
        let ok = ServerConfig::from_toml(
            "[[chatops.channels]]\nkind = \"teams\"\nwebhook_env = \"AEGIS_TEAMS_WEBHOOK_URL\"\n\
             events = [\"p1_stopped\"]",
        )
        .unwrap();
        assert_eq!(ok.chatops.channels.len(), 1);
        assert!(
            ServerConfig::from_toml("")
                .unwrap()
                .chatops
                .channels
                .is_empty()
        );
        let err = ServerConfig::from_toml(
            "[[chatops.channels]]\nkind = \"slack\"\nwebhook_env = \"a/b\"",
        )
        .unwrap_err();
        assert!(err.contains("chatops.channels[0]"), "{err}");
    }

    #[test]
    fn bad_worker_settings_are_refused() {
        for (bad, why) in [
            ("[worker]\nlease_s = 10\nrenew_s = 10\n", "renew_s"),
            ("[worker]\nmax_cost_usd = 0.0\n", "max_cost_usd"),
            ("[worker]\nrepos = []\n", "unknown field"),
            (
                "[worker]\nmax_cache_disk_gb = 18446744073709551615\n",
                "max_cache_disk_gb",
            ),
            ("[worker]\ncache_ttl_days = 4294967296\n", "cache_ttl_days"),
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
            (
                "[api]\nlisten = \"0.0.0.0:8080\"\n[metrics]\nlisten = \"0.0.0.0:8080\"\n",
                "metrics",
            ),
            ("[budget]\nwarn_ratio = 1.5\n", "warn_ratio"),
        ] {
            let err = ServerConfig::from_toml(bad).unwrap_err();
            assert!(err.contains(why), "{bad}: {err}");
        }
        let config = ServerConfig::from_toml("").unwrap();
        assert_eq!(config.worker.max_workers, 4);
        let rules = config.budget_rules();
        assert_eq!(
            (rules.time_zone.as_str(), rules.job_reserve_usd),
            ("UTC", 4.0)
        );
        assert!(config.modes.is_empty());
    }

    #[test]
    fn the_cache_rules_default_to_60_gb_and_14_days_and_zero_turns_each_off() {
        let rules = ServerConfig::from_toml("").unwrap().cache_rules().unwrap();
        assert_eq!(rules.max_bytes, 60 * GB);
        assert_eq!(rules.ttl, Some(14 * DAY));
        assert_eq!(rules.grace, CACHE_GRACE);

        let no_limit = "[worker]\nmax_cache_disk_gb = 0\n";
        let rules = ServerConfig::from_toml(no_limit).unwrap().cache_rules();
        assert_eq!(
            rules.map(|r| (r.max_bytes, r.ttl)),
            Some((0, Some(14 * DAY)))
        );

        let off = "[worker]\nmax_cache_disk_gb = 0\ncache_ttl_days = 0\n";
        assert_eq!(ServerConfig::from_toml(off).unwrap().cache_rules(), None);
    }
}
