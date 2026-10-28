//! One issue from start to PR or stop (design §2 steps 1–10, §5.3).
//!
//! Light classifies; Core works in a model loop with the ACI tools until it
//! gives its final answer; the gate checks the patch; a failure is
//! summarized by Light and handed back; the router escalates to Expert
//! (same history) or stops. Every model call and gate run goes to the
//! ledger as it happens. The future is `Send + 'static` so a server can
//! spawn it (ADR 0003).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use aegis_aci::{REDACTED_MARKER, Workspace, load_or_create_summary, redact};
use aegis_gate::{
    BuildCache, CancelToken, Egress, FetchStep, GatePart, GateReport, GateRequest, ReferenceGroup,
    Runner, Sandbox, SonarStep, StepName, StepStatus, WORKDIR, failing_tests_with, run_gate,
};
use aegis_lang::{AllowList, ProfilePart, Profiles, RepoProfiles, ResolvedProfile};
use aegis_ledger::{
    GateRun, IssueId, IssueLedger, LedgerStore, Outcome, PriceTable, Tier, ToolCall,
};
use aegis_orchestrator::{
    LoopConfig, LoopEnd, McpServerSpec, McpTools, ModelSpec, RepoPrompt, Session, TurnRecord,
    run_loop, system_blocks,
};
use aegis_provider::{CacheTtl, ContentBlock, Message, ModelProvider, ProviderConfig, Role};
use aegis_router::{BudgetCheck, Classification, Decision, Router, Signal, Spent};
use aegis_sonar_batch::{Author, commit_tree};
use tokio_util::sync::CancellationToken;

use crate::answer::{FinalAnswer, Status, parse_answer};
use crate::batch::sonar_batch;
use crate::checks::{Halt, JobCheck, fold_tools, test_reminder};
use crate::code_graph::{self, CodeGraph, ServerCalls};
use crate::issue::HiddenTests;
use crate::job::{self, Worktree};
use crate::light;
use crate::publish::{fork_remote, publish, publish_round};
use crate::review::{MAX_DIFF_CHARS, ReviewRound};
use crate::{IssueInput, LifecycleError, RunConfig};

/// Final answers the agent may get wrong before the job stops.
const MAX_ANSWER_RETRIES: u32 = 2;

/// What every job of a run shares. Built once; jobs hold it in an `Arc`.
pub struct Platform<P, S> {
    pub config: RunConfig,
    pub provider: P,
    pub store: S,
    pub prices: PriceTable,
    pub profiles: Profiles,
    /// For the fork: push and open PRs.
    pub github_token: String,
}

impl<P, S> std::fmt::Debug for Platform<P, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Platform")
            .field("repo", &self.config.repo.key)
            .field("github_token", &"<redacted>")
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct IssueReport {
    pub issue_id: String,
    pub outcome: Outcome,
    /// Why it stopped; `None` when a PR was opened.
    pub reason: Option<String>,
    pub pr_url: Option<String>,
    pub cost_usd: f64,
    pub model_calls: u32,
    /// Eval runs: whether the fix's own tests pass on the agent's patch.
    pub hidden_tests_passed: Option<bool>,
}

/// How the work on an issue ended, before the ledger closes it.
pub(crate) struct Ending {
    pub outcome: Outcome,
    pub reason: Option<String>,
    pub pr_url: Option<String>,
    /// [`Finish::Keep`]: the commit of the tree the gate passed.
    pub kept: Option<Kept>,
}

impl Ending {
    pub(crate) fn stop(outcome: Outcome, reason: impl Into<String>) -> Self {
        Self {
            outcome,
            reason: Some(reason.into()),
            pr_url: None,
            kept: None,
        }
    }
}

/// A Sonar group's passing patch, committed on its base (#17).
#[derive(Debug, Clone)]
pub(crate) struct Kept {
    pub commit: String,
    pub answer: FinalAnswer,
}

/// What a passing gate leads to.
pub(crate) enum Finish {
    /// The PR (design §2 step 7).
    Publish,
    /// A commit in the clone, merged later with the batch's other groups.
    Keep(Author),
    /// One more commit on an open PR's branch, and a reply (#243).
    Update(Box<ReviewRound>),
}

/// Runs one issue. `cancel` is checked before every model call and stops a
/// running gate.
pub async fn run_issue<P, S>(
    platform: Arc<Platform<P, S>>,
    issue: IssueInput,
    cancel: CancellationToken,
) -> Result<IssueReport, LifecycleError>
where
    P: ModelProvider + Send + Sync + 'static,
    S: LedgerStore + Send + Sync + 'static,
{
    run(platform, issue, None, cancel).await
}

/// Runs a review round (#243) of `issue`'s PR under `issue.id`, its own
/// ledger: the agent goes on from the PR's branch and, when the gate
/// passes, pushes to it and replies on the PR.
pub async fn run_review<P, S>(
    platform: Arc<Platform<P, S>>,
    issue: IssueInput,
    round: ReviewRound,
    cancel: CancellationToken,
) -> Result<IssueReport, LifecycleError>
where
    P: ModelProvider + Send + Sync + 'static,
    S: LedgerStore + Send + Sync + 'static,
{
    run(platform, issue, Some(round), cancel).await
}

async fn run<P, S>(
    platform: Arc<Platform<P, S>>,
    issue: IssueInput,
    round: Option<ReviewRound>,
    cancel: CancellationToken,
) -> Result<IssueReport, LifecycleError>
where
    P: ModelProvider + Send + Sync + 'static,
    S: LedgerStore + Send + Sync + 'static,
{
    let started = Instant::now();
    let cfg = &platform.config;
    let regional = matches!(cfg.provider, ProviderConfig::Bedrock { .. });
    let issue_id = IssueId::parse(issue.id.clone())?;
    // One spelling of the id everywhere: ledger, job dir, branch.
    let issue = IssueInput {
        id: issue_id.as_str().to_ascii_lowercase(),
        ..issue
    };
    let mut ledger =
        IssueLedger::open(&platform.store, &platform.prices, regional, issue_id).await?;
    if let Some(outcome) = ledger.record().outcome {
        return Err(LifecycleError::Config(format!(
            "issue `{}` already ended ({outcome:?}); run it under a new id",
            issue.id
        )));
    }

    let prepared = match round {
        Some(round) => review_and_work(&platform, &mut ledger, &issue, round, &cancel).await,
        None => prepare_and_work(&platform, &mut ledger, &issue, &cancel).await,
    };
    let ending = match prepared {
        Ok(ending) => ending,
        // The ledger still gets an outcome, so the run is not left open.
        Err(e) => Ending::stop(Outcome::StoppedSteps, format!("error: {e}")),
    };

    ledger
        .finished(ending.outcome, started.elapsed().as_secs_f64())
        .await?;
    let record = ledger.record();
    Ok(IssueReport {
        issue_id: issue.id,
        outcome: ending.outcome,
        reason: ending.reason,
        pr_url: ending.pr_url,
        cost_usd: record.cost_usd,
        model_calls: record.model_calls,
        hidden_tests_passed: record.hidden_tests_passed,
    })
}

async fn prepare_and_work<P, S>(
    platform: &Platform<P, S>,
    ledger: &mut IssueLedger<'_, S>,
    issue: &IssueInput,
    cancel: &CancellationToken,
) -> Result<Ending, LifecycleError>
where
    P: ModelProvider + Send + Sync + 'static,
    S: LedgerStore + Send + Sync + 'static,
{
    let cfg = &platform.config;
    if cancel.is_cancelled() {
        return Ok(Ending::stop(Outcome::StoppedSteps, "cancelled"));
    }
    // Issue text goes to the model too (design §7).
    let (title, body) = (redact(&issue.title), redact(&issue.body));
    ledger
        .secrets_redacted(count(title.count() + body.count()))
        .await?;
    let issue = &IssueInput {
        title: title.text,
        body: body.text,
        ..issue.clone()
    };
    let (class, response) = match issue.kind {
        // Its source knows: nothing for Light to decide.
        Some(kind) => (
            Classification {
                kind,
                ..light::default_classification(issue)
            },
            None,
        ),
        None => light::classify(&platform.provider, &cfg.models.light, issue).await,
    };
    if let Some(r) = &response {
        ledger
            .model_call(Tier::Light, &cfg.models.light.id, &r.usage)
            .await?;
    }
    ledger.classified(class.kind).await?;
    let profiles = cfg.repo.profiles(&platform.profiles)?;
    if class.needs_sonar_batching() {
        return sonar_batch(platform, ledger, &profiles, issue, cancel).await;
    }
    let first = issue.first_message(&class.summary, class.has_repro_info);
    work(
        platform,
        ledger,
        &profiles,
        issue,
        first,
        &Finish::Publish,
        cancel,
    )
    .await
}

/// A review round: the PR's branch fetched into the clone, the review as
/// the first message, no triage (it is the issue's).
async fn review_and_work<P, S>(
    platform: &Platform<P, S>,
    ledger: &mut IssueLedger<'_, S>,
    issue: &IssueInput,
    round: ReviewRound,
    cancel: &CancellationToken,
) -> Result<Ending, LifecycleError>
where
    P: ModelProvider + Send + Sync + 'static,
    S: LedgerStore + Send + Sync + 'static,
{
    let cfg = &platform.config;
    if cancel.is_cancelled() {
        return Ok(Ending::stop(Outcome::StoppedSteps, "cancelled"));
    }
    let (title, body) = (redact(&issue.title), redact(&issue.body));
    let (round, in_notes) = round.redacted();
    ledger
        .secrets_redacted(count(title.count() + body.count() + in_notes))
        .await?;
    let issue = &IssueInput {
        title: title.text,
        body: body.text,
        ..issue.clone()
    };
    ledger.classified(aegis_ledger::IssueKind::Bug).await?;
    // The branch as it is now, reviewers' commits included.
    let remote = fork_remote(platform)?;
    let (clone, branch) = (cfg.repo.path.clone(), round.pull.head_ref.clone());
    let (url, token) = (remote.url().to_owned(), platform.github_token.clone());
    let into = format!("review/{}", round.branch_id());
    let head = tokio::task::spawn_blocking(move || {
        let source = crate::clone::Source {
            url: &url,
            token: Some(&token).filter(|t| !t.is_empty()).map(String::as_str),
        };
        crate::clone::fetch_ref(&clone, source, &branch, &into)
    })
    .await
    .map_err(|e| LifecycleError::Config(format!("fetching the PR's branch: {e}")))??;
    let base = job::merge_base(&cfg.repo.path, &head, &cfg.repo.base)?;
    let diff = job::diff(&cfg.repo.path, &base, &head, MAX_DIFF_CHARS)?;
    let diff = redact(&diff);
    ledger.secrets_redacted(count(diff.count())).await?;
    let first = round.first_message(issue, &diff.text);
    let round = ReviewRound {
        pull: aegis_pr::PullState {
            head_sha: head,
            ..round.pull
        },
        ..round
    };
    let profiles = cfg.repo.profiles(&platform.profiles)?;
    work(
        platform,
        ledger,
        &profiles,
        issue,
        first,
        &Finish::Update(Box::new(round)),
        cancel,
    )
    .await
}

/// In the Codebase-Memory server's `args` and `env`: the index directory
/// under the state dir.
const INDEX_DIR: &str = "{index_dir}";

/// `repo.dirs` as the ACI's `--parts` file (`[[part]]`, #71), with the
/// mounts of each part's gate steps (its `.venv`, caches, #198).
fn parts_file(cfg: &RunConfig, profiles: &RepoProfiles) -> Result<String, LifecycleError> {
    let mut parts = Vec::new();
    // `RepoProfiles` keeps the order of `repo.dirs`, after the root.
    for (d, p) in cfg.repo.dirs.iter().zip(&profiles.parts()[1..]) {
        let mounts: Vec<toml::Value> = sandbox(cfg, p)?
            .map(|s| s.mounts)
            .unwrap_or_default()
            .into_iter()
            .map(|(host, inside)| {
                let mut mount = toml::Table::new();
                mount.insert("host".into(), host.display().to_string().into());
                mount.insert("path".into(), inside.into());
                mount.into()
            })
            .collect();
        let mut part = toml::Table::new();
        part.insert("dir".into(), d.path.clone().into());
        part.insert("language".into(), d.language.id().into());
        part.insert("overrides".into(), d.overrides.clone().into());
        part.insert("mounts".into(), mounts.into());
        parts.push(toml::Value::from(part));
    }
    let mut file = toml::Table::new();
    file.insert("part".into(), parts.into());
    toml::to_string(&file).map_err(|e| LifecycleError::Config(format!("repo.dirs: {e}")))
}

fn tool_specs(
    cfg: &RunConfig,
    profiles: &RepoProfiles,
    tree: &Path,
    job_dir: &Path,
) -> Result<Vec<McpServerSpec>, LifecycleError> {
    let overrides = job_dir.join("overrides.toml");
    let text = toml::to_string(&cfg.repo.overrides)
        .map_err(|e| LifecycleError::Config(format!("repo.overrides: {e}")))?;
    std::fs::write(&overrides, text).map_err(|e| LifecycleError::io(&overrides, &e))?;
    let root = &profiles.parts()[0];
    let profile = &root.profile;
    let mut args = vec![
        "--repo".into(),
        tree.display().to_string(),
        "--language".into(),
        cfg.repo.language.id().into(),
        "--overrides".into(),
        overrides.display().to_string(),
        // `run_tests` stops at the step timeout, or sooner (#137).
        "--test-timeout".into(),
        cfg.run.step_timeout_s.to_string(),
        "--test-dir".into(),
        job_dir.join("run_tests").display().to_string(),
    ];
    if !cfg.repo.dirs.is_empty() {
        let parts = job_dir.join("parts.toml");
        std::fs::write(&parts, parts_file(cfg, profiles)?)
            .map_err(|e| LifecycleError::io(&parts, &e))?;
        args.extend(["--parts".into(), parts.display().to_string()]);
    }
    // With the gate in containers, `edit`'s formatter and `run_tests` run
    // there too: the machine needs no language tools (#104), and the test
    // sees what the gate's would (#137).
    if let Some(sandbox) = sandbox(cfg, root)? {
        args.extend([
            "--format-image".into(),
            profile.image.clone(),
            "--docker".into(),
            cfg.run.docker.clone(),
        ]);
        for (host, inside) in &sandbox.mounts {
            args.extend([
                "--format-mount".into(),
                format!("{}:{inside}", host.display()),
            ]);
        }
        for (key, value) in &sandbox.env {
            args.extend(["--test-env".into(), format!("{key}={value}")]);
        }
    }
    let mut specs = vec![McpServerSpec {
        name: "aci".into(),
        program: cfg.aci_bin().display().to_string(),
        args,
        cwd: None,
        env: vec![],
        tools: None,
    }];
    if let Some(cbm) = &cfg.codebase_memory {
        // One index directory for every repo: the server's daemon is one
        // per user and serves a single cache directory, so a job with
        // another one could not start while a job is running (#185). Each
        // job indexes its tree as its own project and drops it at the end
        // (`CodeGraph`). The server runs in the index directory, not in the
        // tree: the daemon the first job's server starts keeps that
        // directory as its own, and once the job's tree is gone
        // `detect_changes` can run no git command (#203).
        let index = cfg.run.state_dir.join("index");
        std::fs::create_dir_all(&index).map_err(|e| LifecycleError::io(&index, &e))?;
        let cwd = index.clone();
        let index = index.display().to_string();
        let fill = |v: &String| v.replace(INDEX_DIR, &index);
        specs.push(McpServerSpec {
            name: code_graph::SERVER.into(),
            program: cbm.program.clone(),
            args: cbm.args.iter().map(fill).collect(),
            cwd: Some(cwd),
            env: cbm.env.iter().map(|(k, v)| (k.clone(), fill(v))).collect(),
            tools: Some(cbm.tools.clone()),
        });
    }
    Ok(specs)
}

/// Steps 2–10 on the job's own worktree.
pub(crate) async fn work<P, S>(
    platform: &Platform<P, S>,
    ledger: &mut IssueLedger<'_, S>,
    profiles: &RepoProfiles,
    issue: &IssueInput,
    first_message: String,
    finish: &Finish,
    cancel: &CancellationToken,
) -> Result<Ending, LifecycleError>
where
    P: ModelProvider + Send + Sync + 'static,
    S: LedgerStore + Send + Sync + 'static,
{
    let cfg = &platform.config;
    // A review round starts from the PR's head and is still checked
    // against the PR's base (#243).
    let (base, start) = match finish {
        Finish::Update(round) => (
            job::merge_base(&cfg.repo.path, &round.pull.head_sha, &cfg.repo.base)?,
            round.pull.head_sha.clone(),
        ),
        _ => {
            let base = job::resolve(
                &cfg.repo.path,
                issue.base.as_deref().unwrap_or(&cfg.repo.base),
            )?;
            (base.clone(), base)
        }
    };
    let job_dir = cfg.run.state_dir.join("jobs").join(&issue.id);
    let tree = Worktree::create(&cfg.repo.path, &start, &job_dir.join("repo"))?;

    let map = module_map(cfg, profiles, tree.path(), &base)?;
    let conventions = language_conventions(profiles);
    let system = system_blocks(&RepoPrompt {
        language_conventions: conventions.as_deref(),
        repo_conventions: cfg.repo.conventions.as_deref(),
        module_summary: Some(&map),
    });

    for part in profiles.parts() {
        let dir = tree.path().join(&part.dir);
        if let Some(why) = fetch_base(cfg, part, &dir, &job_dir, cancel).await? {
            return Ok(Ending::stop(Outcome::StoppedSteps, why));
        }
        // The gate would start the tree's build from the cache anyway; doing
        // it now spares the agent's first `run_tests` a cold build (#137).
        if let Some(cache) = build_cache(cfg, part) {
            tokio::task::spawn_blocking(move || cache.seed(&dir))
                .await
                .map_err(|e| LifecycleError::Config(format!("seeding the build cache: {e}")))??;
        }
    }
    let tools = McpTools::connect(&tool_specs(cfg, profiles, tree.path(), &job_dir)?).await?;
    let queries = cfg.codebase_memory.as_ref().map(|c| c.tools.as_slice());
    let graph = match CodeGraph::open(&tools, queries, tree.path()).await {
        Ok(graph) => graph,
        Err(e) => {
            tools.shutdown().await;
            return Err(e);
        }
    };
    let session = Session::new(
        system,
        Message::user_text(first_message),
        CacheTtl::FiveMinutes,
    );
    let result = drive(
        platform,
        ledger,
        profiles,
        issue,
        &graph,
        session,
        tree.path(),
        &base,
        &job_dir,
        finish,
        cancel,
    )
    .await;
    graph.close().await;
    tools.shutdown().await;
    drop(tree);
    result
}

/// The module map of the repo at `base`, made from the job's fresh
/// checkout `tree` and kept under the state dir by the digest of the files
/// the profile reads. BP2 is the same for every issue at the same files
/// (design §4.4); a new base that changes one of them makes a new map
/// (#24).
pub(crate) fn module_map(
    cfg: &RunConfig,
    profiles: &RepoProfiles,
    tree: &Path,
    base: &str,
) -> Result<String, LifecycleError> {
    let digest = job::files_digest(&cfg.repo.path, base, |p| {
        profiles.owns_file(&p.to_string_lossy())
    })?;
    let checkout = Workspace::new(tree, profiles.clone())?;
    let map_path = cfg
        .run
        .state_dir
        .join("maps")
        .join(&cfg.repo.key)
        .join(format!("{digest}.md"));
    Ok(load_or_create_summary(&checkout, &map_path)?)
}

/// The profile's conventions for the system prompt; with directories of
/// other languages (#71), each language's, and where each goes.
fn language_conventions(profiles: &RepoProfiles) -> Option<String> {
    let parts = profiles.parts();
    if parts.len() == 1 {
        return profiles.root().profile.conventions().map(str::to_owned);
    }
    let mut text = String::from(
        "# Languages of this repo\n\nThis repo has more than one language; each directory below \
         goes by its own conventions and checks, the rest of the repo by the first:\n",
    );
    for part in parts {
        text.push_str(&format!("- {}\n", part.label()));
    }
    text.push_str(
        "\nName files, and tests given to `run_tests` or as `repro_test`, by their path from \
         the repo root (`web/tests/cart.test.ts`): a test is then run with its directory's \
         command, in that directory.\n",
    );
    let mut seen = Vec::new();
    for part in parts {
        let language = part.profile.language();
        if seen.contains(&language) {
            continue;
        }
        seen.push(language);
        if let Some(c) = part.profile.profile.conventions() {
            text.push('\n');
            text.push_str(c.trim_end());
            text.push('\n');
        }
    }
    Some(text)
}

/// Where a part's caches live on the host: the repo's cache dir, a
/// directory of its own for a part (#71).
fn part_cache(cfg: &RunConfig, part: &ProfilePart) -> PathBuf {
    let cache = cfg.run.state_dir.join("cache").join(&cfg.repo.key);
    match part.dir.is_empty() {
        true => cache,
        false => cache.join("parts").join(part.dir.replace('/', "_")),
    }
}

/// The ledger's view of the tools of `turn` (#129).
fn tool_calls(turn: &TurnRecord) -> Vec<ToolCall> {
    turn.tool_calls
        .iter()
        .map(|c| ToolCall {
            name: c.name.clone(),
            is_error: c.is_error,
            target: c.target.clone(),
        })
        .collect()
}

fn model_spec(cfg: &RunConfig, tier: Tier) -> ModelSpec {
    let model = cfg.models.for_tier(tier);
    ModelSpec {
        id: model.id.clone(),
        effort: model.effort,
        max_tokens: cfg.run.max_tokens,
        context_window: cfg.run.context_window,
    }
}

/// The gate's container: the profile's image, and each cache directory
/// outside the tree kept on the host under `state_dir/cache/<repo>/`, so it
/// survives between jobs.
/// The repo's build cache (`target/` for Rust), when the gate runs in a
/// sandbox: every container sees the tree at the same `/work`, so a build
/// started from another tree's directory is reused (#104). Directories the
/// fetch step fills (a venv) are mounted, not cached here.
fn build_cache(cfg: &RunConfig, part: &ProfilePart) -> Option<BuildCache> {
    let profile = &part.profile;
    let deps = &profile.profile.dependencies.dirs;
    let dirs: Vec<String> = profile
        .profile
        .sandbox
        .cache_dirs
        .iter()
        .filter(|d| !d.starts_with('/') && !deps.contains(d))
        .cloned()
        .collect();
    (cfg.run.sandbox && !dirs.is_empty()).then(|| BuildCache {
        dir: part_cache(cfg, part).join("build"),
        dirs,
    })
}

/// The Sonar step of the configuration, its scanner cache under the
/// state dir, shared by the repo's jobs.
pub(crate) fn sonar_step(cfg: &RunConfig) -> Result<Option<SonarStep>, LifecycleError> {
    let Some(sonar) = &cfg.sonar else {
        return Ok(None);
    };
    let cache = cfg.run.state_dir.join("cache").join("sonar");
    std::fs::create_dir_all(&cache).map_err(|e| LifecycleError::io(&cache, &e))?;
    sonar.step(Some(cache)).map(Some)
}

/// The profile's fetch command and its egress, when the gate runs in a
/// sandbox (#66). On the machine there is no cache to fetch into.
fn fetch_step(cfg: &RunConfig, profile: &ResolvedProfile) -> Option<FetchStep> {
    let command = profile.fetch.command()?.render(&[]).ok()?;
    cfg.run.sandbox.then(|| FetchStep {
        egress: Egress {
            docker: cfg.run.docker.clone(),
            proxy_image: cfg.run.egress_image.clone(),
            allow: profile.fetch_hosts.clone(),
        },
        command,
        audit_hosts: profile.profile.dependencies.audit_hosts.clone(),
    })
}

/// The base's dependencies, fetched through the egress proxy before the
/// agent starts (#66); `Some(reason)` when that fails.
async fn fetch_base(
    cfg: &RunConfig,
    part: &ProfilePart,
    tree: &Path,
    job_dir: &Path,
    cancel: &CancellationToken,
) -> Result<Option<String>, LifecycleError> {
    let (Some(fetch), Some(sandbox)) = (fetch_step(cfg, &part.profile), sandbox(cfg, part)?) else {
        return Ok(None);
    };
    let log = match part.dir.is_empty() {
        true => "fetch.log".to_owned(),
        false => format!("fetch-{}.log", part.dir.replace('/', "_")),
    };
    let gate_cancel = CancelToken::new();
    let _stop = CancelOnDrop(gate_cancel.clone());
    let _watcher = AbortOnDrop(tokio::spawn({
        let (outer, inner) = (cancel.clone(), gate_cancel.clone());
        async move {
            outer.cancelled().await;
            inner.cancel();
        }
    }));
    let timeout = Duration::from_secs(cfg.run.step_timeout_s);
    let (tree, log) = (tree.to_path_buf(), job_dir.join(log));
    let outcome = tokio::task::spawn_blocking(move || -> Result<_, aegis_gate::GateError> {
        let session = fetch.egress.start()?;
        let ran = session.fetch(&sandbox, &fetch.command, &tree, &log, timeout, gate_cancel)?;
        Ok((ran, session.log()))
    })
    .await
    .map_err(|e| LifecycleError::Config(format!("the fetch did not finish: {e}")))??;
    let (ran, proxy) = outcome;
    if ran.exit.success() {
        return Ok(None);
    }
    let log = std::fs::read_to_string(&ran.log).unwrap_or_default();
    let tail: Vec<&str> = log.lines().rev().take(20).collect();
    Ok(Some(format!(
        "fetching the dependencies of {} failed:\n{}\n\nproxy:\n{proxy}",
        part.label(),
        tail.into_iter().rev().collect::<Vec<_>>().join("\n")
    )))
}

fn sandbox(cfg: &RunConfig, part: &ProfilePart) -> Result<Option<Sandbox>, LifecycleError> {
    if !cfg.run.sandbox {
        return Ok(None);
    }
    let profile = &part.profile;
    let cache = part_cache(cfg, part);
    let mut mounts = Vec::new();
    for dir in profile
        .profile
        .sandbox
        .cache_dirs
        .iter()
        .filter(|d| d.starts_with('/'))
    {
        let host = cache.join(dir.trim_start_matches('/').replace('/', "_"));
        std::fs::create_dir_all(&host).map_err(|e| LifecycleError::io(&host, &e))?;
        mounts.push((host, dir.clone()));
    }
    // Dependencies installed inside the tree (a venv) live outside it, so
    // the base copy and every container of the job see the same ones.
    for dir in &profile.profile.dependencies.dirs {
        let host = cache.join("deps").join(dir.replace('/', "_"));
        std::fs::create_dir_all(&host).map_err(|e| LifecycleError::io(&host, &e))?;
        mounts.push((host, format!("{WORKDIR}/{dir}")));
    }
    Ok(Some(Sandbox {
        docker: cfg.run.docker.clone(),
        image: profile.image.clone(),
        mounts,
        env: cfg.run.sandbox_env.clone().into_iter().collect(),
        network: None,
        read_only_mounts: true,
    }))
}

/// The policy file's packages and the repo's own.
fn allowed_packages(cfg: &RunConfig) -> Result<BTreeSet<String>, LifecycleError> {
    let own = cfg.repo.allowed_package_names.iter().cloned();
    let Some(path) = &cfg.repo.allowed_packages else {
        return Ok(own.collect());
    };
    let text = std::fs::read_to_string(path).map_err(|e| LifecycleError::io(path, &e))?;
    let mut names = AllowList::from_toml(&text)?.names().clone();
    names.extend(own);
    Ok(names)
}

/// The gate on the job's tree, off the async runtime; `cancel` stops it.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn gate(
    cfg: &RunConfig,
    profiles: &RepoProfiles,
    tree: &Path,
    base: &str,
    job_dir: &Path,
    answer: &FinalAnswer,
    cancel: &CancellationToken,
) -> Result<GateReport, LifecycleError> {
    let allowed = allowed_packages(cfg)?;
    let root = &profiles.parts()[0];
    // Each directory's own container, fetch and cache (#71).
    let parts = profiles.parts()[1..]
        .iter()
        .map(|p| {
            Ok((
                sandbox(cfg, p)?,
                fetch_step(cfg, &p.profile),
                build_cache(cfg, p),
            ))
        })
        .collect::<Result<Vec<_>, LifecycleError>>()?;
    let sandbox = sandbox(cfg, root)?;
    let sonar = sonar_step(cfg)?;
    let gate_cancel = CancelToken::new();
    // Stops the gate when the job is cancelled, and when this future is
    // dropped (a server aborting the job).
    let _stop = CancelOnDrop(gate_cancel.clone());
    let _watcher = AbortOnDrop(tokio::spawn({
        let (outer, inner) = (cancel.clone(), gate_cancel.clone());
        async move {
            outer.cancelled().await;
            inner.cancel();
        }
    }));
    let owned = (
        tree.to_path_buf(),
        base.to_owned(),
        profiles.clone(),
        parts,
        allowed,
        answer.repro.clone(),
        job_dir.join("gate"),
        Duration::from_secs(cfg.run.step_timeout_s),
        sandbox,
        fetch_step(cfg, &root.profile),
        build_cache(cfg, root),
        answer
            .allowed_test_changes
            .iter()
            .map(|c| c.test.clone())
            .collect::<Vec<_>>(),
        answer
            .allowed_reference_changes
            .iter()
            .map(|g| ReferenceGroup {
                dir: g.references.clone(),
                tests: g.tests.clone(),
            })
            .collect::<Vec<_>>(),
        sonar,
        cfg.repo.audit_policy(),
    );
    let report = tokio::task::spawn_blocking(move || {
        let (
            repo,
            base,
            profiles,
            parts,
            allowed,
            repro,
            work_dir,
            step_timeout,
            sandbox,
            fetch,
            build_cache,
            allowed_tests,
            reference_groups,
            sonar,
            audit,
        ) = owned;
        let parts = profiles.parts()[1..]
            .iter()
            .zip(parts)
            .map(|(p, (sandbox, fetch, build_cache))| GatePart {
                dir: p.dir.clone(),
                profile: &p.profile,
                sandbox,
                fetch,
                build_cache,
            })
            .collect();
        run_gate(&GateRequest {
            repo: &repo,
            base: &base,
            profile: profiles.root(),
            allowed: &allowed,
            repro: repro.as_ref(),
            work_dir: &work_dir,
            env: vec![],
            step_timeout,
            cancel: gate_cancel,
            sandbox,
            fetch,
            build_cache,
            allowed_tests,
            reference_groups,
            sonar,
            audit,
            parts,
        })
    })
    .await
    .map_err(|e| LifecycleError::Config(format!("the gate did not finish: {e}")))?;
    Ok(report?)
}

struct CancelOnDrop(CancelToken);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Text of the model's last reply, only when the last message is the
/// model's: never the issue or a gate summary.
fn last_reply(session: &Session) -> String {
    session
        .messages()
        .last()
        .filter(|m| m.role == Role::Assistant)
        .map(|m| {
            m.content
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::Text { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// Values the ACI server hid in the tool results of `messages`.
fn hidden_in_tool_results(messages: &[Message]) -> u32 {
    let n = messages
        .iter()
        .flat_map(|m| &m.content)
        .filter_map(|b| match b {
            ContentBlock::ToolResult { content, .. } => {
                Some(content.matches(REDACTED_MARKER).count())
            }
            _ => None,
        })
        .sum();
    count(n)
}

/// The fix's own tests on the agent's patch, after the PR is open (the
/// PR holds the gated tree, so copying the tests in changes nothing sent).
/// Same runner and sandbox as the gate's for `part`, the part holding the
/// tests, in its directory (#199); the log stays in the job dir and never
/// reaches the model.
async fn run_hidden(
    cfg: &RunConfig,
    part: &ProfilePart,
    tree: &Path,
    job_dir: &Path,
    hidden: &HiddenTests,
    cancel: &CancellationToken,
) -> Result<(bool, Duration), LifecycleError> {
    for file in &hidden.test_files {
        let content = job::show(&cfg.repo.path, &hidden.fix_commit, file)?;
        let Some(content) = content else { continue };
        let target = tree.join(file);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| LifecycleError::io(parent, &e))?;
        }
        std::fs::write(&target, content).map_err(|e| LifecycleError::io(&target, &e))?;
    }
    let gate_cancel = CancelToken::new();
    let _stop = CancelOnDrop(gate_cancel.clone());
    let _watcher = AbortOnDrop(tokio::spawn({
        let (outer, inner) = (cancel.clone(), gate_cancel.clone());
        async move {
            outer.cancelled().await;
            inner.cancel();
        }
    }));
    let profile: &ResolvedProfile = &part.profile;
    let runner = Runner {
        env: vec![],
        timeout: Duration::from_secs(cfg.run.step_timeout_s),
        cancel: gate_cancel,
        sandbox: sandbox(cfg, part)?,
    };
    let (argv, tree, log) = (
        hidden.command.clone(),
        tree.join(&part.dir),
        job_dir.join("hidden-tests.log"),
    );
    let ran = tokio::task::spawn_blocking(move || runner.run(&argv, &tree, &log, &[]))
        .await
        .map_err(|e| LifecycleError::Config(format!("the hidden tests did not finish: {e}")))??;
    if ran.exit.success() {
        return Ok((true, ran.duration));
    }
    // What failed, kept for the dry run to fill `hidden_known_failures`.
    let failing = profile
        .gate
        .test_output
        .map(|format| {
            failing_tests_with(
                format,
                profile.gate.harness.as_ref(),
                &std::fs::read_to_string(&ran.log).unwrap_or_default(),
            )
        })
        .unwrap_or_default();
    let listed = job_dir.join("hidden-failures.txt");
    let text: String = failing.iter().map(|t| format!("{t}\n")).collect();
    std::fs::write(&listed, text).map_err(|e| LifecycleError::io(&listed, &e))?;
    let known = &hidden.known_failures;
    let passed = matches!(ran.exit, aegis_gate::Exit::Code(_))
        && !failing.is_empty()
        && failing.iter().all(|t| known.contains(t));
    Ok((passed, ran.duration))
}

pub(crate) fn gate_run(report: &GateReport, took: Duration) -> GateRun {
    GateRun {
        passed: report.passed(),
        failed_step: report
            .steps
            .iter()
            .find(|s| s.status == StepStatus::Failed)
            .map(|s| s.name.id().to_owned()),
        duration_s: took.as_secs_f64(),
    }
}

/// Model calls the agent needs at least to act on the code graph's note
/// before the gate: read a file, edit it, answer again, with one to spare.
const IMPACT_CALLS: u32 = 4;

/// The Core/Expert loop with gate runs between attempts.
#[allow(clippy::too_many_arguments)]
async fn drive<P, S, T>(
    platform: &Platform<P, S>,
    ledger: &mut IssueLedger<'_, S>,
    profiles: &RepoProfiles,
    issue: &IssueInput,
    tools: &CodeGraph<'_, T>,
    session: Session,
    tree: &Path,
    base: &str,
    job_dir: &Path,
    finish: &Finish,
    cancel: &CancellationToken,
) -> Result<Ending, LifecycleError>
where
    P: ModelProvider + Send + Sync + 'static,
    S: LedgerStore + Send + Sync + 'static,
    T: ServerCalls + Sync,
{
    let cfg = &platform.config;
    let regional = matches!(cfg.provider, ProviderConfig::Bedrock { .. });
    let mut router = Router::new(cfg.budget.rules());
    let mut session = session;
    let mut answer_retries = 0;
    let mut dearest_call_usd: f64 = 0.0;
    let mut first_expert_call = false;
    let mut impact_checked = false;
    loop {
        let tier = router.tier();
        let mut budget = BudgetCheck::new(
            Arc::new(platform.prices.clone()),
            regional,
            cfg.budget.limits(),
            tier,
            Spent::from_record(ledger.record()),
        );
        if std::mem::take(&mut first_expert_call) && dearest_call_usd > 0.0 {
            // New model, cold cache: the first Expert call costs more.
            budget = budget.with_first_call_estimate(dearest_call_usd * 3.0);
        }
        let check = JobCheck::new(budget, router, cancel.clone());
        let seen = session.messages().len();
        let model = model_spec(cfg, tier);
        let outcome = run_loop(
            &platform.provider,
            tools,
            &check,
            &model,
            session,
            &LoopConfig::default(),
        )
        .await;
        for turn in &outcome.turns {
            let cost = ledger.model_call(tier, &turn.model, &turn.usage).await?;
            dearest_call_usd = dearest_call_usd.max(cost);
            ledger.tool_calls(tier, tool_calls(turn)).await?;
        }
        let prior_calls = check.budget.model_calls(&[]);
        let (after_edits, decision) = fold_tools(router, prior_calls, &outcome.turns);
        if after_edits.tier() != router.tier() {
            // Escalated on the loop's last turn: Expert takes over from here.
            first_expert_call = true;
            if let Decision::Escalate(reason) = decision {
                let calls = ledger.record().model_calls;
                ledger.escalated(reason.to_string(), calls).await?;
            }
        }
        router = after_edits;
        session = outcome.session;
        ledger
            .secrets_redacted(hidden_in_tool_results(&session.messages()[seen..]))
            .await?;

        let reply = match outcome.end {
            LoopEnd::ProviderFailed(e) => {
                return Ok(Ending::stop(
                    Outcome::StoppedSteps,
                    format!("model provider failed: {e}"),
                ));
            }
            LoopEnd::Stopped { reason } => match check.halt() {
                Some(Halt::Cancelled) => {
                    return Ok(Ending::stop(Outcome::StoppedSteps, "cancelled"));
                }
                Some(Halt::Budget(stop)) => {
                    return Ok(Ending::stop(stop.outcome(), stop.to_string()));
                }
                Some(Halt::RemindTest { calls, last_test }) => {
                    // The router counts the reminder, so the next one comes
                    // only after more calls without a test run (#144, #156).
                    let (next, decision) = router.observe(Signal::ModelCalls { calls });
                    debug_assert_eq!(decision, Decision::RemindTest { calls, last_test });
                    router = next;
                    ledger.test_reminded(tier, calls).await?;
                    session = session.with_user_text(test_reminder(calls, last_test));
                    continue;
                }
                Some(Halt::Escalate(reason)) => {
                    if router.tier() == Tier::Core {
                        // Not an edit rule, which `fold_tools` already applied:
                        // Core used its calls without a patch.
                        let calls = ledger.record().model_calls;
                        let (next, decision) = router.observe(Signal::ModelCalls { calls });
                        debug_assert_eq!(decision, Decision::Escalate(reason));
                        router = next;
                        ledger.escalated(reason.to_string(), calls).await?;
                    }
                    first_expert_call = true;
                    continue;
                }
                None => return Ok(Ending::stop(Outcome::StoppedSteps, reason)),
            },
            LoopEnd::Finished { .. } => last_reply(&session),
        };

        let answer = match parse_answer(&reply) {
            Ok(answer) => answer,
            Err(why) if answer_retries < MAX_ANSWER_RETRIES => {
                answer_retries += 1;
                session = session.with_message(Message::user_text(format!(
                    "Your final answer could not be read ({why}). Reply with only the JSON \
                     object described in the instructions."
                )));
                continue;
            }
            Err(why) => {
                return Ok(Ending::stop(
                    Outcome::StoppedSteps,
                    format!("no usable final answer: {why}"),
                ));
            }
        };
        if answer.status == Status::Stopped {
            let why = answer
                .stop_reason
                .unwrap_or_else(|| "the agent stopped".into());
            return Ok(Ending::stop(
                Outcome::StoppedSteps,
                format!("agent stopped: {why}"),
            ));
        }

        // Once a job, before its first gate run: the code the graph links
        // to the patch, for the agent to check (#203). The graph only
        // helps, so a failed query lets the patch go on to the gate.
        // Not when the budget leaves the agent too few calls to act on the
        // note: the patch would then stop at the budget instead of going to
        // the gate.
        let record = ledger.record();
        let calls_left = cfg
            .budget
            .max_model_calls
            .saturating_sub(record.model_calls);
        let usd_left = cfg.budget.max_cost_usd - record.cost_usd;
        let room =
            calls_left >= IMPACT_CALLS && usd_left >= dearest_call_usd * f64::from(IMPACT_CALLS);
        if room
            && !std::mem::replace(&mut impact_checked, true)
            && let Some(impact) = tools.impact(base).await
        {
            match impact {
                Ok(impact) => {
                    ledger.impact_checked(impact.files(), None).await?;
                    if let Some(note) = impact.note() {
                        session = session.with_user_text(note);
                        continue;
                    }
                }
                Err(why) => ledger.impact_checked(Vec::new(), Some(why)).await?,
            }
        }

        let gate_started = Instant::now();
        let report = gate(cfg, profiles, tree, base, job_dir, &answer, cancel).await?;
        ledger
            .gate_run(gate_run(&report, gate_started.elapsed()))
            .await?;
        if report.cancelled() {
            return Ok(Ending::stop(Outcome::StoppedSteps, "cancelled"));
        }
        if cancel.is_cancelled() {
            return Ok(Ending::stop(Outcome::StoppedSteps, "cancelled"));
        }
        if report.passed() {
            let (_, decision) = router.observe(Signal::GatePassed);
            debug_assert_eq!(decision, Decision::OpenPr);
            if let Finish::Keep(author) = finish {
                let commit = commit_tree(
                    &cfg.repo.path,
                    &report.tree,
                    &report.base,
                    &format!("Sonar group {}", issue.id),
                    author,
                )?;
                return Ok(Ending {
                    outcome: Outcome::PrOpened,
                    reason: None,
                    pr_url: None,
                    kept: Some(Kept { commit, answer }),
                });
            }
            if let Finish::Update(round) = finish {
                let published =
                    publish_round(platform, ledger, tree, issue, round, &answer, &report).await?;
                return Ok(Ending {
                    outcome: Outcome::PrOpened,
                    reason: published.note,
                    pr_url: Some(published.url),
                    kept: None,
                });
            }
            let pr_url = if cfg.run.open_prs {
                let published =
                    publish(platform, ledger, profiles, tree, issue, &answer, &report).await?;
                Some(published.url)
            } else {
                None
            };
            if let Some(hidden) = &issue.hidden {
                let part = profiles
                    .parts()
                    .iter()
                    .find(|p| p.dir == hidden.dir)
                    .ok_or_else(|| {
                        LifecycleError::Config(format!(
                            "the hidden tests are in `{}/`, which has no profile",
                            hidden.dir
                        ))
                    })?;
                let (passed, took) = run_hidden(cfg, part, tree, job_dir, hidden, cancel).await?;
                ledger.hidden_tests(passed, took.as_secs_f64()).await?;
            }
            // With `open_prs` off the run ends where it would open the PR.
            return Ok(Ending {
                outcome: Outcome::PrOpened,
                reason: pr_url.is_none().then(|| "no PR opened".to_owned()),
                pr_url,
                kept: None,
            });
        }

        // Only a repro step that ran tells whether the patch fixed the issue;
        // one skipped after an earlier failure says nothing.
        let repro_ran = report
            .step(StepName::ReproTest)
            .map(|s| &s.status)
            .filter(|s| matches!(s, StepStatus::Passed | StepStatus::Failed));
        let (next, decision) = match repro_ran {
            Some(status) => router.observe(Signal::PatchTested {
                repro_passes: *status == StepStatus::Passed,
            }),
            None => (router, Decision::Continue),
        };
        router = next;
        let decision = match decision {
            Decision::Continue => {
                let (next, decision) = router.observe(Signal::GateFailed);
                router = next;
                decision
            }
            other => other,
        };
        if let Decision::Stop(reason) = decision {
            return Ok(Ending::stop(reason.outcome(), reason.to_string()));
        }
        if let Decision::Escalate(reason) = decision {
            first_expert_call = true;
            let calls = ledger.record().model_calls;
            ledger.escalated(reason.to_string(), calls).await?;
        }
        if cancel.is_cancelled() {
            return Ok(Ending::stop(Outcome::StoppedSteps, "cancelled"));
        }
        let gate_text = redact(&report.to_string());
        ledger.secrets_redacted(count(gate_text.count())).await?;
        let (summary, response) =
            light::summarize_gate(&platform.provider, &cfg.models.light, &gate_text.text).await;
        if let Some(r) = &response {
            ledger
                .model_call(Tier::Light, &cfg.models.light.id, &r.usage)
                .await?;
        }
        session = session.with_message(Message::user_text(format!(
            "The checks failed on your patch:\n\n{summary}\n\nFix the problem with the tools, \
             then reply with the final JSON answer again."
        )));
    }
}

/// Where a job's files go, for callers that want to inspect them.
pub fn job_dir(cfg: &RunConfig, issue_id: &str) -> PathBuf {
    cfg.run.state_dir.join("jobs").join(issue_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The PoC config with a Rust root and a Python `py/`, its state in a
    /// directory of its own.
    fn monorepo(sandbox: bool) -> RunConfig {
        let mut cfg: toml::Table =
            toml::from_str(include_str!("../../../config/poc.toml")).unwrap();
        let state = std::env::temp_dir().join(format!("aegis-parts-{}", std::process::id()));
        let run = cfg["run"].as_table_mut().unwrap();
        run.insert("state_dir".into(), state.display().to_string().into());
        run.insert("sandbox".into(), sandbox.into());
        let repo = cfg["repo"].as_table_mut().unwrap();
        repo.insert("language".into(), "rust".into());
        repo.insert(
            "dirs".into(),
            toml::from_str::<toml::Table>("d = [{ path = \"py\", language = \"python\" }]")
                .unwrap()["d"]
                .clone(),
        );
        RunConfig::from_toml(&toml::to_string(&cfg).unwrap()).unwrap()
    }

    #[test]
    fn codebase_memory_runs_in_the_index_directory_not_in_the_job_s_tree() {
        // #203: the daemon keeps its first server's directory, and a job's
        // tree is removed at the end of the job.
        let cfg = monorepo(false);
        let profiles = cfg.repo.profiles(&Profiles::builtin().unwrap()).unwrap();
        let job_dir = cfg.run.state_dir.join("jobs/cwd");
        std::fs::create_dir_all(&job_dir).unwrap();
        let tree = job_dir.join("repo");
        let specs = tool_specs(&cfg, &profiles, &tree, &job_dir).unwrap();
        let cbm = specs.iter().find(|s| s.name == code_graph::SERVER).unwrap();
        assert_eq!(cbm.cwd, Some(cfg.run.state_dir.join("index")));
        assert_eq!(cbm.tools, Some(vec![]));
    }

    #[test]
    fn the_parts_file_gives_each_part_the_mounts_of_its_gate_steps() {
        // #198: `run_tests` and the formatter of `py/` see the `.venv` the
        // gate's steps there do.
        let cfg = monorepo(true);
        let profiles = cfg.repo.profiles(&Profiles::builtin().unwrap()).unwrap();
        let file: toml::Table = toml::from_str(&parts_file(&cfg, &profiles).unwrap()).unwrap();
        let part = &file["part"].as_array().unwrap()[0];
        assert_eq!(part["dir"].as_str(), Some("py"));
        let mounts: Vec<(String, String)> = part["mounts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| {
                let host = m["host"].as_str().unwrap().to_owned();
                (host, m["path"].as_str().unwrap().to_owned())
            })
            .collect();
        let gate: Vec<(String, String)> = sandbox(&cfg, &profiles.parts()[1])
            .unwrap()
            .unwrap()
            .mounts
            .into_iter()
            .map(|(host, inside)| (host.display().to_string(), inside))
            .collect();
        assert_eq!(mounts, gate);
        assert!(
            mounts
                .iter()
                .any(|(host, inside)| inside == "/work/.venv" && host.contains("parts/py/deps")),
            "{mounts:?}"
        );
    }

    #[test]
    fn without_the_sandbox_a_part_mounts_nothing() {
        let cfg = monorepo(false);
        let profiles = cfg.repo.profiles(&Profiles::builtin().unwrap()).unwrap();
        let file: toml::Table = toml::from_str(&parts_file(&cfg, &profiles).unwrap()).unwrap();
        let part = &file["part"].as_array().unwrap()[0];
        assert_eq!(part["mounts"].as_array().map(Vec::len), Some(0));
    }
}
