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
}
