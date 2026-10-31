//! The run configuration (`config/poc.toml`, `config/customer-jp.toml`):
//! provider and models as before, plus the target repo, where the job's
//! files go, budget and the fork that receives PRs (design §1: the two
//! configurations differ only in files, never in code).

use std::path::{Path, PathBuf};

use aegis_lang::{Language, ProfileOverrides, Profiles, RepoProfiles, check_dir};
use aegis_provider::ProviderConfig;
use aegis_router::{BudgetLimits, EscalationRules, ModelsConfig};
use serde::{Deserialize, Serialize};

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
    /// Memory, CPUs, processes and `/tmp` of each sandbox container (#320);
    /// a profile's `sandbox.memory_mb` and `sandbox.cpus` go in their place.
    #[serde(default)]
    pub sandbox_limits: aegis_gate::Limits,
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

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
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

/// The repo the agent maintains: the `[repo]` section of a CLI run, or
/// built from the registry (#24, [`crate::Modes`]).
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
    /// Directories that go by another profile (monorepo, ADR 0004 §1,
    /// #71): `[[repo.dirs]]` with `path`, `language` and `overrides`.
    /// `language` above is then the rest of the repo's.
    #[serde(default)]
    pub dirs: Vec<DirConfig>,
    /// Packages a patch may add (`policies/allowed-*.toml`).
    #[serde(default)]
    pub allowed_packages: Option<PathBuf>,
    /// More packages a patch may add, this repo's own allow-list.
    #[serde(default)]
    pub allowed_package_names: Vec<String>,
    /// Licenses this repo's dependencies may have, in place of
    /// `policies/licenses.toml`, for the audit step (`{licenses}`, #159).
    #[serde(default)]
    pub licenses: Option<Vec<String>>,
    /// Advisories (`RUSTSEC-…`, `GHSA-…`) this repo's audit lets through.
    #[serde(default)]
    pub ignored_advisories: Vec<String>,
    /// How eval runs run an issue's hidden tests (#98): `{tests}` becomes
    /// the tests as separate arguments, `{tests_regex}` one `^(a|b)$`.
    #[serde(default)]
    pub hidden_test: Option<Vec<String>>,
    pub fork: ForkConfig,
}

/// A directory of the repo with its own language profile (#71).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirConfig {
    /// Relative to the repo root, e.g. `web` or `services/api`.
    pub path: String,
    pub language: Language,
    /// The profile's overrides, as `repo.overrides`; commands run in
    /// `path`, and paths in them are relative to it.
    #[serde(default)]
    pub overrides: toml::Table,
    /// How eval runs run hidden tests of this directory, as
    /// `repo.hidden_test`; it runs in `path` (#199).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hidden_test: Option<Vec<String>>,
}

impl DirConfig {
    pub fn profile_overrides(&self) -> Result<ProfileOverrides, LifecycleError> {
        self.overrides
            .clone()
            .try_into()
            .map_err(|e: toml::de::Error| {
                LifecycleError::Config(format!("repo.dirs `{}`: overrides: {e}", self.path))
            })
    }
}

impl RepoConfig {
    /// The repo's profile and its directories' own, resolved.
    pub fn profiles(&self, builtin: &Profiles) -> Result<RepoProfiles, LifecycleError> {
        let root = builtin
            .get(self.language)?
            .resolve(&self.profile_overrides()?)?;
        let dirs = self
            .dirs
            .iter()
            .map(|d| {
                let profile = builtin.get(d.language)?.resolve(&d.profile_overrides()?)?;
                Ok((d.path.clone(), profile))
            })
            .collect::<Result<Vec<_>, LifecycleError>>()?;
        Ok(RepoProfiles::new(root, dirs)?)
    }

    /// The directories are plain and named once, their overrides read.
    fn check_dirs(&self) -> Result<(), LifecycleError> {
        for (i, dir) in self.dirs.iter().enumerate() {
            check_dir(&dir.path)
                .map_err(|e| LifecycleError::Config(format!("repo.dirs path {e}")))?;
            if self.dirs[..i].iter().any(|d| d.path == dir.path) {
                return Err(LifecycleError::Config(format!(
                    "repo.dirs: `{}` is given twice",
                    dir.path
                )));
            }
            dir.profile_overrides()?;
        }
        Ok(())
    }

    /// The repo's policy for the gate's audit step (#159).
    pub fn audit_policy(&self) -> aegis_gate::AuditPolicy {
        aegis_gate::AuditPolicy {
            licenses: self.licenses.clone(),
            ignored_advisories: self.ignored_advisories.clone(),
        }
    }

    pub fn profile_overrides(&self) -> Result<ProfileOverrides, LifecycleError> {
        self.overrides
            .clone()
            .try_into()
            .map_err(|e: toml::de::Error| LifecycleError::Config(format!("repo.overrides: {e}")))
    }
}

/// Where the fork is (#252).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ForkPlatform {
    #[default]
    GitHub,
    GitLab,
}

/// The fork the agent pushes to and opens PRs in: a GitHub repository, or
/// a GitLab project (`owner` is then its namespace, subgroups included,
/// and `api` the instance's API root).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForkConfig {
    #[serde(default, skip_serializing_if = "is_github")]
    pub platform: ForkPlatform,
    pub owner: String,
    pub name: String,
    #[serde(default = "default_api")]
    pub api: String,
    pub token_env: String,
    /// Push to this local bare repo instead of the fork (tests, dry runs).
    #[serde(default)]
    pub push_to_local: Option<PathBuf>,
}

fn is_github(platform: &ForkPlatform) -> bool {
    *platform == ForkPlatform::GitHub
}

impl ForkConfig {
    /// `owner/name`: the repository or project path.
    pub fn path(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SonarConfig {
    pub url: String,
    /// Variable holding the analysis token; read when the gate runs and
    /// given to the scanner and the Web API only. The server reads the
    /// secret of this name instead and sets [`Self::token`] (#30).
    pub token_env: String,
    /// The token, when its owner read it from a secret store; never in a
    /// file.
    #[serde(skip)]
    pub token: Option<aegis_secrets::Secret>,
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

/// Another MCP server next to the ACI (Codebase-Memory): the agent gets
/// its `tools`, maybe none, and the platform calls it too (#203).
/// `{index_dir}` in `args` and `env` values is the index directory,
/// `<state_dir>/index`, shared by every repo.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtraServer {
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: Vec<(String, String)>,
    #[serde(default)]
    pub tools: Vec<String>,
}

impl ExtraServer {
    /// `program` taken from `base` when it is a relative path; a bare name
    /// stays one, to be looked up on `PATH`.
    pub(crate) fn program_from(&self, base: &Path) -> String {
        let program = Path::new(&self.program);
        if program.is_relative() && program.components().count() > 1 {
            base.join(program).display().to_string()
        } else {
            self.program.clone()
        }
    }
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

    /// The gate's step, with [`Self::token`] or else the token read now
    /// from `token_env`.
    pub fn step(&self, cache: Option<PathBuf>) -> Result<aegis_gate::SonarStep, LifecycleError> {
        let token = self
            .token
            .as_ref()
            .map(|t| t.expose().to_owned())
            .or_else(|| std::env::var(&self.token_env).ok())
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
        config.check()
    }

    /// What deserializing alone does not check. A registry slug is a valid
    /// key: `.` is allowed past the first character.
    pub(crate) fn check(self) -> Result<Self, LifecycleError> {
        self.repo.profile_overrides()?;
        self.repo.check_dirs()?;
        let key = &self.repo.key;
        let key_ok = key.starts_with(|c: char| c.is_ascii_alphanumeric())
            && key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c));
        if !key_ok {
            return Err(LifecycleError::Config(format!(
                "repo.key `{key}` must be letters, digits, `-`, `_` or `.`, starting with a letter or digit"
            )));
        }
        if let Some(sonar) = &self.sonar {
            sonar.check()?;
        }
        let fork = &self.repo.fork;
        if fork.platform == ForkPlatform::GitLab && fork.api == default_api() {
            return Err(LifecycleError::Config(
                "repo.fork.api: a GitLab fork needs its instance's API root \
                 (`https://gitlab.com/api/v4` or a self-hosted one)"
                    .into(),
            ));
        }
        self.repo
            .audit_policy()
            .check_ids()
            .map_err(LifecycleError::Config)?;
        Ok(self)
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
        if let Some(cbm) = self.codebase_memory.as_mut() {
            cbm.program = cbm.program_from(base);
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

    /// Fails unless the Codebase-Memory program, when configured, is a file
    /// or found on `PATH`; checked with [`Self::check_aci_bin`], for the
    /// same reason.
    pub fn check_codebase_memory(&self) -> Result<(), LifecycleError> {
        let Some(cbm) = &self.codebase_memory else {
            return Ok(());
        };
        let program = Path::new(&cbm.program);
        let found = if program.components().count() > 1 {
            program.is_file()
        } else {
            std::env::var_os("PATH")
                .is_some_and(|path| std::env::split_paths(&path).any(|d| d.join(program).is_file()))
        };
        if found {
            return Ok(());
        }
        Err(LifecycleError::Config(format!(
            "no Codebase-Memory program `{}` (`codebase_memory.program`); install \
             codebase-memory-mcp (eval/README.md, \"Codebase-Memory\") or remove `[codebase_memory]`",
            cbm.program
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
    fn a_gitlab_fork_names_its_api_root() {
        let gitlab = |api: &str| {
            RunConfig::from_toml(&POC.replacen(
                "[repo.fork]",
                &format!("[repo.fork]\nplatform = \"gitlab\"{api}"),
                1,
            ))
        };
        let err = gitlab("").unwrap_err().to_string();
        assert!(err.contains("a GitLab fork needs"), "{err}");
        let cfg = gitlab("\napi = \"https://gitlab.com/api/v4\"").unwrap();
        assert_eq!(cfg.repo.fork.platform, ForkPlatform::GitLab);
        assert_eq!(
            RunConfig::from_toml(POC).unwrap().repo.fork.platform,
            ForkPlatform::GitHub
        );
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

    fn with_repo(key: &str, value: toml::Value) -> Result<RunConfig, LifecycleError> {
        let mut cfg: toml::Table = toml::from_str(POC).unwrap();
        cfg["repo"]
            .as_table_mut()
            .unwrap()
            .insert(key.into(), value);
        RunConfig::from_toml(&toml::to_string(&cfg).unwrap())
    }

    fn strings(v: &[&str]) -> toml::Value {
        toml::Value::Array(v.iter().map(|s| (*s).into()).collect())
    }

    #[test]
    fn the_repo_s_licenses_and_ignored_advisories_must_be_identifiers() {
        let cfg = with_repo("licenses", strings(&["MIT", "Apache-2.0"])).unwrap();
        assert_eq!(
            cfg.repo.audit_policy().licenses,
            Some(vec!["MIT".into(), "Apache-2.0".into()])
        );
        assert!(with_repo("ignored_advisories", strings(&["RUSTSEC-2024-0001"])).is_ok());
        for (key, bad) in [
            ("licenses", strings(&[])),
            ("licenses", strings(&["MIT,GPL-3.0"])),
            ("licenses", strings(&["--all"])),
            ("ignored_advisories", strings(&["a b"])),
        ] {
            let err = with_repo(key, bad.clone()).unwrap_err().to_string();
            assert!(err.contains("repo."), "{key} = {bad}: {err}");
        }
    }

    #[test]
    fn the_poc_repos_have_the_call_graph_but_not_the_agent() {
        let typst = include_str!("../../../config/typst.toml");
        for toml in [POC, typst] {
            let cbm = RunConfig::from_toml(toml).unwrap().codebase_memory.unwrap();
            assert_eq!(cbm.program, "codebase-memory-mcp");
            assert!(cbm.tools.is_empty(), "{:?}", cbm.tools);
            assert_eq!(cbm.env, [("HOME".into(), "{index_dir}".into())]);
        }
    }

    #[test]
    fn a_relative_codebase_memory_path_is_taken_from_the_base() {
        let base = Path::new("/srv/aegis");
        let program = |p: &str| {
            let mut cfg = RunConfig::from_toml(POC).unwrap();
            cfg.codebase_memory.as_mut().unwrap().program = p.into();
            cfg.with_paths_from(base).codebase_memory.unwrap().program
        };
        assert_eq!(
            program(".workspace/tools/cbm/codebase-memory-mcp"),
            "/srv/aegis/.workspace/tools/cbm/codebase-memory-mcp"
        );
        assert_eq!(program("codebase-memory-mcp"), "codebase-memory-mcp");
        assert_eq!(program("/opt/cbm"), "/opt/cbm");
    }

    #[test]
    fn a_missing_codebase_memory_program_is_named() {
        let mut cfg = RunConfig::from_toml(POC).unwrap();
        cfg.codebase_memory.as_mut().unwrap().program =
            "aegis-surely-not-on-path-codebase-memory".into();
        let err = cfg.check_codebase_memory().unwrap_err().to_string();
        assert!(
            err.contains("aegis-surely-not-on-path-codebase-memory"),
            "{err}"
        );

        // Any file will do: the check only looks for it.
        let exe = std::env::current_exe().unwrap();
        cfg.codebase_memory.as_mut().unwrap().program = exe.display().to_string();
        assert!(cfg.check_codebase_memory().is_ok());
        cfg.codebase_memory = None;
        assert!(cfg.check_codebase_memory().is_ok());
    }

    fn with_dirs(dirs: &str) -> Result<RunConfig, LifecycleError> {
        RunConfig::from_toml(&POC.replacen("[repo.fork]", &format!("{dirs}\n[repo.fork]"), 1))
    }

    #[test]
    fn a_monorepo_gives_directories_their_own_profile() {
        let cfg = with_dirs(
            r#"[[repo.dirs]]
path = "web"
language = "typescript"
[repo.dirs.overrides.gate]
test = ["npm", "test"]
[[repo.dirs]]
path = "services/api"
language = "go""#,
        )
        .unwrap();
        let profiles = cfg.repo.profiles(&Profiles::builtin().unwrap()).unwrap();
        let language = |p: &str| profiles.part_of(p).profile.language();
        assert_eq!(language("web/src/app.ts"), Language::TypeScript);
        assert_eq!(language("services/api/main.go"), Language::Go);
        assert_eq!(language("scrapy/http.py"), cfg.repo.language);
        let web = &profiles.parts()[1].profile;
        assert_eq!(web.gate.test.render(&[]).unwrap(), ["npm", "test"]);
    }

    #[test]
    fn a_monorepo_s_directories_are_checked() {
        for (bad, says) in [
            (
                "[[repo.dirs]]\npath = \"../web\"\nlanguage = \"go\"",
                "plain relative",
            ),
            (
                "[[repo.dirs]]\npath = \"web\"\nlanguage = \"go\"\n\
                 [[repo.dirs]]\npath = \"web\"\nlanguage = \"go\"",
                "twice",
            ),
            (
                "[[repo.dirs]]\npath = \"web\"\nlanguage = \"go\"\n\
                 [repo.dirs.overrides]\nnope = 1",
                "repo.dirs `web`",
            ),
        ] {
            let err = with_dirs(bad).unwrap_err().to_string();
            assert!(err.contains(says), "{bad}: {err}");
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
        let cfg = SonarConfig {
            token: Some(aegis_secrets::Secret::new("from-the-store")),
            ..cfg
        };
        assert_eq!(cfg.step(None).unwrap().token, "from-the-store");
    }
}
