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

use aegis_aci::{MAP_FORMAT, REDACTED_MARKER, Workspace, load_or_create_summary, redact};
use aegis_gate::{
    BuildCache, CLIENT_ENV, CancelToken, DependencyPolicy, Egress, FetchStep, GatePart, GateReport,
    GateRequest, Hardening, ReferenceGroup, Runner, Sandbox, SonarStep, StepName, StepStatus,
    WORKDIR, failing_tests_with, run_gate,
};
use aegis_lang::{AllowList, ProfilePart, Profiles, RepoProfiles, ResolvedProfile};
use aegis_ledger::{
    AnswerChange, AnswerReply, AnswerSummary, AnswerSuspicious, GateRun, InputFlag, IssueId,
    IssueKind, IssueLedger, LedgerStore, Outcome, PriceTable, Tier, ToolCall,
};
use aegis_orchestrator::{
    INHERITED_ENV, LoopConfig, LoopEnd, McpServerSpec, McpTools, ModelSpec, RepoPrompt, Session,
    TurnRecord, run_loop_observed, system_blocks,
};
use aegis_provider::{CacheTtl, ContentBlock, Message, ModelProvider, ProviderConfig, Role};
use aegis_router::{BudgetCheck, Classification, Decision, Router, Signal, Spent};
use aegis_sonar_batch::{Author, commit_tree};
use tokio_util::sync::CancellationToken;

use crate::answer::{FinalAnswer, Status, parse_answer};
use crate::batch::sonar_batch;
use crate::checks::{Halt, JobCheck, fold_tools, test_reminder};
use crate::code_graph::{self, CodeGraph, ServerCalls, changed_functions};
use crate::cve;
use crate::evidence::check_sources;
use crate::issue::HiddenTests;
use crate::job;
use crate::light;
use crate::publish::{PublishContext, fork_remote, publish, publish_round};
use crate::review::{MAX_DIFF_CHARS, ReviewRound};
use crate::{IssueInput, LifecycleError, RunConfig};

mod checkpoint;
mod drive;
mod env;
mod gating;

/// A job's sandbox, fetch step and build cache for one part of a repo, for
/// the pre-flight check's baseline gate (#599).
pub(crate) use gating::{
    build_cache as gate_build_cache, fetch_step as gate_fetch, sandbox as gate_sandbox,
};
mod pool;
mod prepare;
mod remote_cache;
mod review;
mod tools;

use drive::*;
use env::{ExecutionEnv, JobEnv};
pub(crate) use gating::*;
pub(crate) use prepare::*;
use review::*;
use tools::*;

/// Final answers the agent may get wrong before the job stops.
const MAX_ANSWER_RETRIES: u32 = 2;
/// The unchecked places listed back to the agent, or in the stop reason.
const MAX_SOURCE_COMPLAINTS: usize = 10;

/// What every job of a run shares. Built once; jobs hold it in an `Arc`.
pub struct Platform<P, S> {
    pub config: RunConfig,
    pub provider: P,
    pub store: S,
    pub prices: PriceTable,
    pub profiles: Profiles,
    /// For the fork: push and open PRs. A fixed token on the token path; on
    /// the app path a fresh installation token at each use (ADR 0017).
    pub github: aegis_pr::TokenSource,
}

impl<P, S> std::fmt::Debug for Platform<P, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Platform")
            .field("repo", &self.config.repo.key)
            .field("github", &self.github)
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
    /// What the PR holds, when one was opened or updated (#312).
    pub patch: Option<aegis_store::Patch>,
    /// The report the diagnosis lane wrote for the ticket (#451), for a job
    /// that ended [`Outcome::Diagnosed`]; a feature's RFC (#677), for a job
    /// that ended [`Outcome::AwaitingApproval`].
    pub diagnosis: Option<String>,
    /// The feature's plan (#677), for a job that ended
    /// [`Outcome::AwaitingApproval`]: the store keeps it for the approval.
    pub plan: Option<crate::FeaturePlan>,
}

/// How the work on an issue ended, before the ledger closes it.
pub(crate) struct Ending {
    pub outcome: Outcome,
    pub reason: Option<String>,
    pub pr_url: Option<String>,
    /// [`Finish::Keep`]: the commit of the tree the gate passed.
    pub kept: Option<Kept>,
    /// The patch of the PR opened or updated.
    pub patch: Option<aegis_store::Patch>,
    /// The diagnosis lane's report (#451), or a feature's RFC (#677).
    pub diagnosis: Option<String>,
    /// A feature's plan (#677).
    pub plan: Option<crate::FeaturePlan>,
}

impl Ending {
    pub(crate) fn stop(outcome: Outcome, reason: impl Into<String>) -> Self {
        Self {
            outcome,
            reason: Some(reason.into()),
            pr_url: None,
            kept: None,
            patch: None,
            diagnosis: None,
            plan: None,
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
    /// A feature's sub-job (#678): its acceptance tests gated, its PR on
    /// what it started from.
    Feature(Box<crate::FeatureTask>),
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
    run(platform, issue, Stage::Issue, cancel).await
}

/// Plans `issue`'s feature again (#677), as `revision` asks: the Architect
/// phase goes on under the same ledger from the plan a person sent back
/// with feedback, and the job waits for approval again.
pub async fn run_feature_revision<P, S>(
    platform: Arc<Platform<P, S>>,
    issue: IssueInput,
    revision: crate::FeatureRevision,
    cancel: CancellationToken,
) -> Result<IssueReport, LifecycleError>
where
    P: ModelProvider + Send + Sync + 'static,
    S: LedgerStore + Send + Sync + 'static,
{
    run(platform, issue, Stage::Revision(Box::new(revision)), cancel).await
}

/// Runs `issue`'s sub-job `task` of an approved feature (#678) under
/// `issue.id`, its own ledger: the part of the plan it is, from what it is
/// stacked on, to a PR of its own.
pub async fn run_feature_sub_job<P, S>(
    platform: Arc<Platform<P, S>>,
    issue: IssueInput,
    task: crate::FeatureTask,
    cancel: CancellationToken,
) -> Result<IssueReport, LifecycleError>
where
    P: ModelProvider + Send + Sync + 'static,
    S: LedgerStore + Send + Sync + 'static,
{
    run(platform, issue, Stage::SubJob(Box::new(task)), cancel).await
}

/// What a run does.
enum Stage {
    Issue,
    Review(ReviewRound),
    Revision(Box<crate::FeatureRevision>),
    SubJob(Box<crate::FeatureTask>),
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
    run(platform, issue, Stage::Review(round), cancel).await
}

async fn run<P, S>(
    platform: Arc<Platform<P, S>>,
    issue: IssueInput,
    stage: Stage,
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
    // A plan waiting for approval goes on when it is revised (#677).
    let revising = matches!(stage, Stage::Revision(_));
    match ledger.record().outcome {
        None => {}
        Some(Outcome::AwaitingApproval) if revising => {}
        Some(outcome) => {
            return Err(LifecycleError::Config(format!(
                "issue `{}` already ended ({outcome:?}); run it under a new id",
                issue.id
            )));
        }
    }
    if revising && ledger.record().outcome.is_none() {
        return Err(LifecycleError::Config(format!(
            "issue `{}` has no plan waiting to be revised",
            issue.id
        )));
    }

    let prepared = match stage {
        Stage::Review(round) => {
            review_and_work(&platform, &mut ledger, &issue, round, &cancel).await
        }
        Stage::Revision(revision) => {
            crate::feature::revise(&platform, &mut ledger, &issue, &revision).await
        }
        Stage::SubJob(task) => {
            sub_job_and_work(&platform, &mut ledger, &issue, *task, &cancel).await
        }
        Stage::Issue => prepare_and_work(&platform, &mut ledger, &issue, &cancel).await,
    };
    let ending = match prepared {
        Ok(ending) => ending,
        // The ledger still gets an outcome, so the run is not left open.
        Err(e) => Ending::stop(
            Outcome::StoppedSteps,
            format!("{}{e}", crate::PLATFORM_ERROR),
        ),
    };

    ledger
        .finished(ending.outcome, started.elapsed().as_secs_f64())
        .await?;
    // The job ended: nothing to go on from (#662).
    if let Err(e) = platform
        .store
        .clear_checkpoint(&ledger.record().issue_id)
        .await
    {
        tracing::warn!(issue = %issue.id, error = %e, "removing the checkpoint");
    }
    let record = ledger.record();
    Ok(IssueReport {
        issue_id: issue.id,
        outcome: ending.outcome,
        reason: ending.reason,
        pr_url: ending.pr_url,
        patch: ending.patch,
        diagnosis: ending.diagnosis,
        plan: ending.plan,
        cost_usd: record.cost_usd,
        model_calls: record.model_calls,
        hidden_tests_passed: record.hidden_tests_passed,
    })
}

/// Embedded instructions in what the job read (#444), from the run's tool
/// calls so far, recorded in the ledger.
async fn flag_inputs<P, S>(
    platform: &Platform<P, S>,
    ledger: &mut IssueLedger<'_, S>,
    issue: &IssueInput,
    round: Option<&ReviewRound>,
    base: &str,
) -> Result<Vec<InputFlag>, LifecycleError>
where
    S: LedgerStore,
{
    let events = platform.store.events(&ledger.record().issue_id).await?;
    let flags = crate::flags::collect(issue, round, &events, &platform.config.repo.path, base)?;
    ledger.input_flagged(&flags).await?;
    Ok(flags)
}

/// The ledger's summary of a final answer (#316), every text redacted as
/// anything else the platform writes from the model; the values hidden.
fn summary_of(answer: &FinalAnswer) -> (AnswerSummary, u32) {
    let mut hidden = 0;
    let mut clean = |text: &str| {
        let r = redact(text);
        hidden += r.count();
        r.text
    };
    let summary = AnswerSummary {
        fixed: answer.status == Status::Fixed,
        cause: clean(&answer.cause),
        changes: answer
            .changes
            .iter()
            .map(|c| AnswerChange {
                location: clean(&c.location),
                note: clean(&c.note),
            })
            .collect(),
        repro_test: answer.repro.as_ref().map(|r| clean(&r.test)),
        stop_reason: answer.stop_reason.as_deref().map(&mut clean),
        review_replies: answer
            .review_replies
            .iter()
            .map(|r| AnswerReply {
                note: r.note,
                done: r.done,
                reply: clean(&r.reply),
            })
            .collect(),
        suspicious_input: answer
            .suspicious_input
            .iter()
            .map(|s| AnswerSuspicious {
                location: clean(&s.location),
                request: clean(&s.request),
            })
            .collect(),
    };
    (summary, count(hidden))
}
fn ticket_meta(cfg: &RunConfig, issue: &IssueInput) -> aegis_ledger::TicketMeta {
    let source = issue
        .source
        .clone()
        .or_else(|| issue.cve.as_ref().map(|_| "cve".to_owned()))
        .or_else(|| match issue.kind {
            Some(IssueKind::Sonar) => Some("sonar".to_owned()),
            Some(IssueKind::CiFailure) => Some("ci".to_owned()),
            _ => None,
        });
    aegis_ledger::TicketMeta {
        programming_language: Some(cfg.repo.language.to_string().to_ascii_lowercase()),
        ticket_source: source,
        ticket_language: Some(aegis_ledger::detect_ticket_language(
            &issue.title,
            &issue.body,
        )),
        repo: (!cfg.repo.key.is_empty()).then(|| cfg.repo.key.clone()),
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
            toml::from_str(include_str!("../../../../config/poc.toml")).unwrap();
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
    fn a_go_and_typescript_monorepo_picks_each_file_s_profile_and_image() {
        // #598: `[[repo.dirs]]` makes a polyglot monorepo; a file goes by
        // the deepest directory that holds it, the rest by the root.
        let mut cfg = monorepo(true);
        cfg.repo.language = aegis_lang::Language::Go;
        cfg.repo.dirs = vec![toml::from_str("path = \"web\"\nlanguage = \"typescript\"").unwrap()];
        let profiles = cfg.repo.profiles(&Profiles::builtin().unwrap()).unwrap();
        let image = |file: &str| {
            let part = profiles.part_of(file);
            let sandbox = sandbox(&cfg, part).unwrap().unwrap();
            (part.dir.clone(), part.profile.language(), sandbox.image)
        };
        let go = (
            "".to_owned(),
            aegis_lang::Language::Go,
            "aegis-sandbox-go".to_owned(),
        );
        let ts = (
            "web".to_owned(),
            aegis_lang::Language::TypeScript,
            "aegis-sandbox-node".to_owned(),
        );
        assert_eq!(image("cart.go"), go);
        assert_eq!(image("internal/pay/pay.go"), go);
        assert_eq!(image("web/src/cart.ts"), ts);
        assert_eq!(image("web/test/cart.test.ts"), ts);
        // Not under `web/`: the root's.
        assert_eq!(image("webhooks/hook.go"), go);
        // Each part's lockfile is checked by its own profile.
        let lockfiles: Vec<(String, Vec<String>)> = profiles
            .parts()
            .iter()
            .map(|p| {
                (
                    p.dir.clone(),
                    p.profile.profile.dependencies.lockfiles.clone(),
                )
            })
            .collect();
        assert_eq!(lockfiles[0].0, "");
        assert!(
            lockfiles[0].1.contains(&"go.sum".to_owned()),
            "{lockfiles:?}"
        );
        assert_eq!(lockfiles[1].0, "web");
        assert!(
            lockfiles[1].1.contains(&"package-lock.json".to_owned()),
            "{lockfiles:?}"
        );
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
    fn the_aci_reaches_the_daemon_the_gate_does() {
        // #278: its formatter and `run_tests` run `docker` too; MCP
        // servers otherwise get only PATH, HOME and a few more.
        let env = docker_client_env(|k| {
            ["PATH", "DOCKER_HOST", "ANTHROPIC_API_KEY"]
                .contains(&k)
                .then(|| format!("<{k}>"))
        });
        assert_eq!(
            env,
            [("DOCKER_HOST".to_owned(), "<DOCKER_HOST>".to_owned())]
        );

        let cfg = monorepo(true);
        let profiles = cfg.repo.profiles(&Profiles::builtin().unwrap()).unwrap();
        let job_dir = cfg.run.state_dir.join("jobs/docker-env");
        std::fs::create_dir_all(&job_dir).unwrap();
        let specs = tool_specs(&cfg, &profiles, &job_dir.join("repo"), &job_dir).unwrap();
        let aci = specs.iter().find(|s| s.name == "aci").unwrap();
        assert_eq!(aci.env, docker_client_env(|k| std::env::var(k).ok()));
        let cfg = monorepo(false);
        let profiles = cfg.repo.profiles(&Profiles::builtin().unwrap()).unwrap();
        let specs = tool_specs(&cfg, &profiles, &job_dir.join("repo"), &job_dir).unwrap();
        assert_eq!(specs[0].env, []);
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
    fn run_tests_gets_the_sidecars_the_gate_does() {
        // #491: the agent's `run_tests` reaches the repo's services.
        let mut cfg = monorepo(true);
        cfg.repo.sidecars = vec![
            toml::from_str(
                r#"name = "postgres"
image = "postgres:16-alpine"
user = "70:70"
ready = ["pg_isready"]
test_env = { DATABASE_URL = "postgres://postgres:5432/t" }"#,
            )
            .unwrap(),
        ];
        let profiles = cfg.repo.profiles(&Profiles::builtin().unwrap()).unwrap();
        let job_dir = cfg.run.state_dir.join("jobs/sidecars");
        std::fs::create_dir_all(&job_dir).unwrap();
        let aci = |cfg: &RunConfig| {
            let specs = tool_specs(cfg, &profiles, &job_dir.join("repo"), &job_dir).unwrap();
            specs.into_iter().find(|s| s.name == "aci").unwrap().args
        };
        let args = aci(&cfg);
        let at = args.iter().position(|a| a == "--sidecars").unwrap();
        let file = std::fs::read_to_string(&args[at + 1]).unwrap();
        #[derive(serde::Deserialize)]
        struct File {
            sidecar: Vec<aegis_gate::SidecarSpec>,
        }
        let read: File = toml::from_str(&file).unwrap();
        assert_eq!(read.sidecar, cfg.repo.sidecars);
        // None declared: no flag.
        cfg.repo.sidecars.clear();
        assert!(!aci(&cfg).contains(&"--sidecars".to_owned()));
    }

    #[test]
    fn the_runs_sandbox_limits_reach_the_gate_and_the_aci() {
        // #320: the gate's containers and the ACI's are bounded alike.
        let mut cfg = monorepo(true);
        cfg.run.sandbox_limits = aegis_gate::Limits {
            memory_mb: 3072,
            cpus: 2,
            pids: 512,
            tmp_mb: 128,
        };
        let profiles = cfg.repo.profiles(&Profiles::builtin().unwrap()).unwrap();
        let root = &profiles.parts()[0];
        let gate = sandbox(&cfg, root).unwrap().unwrap().hardening;
        assert_eq!(gate.limits, cfg.run.sandbox_limits);
        assert!(gate.read_only_rootfs);
        assert_eq!(gate.writable, root.profile.profile.sandbox.writable);
        assert!(!gate.writable.is_empty());

        let job_dir = cfg.run.state_dir.join("jobs/limits");
        std::fs::create_dir_all(&job_dir).unwrap();
        let specs = tool_specs(&cfg, &profiles, &job_dir.join("repo"), &job_dir).unwrap();
        let args = specs
            .iter()
            .find(|s| s.name == "aci")
            .unwrap()
            .args
            .join(" ");
        assert!(
            args.contains(
                "--sandbox-memory-mb 3072 --sandbox-cpus 2 --sandbox-pids 512 \
                 --sandbox-tmp-mb 128"
            ),
            "{args}"
        );
    }

    #[test]
    fn the_runs_sandbox_runtime_reaches_the_gate_and_the_aci() {
        // #519: unset, nothing changes; set, every container of the repo's
        // code runs on it.
        let mut cfg = monorepo(true);
        let profiles = cfg.repo.profiles(&Profiles::builtin().unwrap()).unwrap();
        let root = &profiles.parts()[0];
        let aci_args = |cfg: &RunConfig, name: &str| {
            let job_dir = cfg.run.state_dir.join(format!("jobs/{name}"));
            std::fs::create_dir_all(&job_dir).unwrap();
            tool_specs(cfg, &profiles, &job_dir.join("repo"), &job_dir)
                .unwrap()
                .into_iter()
                .find(|s| s.name == "aci")
                .unwrap()
                .args
                .join(" ")
        };
        assert_eq!(
            sandbox(&cfg, root).unwrap().unwrap().hardening.runtime,
            None
        );
        assert!(!aci_args(&cfg, "default").contains("--sandbox-runtime"));

        cfg.run.sandbox_runtime = Some("runsc".into());
        let gate = sandbox(&cfg, root).unwrap().unwrap().hardening;
        assert_eq!(gate.runtime.as_deref(), Some("runsc"));
        assert!(aci_args(&cfg, "gvisor").contains("--sandbox-runtime runsc"));
    }

    #[test]
    fn a_runtime_that_could_be_a_flag_is_refused_on_load() {
        let mut cfg = monorepo(true);
        cfg.run.sandbox_runtime = Some("--privileged".into());
        let err = cfg.check().unwrap_err().to_string();
        assert!(err.contains("run.sandbox_runtime"), "{err}");
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
