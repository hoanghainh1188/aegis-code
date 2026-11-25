//! The run configuration (`config/poc.toml`, `config/customer-jp.toml`):
//! provider and models as before, plus the target repo, where the job's
//! files go, budget and the fork that receives PRs (design §1: the two
//! configurations differ only in files, never in code).

use std::path::{Path, PathBuf};

use aegis_lang::{Language, ProfileOverrides, Profiles, RepoProfiles, check_dir};
use aegis_provider::{EmbeddingConfig, ModelPolicy, ProviderConfig, ResidencyConfig};
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
    /// Where its code may go (#592): model prefixes or a local provider
    /// only. Without it, any model and endpoint.
    #[serde(default)]
    pub residency: ResidencyConfig,
    /// Incident & Domain Memory (#655): read by the server only.
    #[serde(default)]
    pub memory: MemorySection,
}

/// `[memory]` of a run configuration or a mode.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemorySection {
    /// The embedding model of semantic recall (#655); without it, recall
    /// is lexical only (paths, full text, substrings).
    #[serde(default)]
    pub embedding: Option<EmbeddingConfig>,
}

/// Bedrock's region for `customer-jp` (design §7).
const JAPAN_REGION: &str = "ap-northeast-1";

impl MemorySection {
    /// Whether the embedding endpoint keeps to `residency`, and for a
    /// Japan configuration (`japan`) also stays in Japan: Bedrock in
    /// Tokyo, or a `local` endpoint on the deployment's own network.
    pub fn check(&self, residency: &ResidencyConfig, japan: bool) -> Result<(), LifecycleError> {
        let Some(embedding) = &self.embedding else {
            return Ok(());
        };
        embedding.check().map_err(LifecycleError::Config)?;
        if japan {
            match &embedding.provider {
                ProviderConfig::Bedrock { region, .. } if region == JAPAN_REGION => {}
                ProviderConfig::Local { .. } => {}
                _ => {
                    return Err(LifecycleError::Config(format!(
                        "customer-jp guard (design §7): [memory.embedding] must be Bedrock in \
                         `{JAPAN_REGION}` or a `local` endpoint"
                    )));
                }
            }
            if let ProviderConfig::Local { .. } = &embedding.provider {
                // Its host is checked as `local_only` checks one.
                let local = ResidencyConfig {
                    local_only: true,
                    local_hosts: residency.local_hosts.clone(),
                    ..ResidencyConfig::default()
                };
                return local
                    .check_embedding(&embedding.provider)
                    .map_err(|e| LifecycleError::Config(format!("customer-jp guard: {e}")));
            }
            return Ok(());
        }
        residency
            .check_embedding(&embedding.provider)
            .map_err(LifecycleError::Config)
    }
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
    /// The OCI runtime of every container that runs the repo's code (#519):
    /// the gate's steps, `run_tests`, the formatter. `runsc` for gVisor;
    /// the daemon must know it (`runtimes` in its `daemon.json`), and the
    /// Docker proxy allows only `runc` and `runsc` (ADR 0010). Unset is the
    /// daemon's default.
    #[serde(default)]
    pub sandbox_runtime: Option<String>,
    /// Image the egress proxy of the fetch step runs in; any with Python 3.
    #[serde(default = "default_egress_image")]
    pub egress_image: String,
    /// How long the egress proxy may take to listen once started (#326).
    #[serde(default = "default_egress_ready_timeout")]
    pub egress_ready_timeout_s: u64,
    /// Where the ledger is kept (#22). The database's URL is not part of
    /// the configuration: the command that opens it gets it from outside.
    #[serde(default)]
    pub ledger: LedgerBackend,
    /// Run the repo's containers (the gate's steps, `run_tests`, the
    /// formatter, sidecars) on a remote execution host's Docker, through
    /// its `aegis-docker-proxy --listen-tls` (#664); unset, on this host's.
    #[serde(default)]
    pub remote_execution: Option<RemoteExecution>,
    /// Keep the repo's caches in an object store too (#665), so a fresh
    /// worker or execution host starts from them instead of cold.
    #[serde(default)]
    pub remote_cache: Option<RemoteCache>,
    /// Get each job a pod of its own from an execution pool (#665) and give
    /// it back when the job ends: [`RemoteExecution`] per job, from the
    /// pool's commands rather than this file.
    #[serde(default)]
    pub execution_pool: Option<ExecutionPool>,
}

/// `[run.execution_pool]` (#665): two commands of the deployment's own
/// (a script over Kubernetes, Firecracker, Nomad, a fixed set of hosts:
/// the platform does not know which) that hand a job an execution pod
/// and take it back.
///
/// Both run on the worker with `AEGIS_JOB_ID`, `AEGIS_JOB_DIR` and
/// `AEGIS_STATE_DIR` set, no stdin, and at most `timeout_s` each.
/// - `provision` prints one JSON object on stdout: `pod` (the pod's name
///   for `release`), `docker_host`, `tls_cert`, `tls_key` and `tls_ca`, as
///   in `[run.remote_execution]`. A job that goes on from its checkpoint
///   after a worker failed has `AEGIS_POD` set to the pod it had: the
///   command hands that one back while it still runs, or a new one.
/// - `release` has `AEGIS_POD` set, when the job ends however it ends. A
///   worker that crashes does not release its pod: the job's next worker
///   gets it back, and the pool reaps any pod no job came back for.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionPool {
    /// The command and its arguments.
    pub provision: Vec<String>,
    pub release: Vec<String>,
    #[serde(default = "default_pool_timeout_s")]
    pub timeout_s: u64,
}

fn default_pool_timeout_s() -> u64 {
    600
}

impl ExecutionPool {
    pub fn check(&self) -> Result<(), String> {
        for (name, argv) in [("provision", &self.provision), ("release", &self.release)] {
            if argv.first().is_none_or(|p| p.trim().is_empty()) {
                return Err(format!("run.execution_pool.{name} names no command"));
            }
        }
        if self.timeout_s == 0 {
            return Err("run.execution_pool.timeout_s is 0".into());
        }
        Ok(())
    }
}

/// `[run.remote_cache]` (#665): an S3 bucket (AWS, MinIO, any S3 API)
/// that holds one archive per repo part of the caches under
/// `state_dir/cache/<repo>/`. The worker moves them, before the job and
/// after it, never a container: the gate's steps keep `--network none`.
/// Credentials come from the AWS chain (`AWS_ACCESS_KEY_ID`, a profile, a
/// role), never from this file.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteCache {
    /// `s3://<bucket>[/<prefix>]`.
    pub url: String,
    /// The S3 API's root for MinIO or another store (`https://minio:9000`);
    /// unset, AWS's own for `region`.
    #[serde(default)]
    pub endpoint: Option<String>,
    #[serde(default = "default_cache_region")]
    pub region: String,
    /// The language profiles whose caches go to the store; empty, every
    /// one. A profile whose cache is cheap to rebuild can stay local.
    #[serde(default)]
    pub languages: Vec<aegis_lang::Language>,
    /// A repo part's caches go up at most this often: a job's gate
    /// refreshes them every time, and an archive can be gigabytes.
    #[serde(default = "default_push_interval")]
    pub push_interval_s: u64,
}

fn default_cache_region() -> String {
    "us-east-1".into()
}

fn default_push_interval() -> u64 {
    3600
}

impl RemoteCache {
    /// The bucket and the key prefix (without a trailing `/`) of `url`.
    pub fn bucket_and_prefix(&self) -> Result<(&str, &str), String> {
        let rest = self
            .url
            .strip_prefix("s3://")
            .ok_or_else(|| format!("run.remote_cache.url `{}` is not s3://<bucket>", self.url))?;
        let (bucket, prefix) = rest.split_once('/').unwrap_or((rest, ""));
        let prefix = prefix.trim_end_matches('/');
        let bucket_ok = (3..=63).contains(&bucket.len())
            && bucket
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || ".-".contains(c));
        if !bucket_ok {
            return Err(format!(
                "run.remote_cache.url `{}`: `{bucket}` is not an S3 bucket name",
                self.url
            ));
        }
        let prefix_ok = prefix.split('/').all(|s| {
            !s.is_empty()
                && s != "."
                && s != ".."
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
        });
        if !prefix.is_empty() && !prefix_ok {
            return Err(format!(
                "run.remote_cache.url `{}`: the prefix `{prefix}` must be plain path segments",
                self.url
            ));
        }
        Ok((bucket, prefix))
    }

    pub fn check(&self) -> Result<(), String> {
        self.bucket_and_prefix()?;
        if let Some(endpoint) = &self.endpoint
            && !(endpoint.starts_with("https://") || endpoint.starts_with("http://"))
        {
            return Err(format!(
                "run.remote_cache.endpoint `{endpoint}` is not an http(s) URL"
            ));
        }
        if self.region.is_empty() {
            return Err("run.remote_cache.region is empty".into());
        }
        Ok(())
    }

    /// Whether `language`'s caches go to the store.
    pub fn covers(&self, language: aegis_lang::Language) -> bool {
        self.languages.is_empty() || self.languages.contains(&language)
    }
}

/// `[run.remote_execution]` (#664): the execution host's Docker proxy and
/// this worker's certificate for it. The host mounts the run's
/// `state_dir` at the same path as here (shared storage): a container
/// there bind-mounts the job's tree by the path the worker gives.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteExecution {
    /// `tcp://<host>:<port>` of the host's `--listen-tls`.
    pub docker_host: String,
    /// This worker's certificate and key, and the host's CA (PEM).
    pub tls_cert: PathBuf,
    pub tls_key: PathBuf,
    pub tls_ca: PathBuf,
}

impl RemoteExecution {
    /// A `tcp://` host the `docker` client takes, and paths a shell script
    /// can quote.
    pub fn check(&self) -> Result<(), String> {
        let host = self
            .docker_host
            .strip_prefix("tcp://")
            .filter(|h| {
                !h.is_empty()
                    && h.chars()
                        .all(|c| c.is_ascii_alphanumeric() || "-.:[]".contains(c))
            })
            .ok_or_else(|| {
                format!(
                    "run.remote_execution.docker_host `{}` is not tcp://<host>:<port>",
                    self.docker_host
                )
            })?;
        if !host.contains(':') {
            return Err(format!(
                "run.remote_execution.docker_host `{}` needs its port",
                self.docker_host
            ));
        }
        for (name, path) in [
            ("tls_cert", &self.tls_cert),
            ("tls_key", &self.tls_key),
            ("tls_ca", &self.tls_ca),
        ] {
            let text = path.to_string_lossy();
            if !path.is_absolute() || text.contains(['\'', '\n', '\0']) {
                return Err(format!(
                    "run.remote_execution.{name} `{text}` must be an absolute path without `'`"
                ));
            }
        }
        Ok(())
    }
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

fn default_egress_ready_timeout() -> u64 {
    aegis_gate::Egress::DEFAULT_READY_TIMEOUT.as_secs()
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
    /// Core + Expert calls for a job. Without it, the repo's language
    /// profile sets the cap, or else the platform's 40 (#471, ADR 0014).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_model_calls: Option<u32>,
}

impl Default for BudgetSection {
    fn default() -> Self {
        Self {
            max_cost_usd: BudgetLimits::default().max_cost_usd,
            max_model_calls: None,
        }
    }
}

impl BudgetSection {
    /// The caps of a job whose language profile sets `language_calls`.
    pub fn limits(&self, language_calls: Option<u32>) -> BudgetLimits {
        BudgetLimits {
            max_cost_usd: self.max_cost_usd,
            max_model_calls: self
                .max_model_calls
                .or(language_calls)
                .unwrap_or(BudgetLimits::default().max_model_calls),
        }
    }

    /// Whether `spent_usd` leaves no room under the cost cap.
    pub fn cost_spent(&self, spent_usd: f64) -> bool {
        self.limits(None).is_spent(spent_usd)
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
    /// Patch scope limits and controls (#359).
    #[serde(default)]
    pub patch_scope: Option<crate::registry::PatchScopeConfig>,
    /// Secrets the gate's `secrets` step lets through (#509), by path and
    /// kind, each with its reason: `[[repo.secrets.allow]]`.
    #[serde(default)]
    pub secrets: aegis_gate::SecretPolicy,
    /// Which triaged tickets get a read-only diagnosis instead of the fix
    /// loop (#451): `[repo.triage_lane]`; off by default.
    #[serde(default)]
    pub triage_lane: crate::TriageLane,
    /// Services the test steps reach, such as PostgreSQL (#490, ADR
    /// 0013): `[[repo.sidecars]]`. They run only with `run.sandbox`.
    #[serde(default)]
    pub sidecars: Vec<aegis_gate::SidecarSpec>,
    /// The language of the PR description, review replies and diagnosis
    /// reports, the platform's words and the model's (#588): `en`, `ja` or
    /// `vi`; `en` when unset.
    #[serde(default)]
    pub locale: aegis_pr::Locale,
    /// Where the agent pushes and opens PRs. On the app path (ADR 0017)
    /// the upstream repo itself, with no `token_env`.
    pub fork: ForkConfig,
    /// Set on the app path (`[settings.app]`, ADR 0017): tokens come from
    /// the instance's GitHub App, minted per use, instead of
    /// `fork.token_env`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<AppConnection>,
}

/// A repo connected through the instance's GitHub App (`[settings.app]`,
/// ADR 0017 §1): no secret, only the API root when it is not the
/// instance's.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppConnection {
    /// GitHub Enterprise Server's API root; the instance's app `api` when
    /// left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api: Option<String>,
}

/// `owner` and `name` of a GitHub repo's HTTPS URL
/// (`https://github.com/acme/shop`, `.git` allowed), plain names only.
pub fn upstream_of(url: &str) -> Option<(String, String)> {
    let rest = url.strip_prefix("https://")?;
    let mut parts = rest.trim_end_matches('/').split('/');
    let (_host, owner, name) = (parts.next()?, parts.next()?, parts.next()?);
    let name = name.strip_suffix(".git").unwrap_or(name);
    let plain = |s: &str| {
        !s.is_empty()
            && s != "."
            && s != ".."
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
    };
    (parts.next().is_none() && plain(owner) && plain(name))
        .then(|| (owner.to_owned(), name.to_owned()))
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

    /// The gate's sidecars (#490): none without any declared or without
    /// the sandbox (`RunConfig::check` refuses that pair).
    pub fn gate_sidecars(&self, run: &RunSection) -> Option<aegis_gate::Sidecars> {
        (run.sandbox && !self.sidecars.is_empty()).then(|| aegis_gate::Sidecars {
            docker: run.docker.clone(),
            specs: self.sidecars.clone(),
        })
    }

    /// The repo's policy for the gate's audit step (#159).
    pub fn audit_policy(&self) -> aegis_gate::AuditPolicy {
        aegis_gate::AuditPolicy {
            licenses: self.licenses.clone(),
            ignored_advisories: self.ignored_advisories.clone(),
        }
    }

    /// The repo's policy for patch scope (#359).
    pub fn scope_policy(&self) -> aegis_gate::ScopePolicy {
        let p = self.patch_scope.as_ref().cloned().unwrap_or_default();
        aegis_gate::ScopePolicy {
            max_files: p.max_files,
            max_dirs: p.max_dirs,
            allow_test_deletion: p.allow_test_deletion,
            allow_test_skips: p.allow_test_skips,
            enforce: p.enforce,
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
    /// Bitbucket Cloud (#634).
    Bitbucket,
    /// Bitbucket Data Center or Server, self-hosted (#635).
    #[serde(rename = "bitbucket-dc")]
    BitbucketDataCenter,
    /// Azure DevOps Services or Server (Azure Repos, #636).
    #[serde(rename = "azure-devops")]
    AzureDevOps,
}

/// The fork the agent pushes to and opens PRs in: a GitHub repository, a
/// GitLab project (`owner` is then its namespace, subgroups included, and
/// `api` the instance's API root), or a Bitbucket Cloud repository
/// (`owner` is its workspace, `api` [`aegis_pr::BITBUCKET_API`]), or a
/// Bitbucket Data Center repository (`owner` is its project key, `name`
/// its slug, `api` the instance's `…/rest/api/1.0`), or an Azure Repos
/// repository (`owner` is its project, `api` the organization or
/// collection URL, `https://dev.azure.com/<org>`).
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
    /// An Azure DevOps fork reached as a Microsoft Entra service principal
    /// (#636): `token_env` then names its client secret, exchanged for an
    /// access token when a job starts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entra: Option<EntraConfig>,
}

/// A Microsoft Entra service principal (#636).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntraConfig {
    /// The tenant's ID or domain.
    pub tenant: String,
    /// The application (client) ID.
    pub client_id: String,
    /// Where tokens are issued; [`aegis_pr::ENTRA_AUTHORITY`] by default.
    #[serde(default = "default_authority")]
    pub authority: String,
}

fn default_authority() -> String {
    aegis_pr::ENTRA_AUTHORITY.into()
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

pub(crate) fn default_api() -> String {
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

/// `residency`'s policy, refused unless `provider` and the model of every
/// tier of `models` keep to it.
pub(crate) fn residency_policy(
    residency: &ResidencyConfig,
    provider: &ProviderConfig,
    models: &ModelsConfig,
) -> Result<ModelPolicy, LifecycleError> {
    let ids = [&models.light, &models.core, &models.expert].map(|m| m.id.as_str());
    residency
        .check(provider, ids)
        .map_err(LifecycleError::Config)
}

impl RunConfig {
    /// The job's caps: `[budget]`, with the call cap of the repo's
    /// language profile (its `language`, not a `[[repo.dirs]]` one) when
    /// `[budget]` has none (#471).
    pub fn budget_limits(&self, builtin: &Profiles) -> Result<BudgetLimits, LifecycleError> {
        let profile = builtin.get(self.repo.language)?;
        Ok(self.budget.limits(profile.budget.max_model_calls))
    }

    /// The residency policy its provider is built with, once `provider`
    /// and every tier's model keep to `[residency]` (#592).
    pub fn model_policy(&self) -> Result<ModelPolicy, LifecycleError> {
        residency_policy(&self.residency, &self.provider, &self.models)
    }

    pub fn from_toml(text: &str) -> Result<Self, LifecycleError> {
        let config: Self =
            toml::from_str(text).map_err(|e| LifecycleError::Config(e.to_string()))?;
        config.check()
    }

    /// What deserializing alone does not check. A registry slug is a valid
    /// key: `.` is allowed past the first character.
    pub(crate) fn check(self) -> Result<Self, LifecycleError> {
        self.model_policy()?;
        let japan = matches!(
            self.provider,
            ProviderConfig::Bedrock {
                require_jp_profile: true,
                ..
            }
        );
        self.memory.check(&self.residency, japan)?;
        if let Some(remote) = &self.run.remote_execution {
            remote.check().map_err(LifecycleError::Config)?;
            if !self.run.sandbox {
                return Err(LifecycleError::Config(
                    "run.remote_execution needs run.sandbox: without it the gate runs on this host"
                        .into(),
                ));
            }
        }
        if let Some(pool) = &self.run.execution_pool {
            pool.check().map_err(LifecycleError::Config)?;
            if self.run.remote_execution.is_some() {
                return Err(LifecycleError::Config(
                    "run.execution_pool and run.remote_execution both name the execution host: \
                     keep one"
                        .into(),
                ));
            }
            if !self.run.sandbox {
                return Err(LifecycleError::Config(
                    "run.execution_pool needs run.sandbox: without it the gate runs on this host"
                        .into(),
                ));
            }
        }
        if let Some(cache) = &self.run.remote_cache {
            cache.check().map_err(LifecycleError::Config)?;
            if !self.run.sandbox {
                return Err(LifecycleError::Config(
                    "run.remote_cache needs run.sandbox: the caches are the sandbox's".into(),
                ));
            }
        }
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
        if self.repo.app.is_some() && fork.platform != ForkPlatform::GitHub {
            return Err(LifecycleError::Config(
                "repo.app: the GitHub App connects GitHub repos only".into(),
            ));
        }
        if fork.platform == ForkPlatform::GitLab && fork.api == default_api() {
            return Err(LifecycleError::Config(
                "repo.fork.api: a GitLab fork needs its instance's API root \
                 (`https://gitlab.com/api/v4` or a self-hosted one)"
                    .into(),
            ));
        }
        if fork.platform == ForkPlatform::Bitbucket && fork.api == default_api() {
            return Err(LifecycleError::Config(format!(
                "repo.fork.api: a Bitbucket fork needs Bitbucket's API root (`{}`)",
                aegis_pr::BITBUCKET_API
            )));
        }
        if fork.platform == ForkPlatform::BitbucketDataCenter
            && !fork.api.trim_end_matches('/').ends_with("/rest/api/1.0")
        {
            return Err(LifecycleError::Config(
                "repo.fork.api: a Bitbucket Data Center fork needs its instance's API root \
                 (`https://<host>/rest/api/1.0`)"
                    .into(),
            ));
        }
        if fork.platform == ForkPlatform::AzureDevOps
            && (fork.api == default_api() || fork.api.contains("/_apis"))
        {
            return Err(LifecycleError::Config(
                "repo.fork.api: an Azure DevOps fork needs its organization or collection URL \
                 (`https://dev.azure.com/<org>`)"
                    .into(),
            ));
        }
        if fork.entra.is_some() && fork.platform != ForkPlatform::AzureDevOps {
            return Err(LifecycleError::Config(
                "repo.fork.entra: a service principal reaches Azure DevOps forks only".into(),
            ));
        }
        self.repo
            .audit_policy()
            .check_ids()
            .map_err(LifecycleError::Config)?;
        self.repo.secrets.check().map_err(LifecycleError::Config)?;
        self.repo
            .triage_lane
            .check()
            .map_err(LifecycleError::Config)?;
        if let Some(runtime) = &self.run.sandbox_runtime {
            aegis_gate::check_runtime(runtime)
                .map_err(|e| LifecycleError::Config(format!("run.sandbox_runtime: {e}")))?;
        }
        aegis_gate::check_sidecars(&self.repo.sidecars)
            .map_err(|e| LifecycleError::Config(format!("repo.sidecars: {e}")))?;
        if !self.repo.sidecars.is_empty() && !self.run.sandbox {
            return Err(LifecycleError::Config(
                "repo.sidecars need run.sandbox = true: the test steps reach them on a Docker network"
                    .into(),
            ));
        }
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

    /// `config/poc.toml` with `sidecar` as its one `[[repo.sidecars]]`,
    /// and `run.sandbox` as given.
    fn with_sidecar_in(sidecar: &str, sandbox: bool) -> Result<RunConfig, LifecycleError> {
        let mut cfg: toml::Table = toml::from_str(POC).unwrap();
        cfg["run"]
            .as_table_mut()
            .unwrap()
            .insert("sandbox".into(), sandbox.into());
        let sidecar: toml::Table = toml::from_str(sidecar).unwrap();
        cfg["repo"]
            .as_table_mut()
            .unwrap()
            .insert("sidecars".into(), toml::Value::Array(vec![sidecar.into()]));
        RunConfig::from_toml(&toml::to_string(&cfg).unwrap())
    }

    fn with_sidecar(sidecar: &str) -> Result<RunConfig, LifecycleError> {
        with_sidecar_in(sidecar, true)
    }

    const POSTGRES: &str = r#"
name = "postgres"
image = "postgres:16-alpine"
user = "70:70"
env = { POSTGRES_PASSWORD = "test" }
tmpfs = ["/var/lib/postgresql/data", "/var/run/postgresql"]
ready = ["pg_isready", "-h", "localhost"]
test_env = { DATABASE_URL = "postgres://postgres:test@postgres:5432/postgres" }
"#;

    #[test]
    fn a_repo_declares_the_services_its_tests_need() {
        let cfg = with_sidecar(POSTGRES).unwrap();
        let [pg] = cfg.repo.sidecars.as_slice() else {
            panic!("{:?}", cfg.repo.sidecars)
        };
        assert_eq!(pg.user, "70:70");
        assert_eq!(pg.ready_timeout_s, 60);
        assert!(!pg.localhost);
        let sidecars = cfg.repo.gate_sidecars(&cfg.run).unwrap();
        assert_eq!(sidecars.docker, cfg.run.docker);
        assert_eq!(sidecars.specs.len(), 1);
        assert!(
            RunConfig::from_toml(POC)
                .unwrap()
                .repo
                .gate_sidecars(&cfg.run)
                .is_none()
        );

        let err = with_sidecar(&POSTGRES.replace("70:70", "0:0"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("repo.sidecars: sidecar `postgres`"), "{err}");
        let err = with_sidecar(&format!("{POSTGRES}\nport = 5432"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("port"), "{err}");
        let err = with_sidecar_in(POSTGRES, false).unwrap_err().to_string();
        assert!(err.contains("need run.sandbox"), "{err}");
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
    fn a_bitbucket_fork_names_bitbuckets_api_root() {
        let bitbucket = |api: &str| {
            RunConfig::from_toml(&POC.replacen(
                "[repo.fork]",
                &format!("[repo.fork]\nplatform = \"bitbucket\"{api}"),
                1,
            ))
        };
        let err = bitbucket("").unwrap_err().to_string();
        assert!(err.contains("a Bitbucket fork needs"), "{err}");
        let cfg = bitbucket("\napi = \"https://api.bitbucket.org/2.0\"").unwrap();
        assert_eq!(cfg.repo.fork.platform, ForkPlatform::Bitbucket);
        let forge = crate::publish::fork_forge_of(
            &cfg.repo.fork,
            &aegis_pr::TokenSource::Static("t".into()),
        )
        .unwrap();
        assert!(matches!(forge, aegis_pr::Forge::Bitbucket(_)));
    }

    #[test]
    fn an_azure_fork_names_its_organization_and_may_be_a_service_principal() {
        let azure = |extra: &str| {
            RunConfig::from_toml(&POC.replacen(
                "[repo.fork]",
                &format!("[repo.fork]\nplatform = \"azure-devops\"{extra}"),
                1,
            ))
        };
        let err = azure("").unwrap_err().to_string();
        assert!(err.contains("an Azure DevOps fork needs"), "{err}");
        let cfg = azure(
            "\napi = \"https://dev.azure.com/acme\"\n\
             entra = { tenant = \"contoso.onmicrosoft.com\", client_id = \"app-1\" }",
        )
        .unwrap();
        assert_eq!(cfg.repo.fork.platform, ForkPlatform::AzureDevOps);
        let entra = cfg.repo.fork.entra.as_ref().unwrap();
        assert_eq!(entra.authority, aegis_pr::ENTRA_AUTHORITY);
        let forge = crate::publish::fork_forge_of(
            &cfg.repo.fork,
            &aegis_pr::TokenSource::Static("t".into()),
        )
        .unwrap();
        assert!(matches!(forge, aegis_pr::Forge::AzureDevOps(_)));
        let err = RunConfig::from_toml(&POC.replacen(
            "[repo.fork]",
            "[repo.fork]\nentra = { tenant = \"t\", client_id = \"c\" }",
            1,
        ))
        .unwrap_err()
        .to_string();
        assert!(err.contains("Azure DevOps forks only"), "{err}");
    }

    #[test]
    fn a_data_center_fork_names_its_instances_api_root() {
        let dc = |api: &str| {
            RunConfig::from_toml(&POC.replacen(
                "[repo.fork]",
                &format!("[repo.fork]\nplatform = \"bitbucket-dc\"{api}"),
                1,
            ))
        };
        let err = dc("").unwrap_err().to_string();
        assert!(err.contains("a Bitbucket Data Center fork needs"), "{err}");
        let cfg = dc("\napi = \"https://git.acme.example/rest/api/1.0\"").unwrap();
        assert_eq!(cfg.repo.fork.platform, ForkPlatform::BitbucketDataCenter);
        let forge = crate::publish::fork_forge_of(
            &cfg.repo.fork,
            &aegis_pr::TokenSource::Static("t".into()),
        )
        .unwrap();
        assert!(matches!(forge, aegis_pr::Forge::BitbucketDataCenter(_)));
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

    /// #471: without its own `max_model_calls` a repo gets its language's
    /// cap, then the platform's 40.
    #[test]
    fn the_call_cap_falls_back_to_the_language_then_to_40() {
        let profiles = Profiles::builtin().unwrap();
        let cfg = |language: &str, calls: &str| {
            let text = POC
                .replacen("[budget]\n", &format!("[budget]\n{calls}"), 1)
                .replacen(
                    "language = \"python\"",
                    &format!("language = \"{language}\""),
                    1,
                );
            RunConfig::from_toml(&text).unwrap()
        };
        let calls = |c: RunConfig| c.budget_limits(&profiles).unwrap().max_model_calls;
        assert_eq!(calls(cfg("rust", "")), 80);
        assert_eq!(calls(cfg("python", "")), 40);
        assert_eq!(calls(cfg("rust", "max_model_calls = 30\n")), 30);
        let limits = cfg("go", "").budget_limits(&profiles).unwrap();
        assert_eq!(limits.max_cost_usd, 4.0);
        assert_eq!(limits.max_model_calls, 40);
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
