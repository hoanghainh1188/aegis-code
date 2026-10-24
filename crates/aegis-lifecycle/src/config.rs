//! The run configuration (`config/poc.toml`, `config/customer-jp.toml`):
//! provider and models as before, plus the target repo, where the job's
//! files go, budget and the fork that receives PRs (design §1: the two
//! configurations differ only in files, never in code).

use std::path::{Path, PathBuf};

use aegis_lang::{Language, ProfileOverrides};
use aegis_provider::ProviderConfig;
use aegis_router::{BudgetLimits, EscalationRules, ModelsConfig};
use serde::Deserialize;

use crate::LifecycleError;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunConfig {
    pub provider: ProviderConfig,
    pub models: ModelsConfig,
    pub run: RunSection,
    #[serde(default)]
    pub budget: BudgetSection,
    pub repo: RepoConfig,
    pub identity: IdentityConfig,
    #[serde(default)]
    pub codebase_memory: Option<ExtraServer>,
    /// The gate's Sonar step (#91); without it the step is skipped as not
    /// configured.
    #[serde(default)]
    pub sonar: Option<SonarConfig>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunSection {
    /// Jobs, module maps and the ledger go under here.
    pub state_dir: PathBuf,
    pub pricing: PathBuf,
    /// The `aegis-aci` binary; by default next to the running program.
    #[serde(default)]
    pub aci_bin: Option<PathBuf>,
    #[serde(default = "default_step_timeout")]
    pub step_timeout_s: u64,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    #[serde(default = "default_context_window")]
    pub context_window: u64,
    /// Open PRs as drafts; a person marks them ready.
    #[serde(default = "default_true")]
    pub draft_prs: bool,
    /// Push and open the PR once the gate passes. Off for dry runs (#104):
    /// the run ends where the PR would be opened, hidden tests included.
    #[serde(default = "default_true")]
    pub open_prs: bool,
    /// Run the gate in the profile's container image (#5), not on the
    /// machine.
    #[serde(default)]
    pub sandbox: bool,
    #[serde(default = "default_docker")]
    pub docker: String,
    /// Set inside the gate's containers, e.g. `CARGO_BUILD_JOBS` where the
    /// Docker VM is small (typst needs 2 jobs on 8 GB, #5).
    #[serde(default)]
    pub sandbox_env: std::collections::BTreeMap<String, String>,
    /// Image the egress proxy of the fetch step runs in; any with Python 3.
    #[serde(default = "default_egress_image")]
    pub egress_image: String,
    /// Where the ledger is kept (#22). The database's URL is not part of
    /// the configuration: the command that opens it gets it from outside.
    #[serde(default)]
    pub ledger: LedgerBackend,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LedgerBackend {
    /// `<state_dir>/ledger/<id>.events.jsonl` and `<id>.json`.
    #[default]
    Json,
    /// `aegis-store`'s PostgreSQL tables.
    Postgres,
}

fn default_egress_image() -> String {
    "aegis-sandbox-python".into()
}

fn default_docker() -> String {
    "docker".into()
}

fn default_true() -> bool {
    true
}

fn default_step_timeout() -> u64 {
    1800
}

fn default_max_tokens() -> u32 {
    16_000
}

fn default_context_window() -> u64 {
    200_000
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BudgetSection {
    pub max_cost_usd: f64,
    pub max_model_calls: u32,
}

impl Default for BudgetSection {
    fn default() -> Self {
        let d = BudgetLimits::default();
        Self {
            max_cost_usd: d.max_cost_usd,
            max_model_calls: d.max_model_calls,
        }
    }
}

impl BudgetSection {
    pub fn limits(&self) -> BudgetLimits {
        BudgetLimits {
            max_cost_usd: self.max_cost_usd,
            max_model_calls: self.max_model_calls,
        }
    }

    pub fn rules(&self) -> EscalationRules {
        EscalationRules::default()
    }
}

/// The repo the agent maintains (Phase 1: one per run configuration;
/// many repos come with #24).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepoConfig {
    /// A clone the platform owns; each job gets its own worktree of it.
    pub path: PathBuf,
    pub language: Language,
    /// Branch the patches are based on and PRs target.
    pub base: String,
    /// Name of the repo in file names (module map, jobs).
    pub key: String,
    /// The repo maintainers' own conventions, added to the system prompt.
    #[serde(default)]
    pub conventions: Option<String>,
    /// `RepoConfig` overrides of the language profile.
    #[serde(default)]
    pub overrides: toml::Table,
    /// Packages a patch may add (`policies/allowed-*.toml`).
    #[serde(default)]
    pub allowed_packages: Option<PathBuf>,
    /// How eval runs run an issue's hidden tests (#98): `{tests}` becomes
    /// the tests as separate arguments, `{tests_regex}` one `^(a|b)$`.
    #[serde(default)]
    pub hidden_test: Option<Vec<String>>,
    pub fork: ForkConfig,
}

impl RepoConfig {
    pub fn profile_overrides(&self) -> Result<ProfileOverrides, LifecycleError> {
        self.overrides
            .clone()
            .try_into()
            .map_err(|e: toml::de::Error| LifecycleError::Config(format!("repo.overrides: {e}")))
    }
}

/// The fork the agent pushes to and opens PRs in.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForkConfig {
    pub owner: String,
    pub name: String,
    #[serde(default = "default_api")]
    pub api: String,
    pub token_env: String,
    /// Push to this local bare repo instead of the fork (tests, dry runs).
    #[serde(default)]
    pub push_to_local: Option<PathBuf>,
}

fn default_api() -> String {
    "https://api.github.com".into()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdentityConfig {
    pub name: String,
    pub email: String,
}

/// SonarQube Server or SonarQube Cloud (design §6.1): they differ in
/// `url`, the token and `organization` (Cloud only).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SonarConfig {
    pub url: String,
    /// Variable holding the analysis token; read when the gate runs and
    /// given to the scanner and the Web API only.
    pub token_env: String,
    #[serde(default)]
    pub organization: Option<String>,
    pub project_key: String,
    /// Analyse each patch as a pull request into `repo.base` (Cloud,
    /// Server Developer Edition and up). Off for the Community Build: the
    /// quality gate then reads the project's new-code period.
    #[serde(default = "default_true")]
    pub pull_request: bool,
    #[serde(default = "default_scanner")]
    pub scanner: Vec<String>,
    /// Image of the scanner in sandbox runs; the default is the CLI
    /// scanner's, `""` takes the profile's (needed by the .NET scanner).
    #[serde(default = "default_scanner_image")]
    pub scanner_image: String,
    /// Docker network of the scanner's container, the way to the server.
    #[serde(default = "default_sonar_network")]
    pub network: String,
    #[serde(default = "default_sonar_wait")]
    pub wait_s: u64,
    #[serde(default = "default_sonar_poll")]
    pub poll_s: u64,
    /// Added after the profile's `[sonar.properties]`.
    #[serde(default)]
    pub properties: std::collections::BTreeMap<String, String>,
}

fn default_scanner() -> Vec<String> {
    vec!["sonar-scanner".into()]
}

fn default_scanner_image() -> String {
    "sonarsource/sonar-scanner-cli".into()
}

fn default_sonar_network() -> String {
    "bridge".into()
}

fn default_sonar_wait() -> u64 {
    600
}

fn default_sonar_poll() -> u64 {
    3
}

/// Another MCP server whose tools the agent gets (Codebase-Memory).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtraServer {
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: Vec<(String, String)>,
    pub tools: Vec<String>,
}

impl SonarConfig {
    fn check(&self) -> Result<(), LifecycleError> {
        let bad = |m: String| Err(LifecycleError::Config(format!("sonar: {m}")));
        if !(self.url.starts_with("https://") || self.url.starts_with("http://")) {
            return bad(format!(
                "url `{}` must start with https:// or http://",
                self.url
            ));
        }
        if self.project_key.trim().is_empty() || self.scanner.is_empty() {
            return bad("project_key and scanner must not be empty".into());
        }
        if self
            .organization
            .as_deref()
            .is_some_and(|o| o.trim().is_empty())
        {
            return bad("organization must not be empty when set".into());
        }
        if self.poll_s == 0 {
            return bad("poll_s must be at least 1".into());
        }
        Ok(())
    }

    /// The gate's step, with the token read now from `token_env`.
    pub fn step(&self, cache: Option<PathBuf>) -> Result<aegis_gate::SonarStep, LifecycleError> {
        let token = std::env::var(&self.token_env)
            .ok()
            .filter(|t| !t.is_empty())
            .ok_or_else(|| {
                LifecycleError::Config(format!(
                    "sonar: the token variable `{}` is not set",
                    self.token_env
                ))
            })?;
        Ok(aegis_gate::SonarStep {
            url: self.url.clone(),
            token,
            organization: self.organization.clone(),
            project_key: self.project_key.clone(),
            pull_request: self.pull_request,
            scanner: self.scanner.clone(),
            image: (!self.scanner_image.is_empty()).then(|| self.scanner_image.clone()),
            network: self.network.clone(),
            cache,
            properties: self.properties.clone(),
            wait: std::time::Duration::from_secs(self.wait_s),
            poll: std::time::Duration::from_secs(self.poll_s),
        })
    }
}

impl RunConfig {
    pub fn from_toml(text: &str) -> Result<Self, LifecycleError> {
        let config: Self =
            toml::from_str(text).map_err(|e| LifecycleError::Config(e.to_string()))?;
        config.repo.profile_overrides()?;
        let key_ok = !config.repo.key.is_empty()
            && config
                .repo
                .key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_".contains(c));
        if !key_ok {
            return Err(LifecycleError::Config(format!(
                "repo.key `{}` must be letters, digits, `-` or `_`",
                config.repo.key
            )));
        }
        if let Some(sonar) = &config.sonar {
            sonar.check()?;
        }
        Ok(config)
    }

    /// Reads `path`; relative paths in it are taken from the current
    /// directory, as when `aegis` runs from the repo root.
    pub fn load(path: &Path) -> Result<Self, LifecycleError> {
        let text = std::fs::read_to_string(path).map_err(|e| LifecycleError::io(path, &e))?;
        let cwd = std::env::current_dir().map_err(|e| LifecycleError::io(Path::new("."), &e))?;
        Ok(Self::from_toml(&text)?.with_paths_from(&cwd))
    }

    /// Every path made absolute against `base`. Git and the child processes
    /// run in other directories, where a relative path means something
    /// else.
    pub fn with_paths_from(mut self, base: &Path) -> Self {
        let abs = |p: &mut PathBuf| {
            if p.is_relative() {
                *p = base.join(&*p);
            }
        };
        abs(&mut self.run.state_dir);
        abs(&mut self.run.pricing);
        if let Some(p) = self.run.aci_bin.as_mut() {
            abs(p);
        }
        abs(&mut self.repo.path);
        if let Some(p) = self.repo.allowed_packages.as_mut() {
            abs(p);
        }
        if let Some(p) = self.repo.fork.push_to_local.as_mut() {
            abs(p);
        }
        self
    }

    pub fn aci_bin(&self) -> PathBuf {
        self.run.aci_bin.clone().unwrap_or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|d| d.join("aegis-aci")))
                .unwrap_or_else(|| PathBuf::from("aegis-aci"))
        })
    }

    /// Fails unless [`Self::aci_bin`] is a file, saying where it looked and
    /// how to build it. Checked before any issue starts: without it every
    /// issue stops at its first step and still lands in the ledger (#113).
    pub fn check_aci_bin(&self) -> Result<(), LifecycleError> {
        let bin = self.aci_bin();
        if bin.is_file() {
            return Ok(());
        }
        let (from, build) = if self.run.aci_bin.is_some() {
            ("`run.aci_bin` in the config", "cargo build -p aegis-aci")
        } else if bin.parent().and_then(Path::file_name) == Some("release".as_ref()) {
            (
                "the default, next to `aegis`",
                "cargo build --release -p aegis-aci",
            )
        } else {
            ("the default, next to `aegis`", "cargo build -p aegis-aci")
        };
        Err(LifecycleError::Config(format!(
            "no `aegis-aci` at `{}` ({from}); build it with `{build}` or set `run.aci_bin`",
            bin.display()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const POC: &str = include_str!("../../../config/poc.toml");

    fn with_sonar(table: &str) -> Result<RunConfig, LifecycleError> {
        RunConfig::from_toml(&format!("{POC}\n[sonar]\n{table}"))
    }

    #[test]
    fn sonar_cloud_and_server_differ_in_url_token_and_organization() {
        let cloud = with_sonar(
            r#"url = "https://sonarcloud.io"
token_env = "SONAR_TOKEN"
organization = "acme"
project_key = "acme_scrapy""#,
        )
        .unwrap()
        .sonar
        .unwrap();
        assert_eq!(cloud.organization.as_deref(), Some("acme"));
        assert!(cloud.pull_request);
        assert_eq!(cloud.scanner, vec!["sonar-scanner"]);
        let server = with_sonar(
            r#"url = "https://sonar.example.com"
token_env = "SONAR_TOKEN"
project_key = "scrapy"
pull_request = false"#,
        )
        .unwrap()
        .sonar
        .unwrap();
        assert_eq!(server.organization, None);
        assert!(!server.pull_request);
    }

    #[test]
    fn the_poc_repos_say_where_the_reproduction_test_goes() {
        let typst = include_str!("../../../config/typst.toml");
        for (toml, place) in [(POC, "tests/test_"), (typst, "tests/suite/")] {
            let repo = RunConfig::from_toml(toml).unwrap().repo;
            let conventions = repo.conventions.unwrap_or_default();
            assert!(conventions.contains(place), "{}: {conventions}", repo.key);
        }
    }

    #[test]
    fn sonar_settings_are_checked() {
        for bad in [
            "url = \"sonar.example.com\"\ntoken_env = \"T\"\nproject_key = \"p\"",
            "url = \"https://s\"\ntoken_env = \"T\"\nproject_key = \"\"",
            "url = \"https://s\"\ntoken_env = \"T\"\nproject_key = \"p\"\norganization = \"\"",
            "url = \"https://s\"\ntoken_env = \"T\"\nproject_key = \"p\"\npoll_s = 0",
        ] {
            assert!(with_sonar(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_sonar_token_is_read_from_its_variable() {
        let cfg = with_sonar(
            r#"url = "https://s"
token_env = "AEGIS_TEST_SURELY_UNSET_SONAR_TOKEN"
project_key = "p""#,
        )
        .unwrap()
        .sonar
        .unwrap();
        let err = cfg.step(None).unwrap_err().to_string();
        assert!(err.contains("AEGIS_TEST_SURELY_UNSET_SONAR_TOKEN"), "{err}");
        // Any variable that is set will do to see it read.
        let cfg = SonarConfig {
            token_env: "PATH".into(),
            ..cfg
        };
        let step = cfg.step(None).unwrap();
        assert_eq!(step.token, std::env::var("PATH").unwrap());
        assert_eq!(step.image.as_deref(), Some("sonarsource/sonar-scanner-cli"));
    }
}
