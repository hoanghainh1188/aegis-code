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

use aegis_aci::{Workspace, load_or_create_summary};
use aegis_gate::{CancelToken, GateReport, GateRequest, StepName, StepStatus, run_gate};
use aegis_lang::{AllowList, Profiles, ResolvedProfile};
use aegis_ledger::{GateRun, IssueId, IssueLedger, LedgerStore, Outcome, PriceTable, Tier};
use aegis_orchestrator::{
    LoopConfig, LoopEnd, McpServerSpec, McpTools, ModelSpec, RepoPrompt, Session, run_loop,
    system_blocks,
};
use aegis_provider::{CacheTtl, ContentBlock, Message, ModelProvider, ProviderConfig, Role};
use aegis_router::{BudgetCheck, Decision, Router, Signal, Spent};
use tokio_util::sync::CancellationToken;

use crate::answer::{FinalAnswer, Status, parse_answer};
use crate::checks::{Halt, JobCheck, fold_edits};
use crate::job::{self, Worktree};
use crate::light;
use crate::publish::publish;
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
}

/// How the work on an issue ended, before the ledger closes it.
struct Ending {
    outcome: Outcome,
    reason: Option<String>,
    pr_url: Option<String>,
}

impl Ending {
    fn stop(outcome: Outcome, reason: impl Into<String>) -> Self {
        Self {
            outcome,
            reason: Some(reason.into()),
            pr_url: None,
        }
    }
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

    let ending = match prepare_and_work(&platform, &mut ledger, &issue, &cancel).await {
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
    let (class, response) = light::classify(&platform.provider, &cfg.models.light, issue).await;
    if let Some(r) = &response {
        ledger
            .model_call(Tier::Light, &cfg.models.light.id, &r.usage)
            .await?;
    }
    ledger.classified(class.kind).await?;
    if class.needs_sonar_batching() {
        return Ok(Ending::stop(
            Outcome::StoppedSteps,
            "Sonar issues go through the batching step (#17)",
        ));
    }
    let profile = platform
        .profiles
        .get(cfg.repo.language)?
        .resolve(&cfg.repo.profile_overrides()?)?;
    let first = issue.first_message(&class.summary, class.has_repro_info);
    work(platform, ledger, &profile, issue, first, cancel).await
}

fn tool_specs(
    cfg: &RunConfig,
    tree: &Path,
    job_dir: &Path,
) -> Result<Vec<McpServerSpec>, LifecycleError> {
    let overrides = job_dir.join("overrides.toml");
    let text = toml::to_string(&cfg.repo.overrides)
        .map_err(|e| LifecycleError::Config(format!("repo.overrides: {e}")))?;
    std::fs::write(&overrides, text).map_err(|e| LifecycleError::io(&overrides, &e))?;
    let mut specs = vec![McpServerSpec {
        name: "aci".into(),
        program: cfg.aci_bin().display().to_string(),
        args: vec![
            "--repo".into(),
            tree.display().to_string(),
            "--language".into(),
            cfg.repo.language.id().into(),
            "--overrides".into(),
            overrides.display().to_string(),
        ],
        cwd: None,
        env: vec![],
        tools: None,
    }];
    if let Some(cbm) = &cfg.codebase_memory {
        specs.push(McpServerSpec {
            name: "codebase-memory".into(),
            program: cbm.program.clone(),
            args: cbm.args.clone(),
            cwd: None,
            env: cbm.env.clone(),
            tools: Some(cbm.tools.clone()),
        });
    }
    Ok(specs)
}

/// Steps 2–10 on the job's own worktree.
async fn work<P, S>(
    platform: &Platform<P, S>,
    ledger: &mut IssueLedger<'_, S>,
    profile: &ResolvedProfile,
    issue: &IssueInput,
    first_message: String,
    cancel: &CancellationToken,
) -> Result<Ending, LifecycleError>
where
    P: ModelProvider + Send + Sync + 'static,
    S: LedgerStore + Send + Sync + 'static,
{
    let cfg = &platform.config;
    let base = job::resolve(&cfg.repo.path, &cfg.repo.base)?;
    let job_dir = cfg.run.state_dir.join("jobs").join(&issue.id);
    let tree = Worktree::create(&cfg.repo.path, &base, &job_dir.join("repo"))?;

    // BP2 is the same for every issue of the repo (design §4.4).
    let clone = Workspace::new(&cfg.repo.path, profile.clone())?;
    let map_path = cfg
        .run
        .state_dir
        .join("maps")
        .join(format!("{}.md", cfg.repo.key));
    let map = load_or_create_summary(&clone, &map_path)?;
    let system = system_blocks(&RepoPrompt {
        language_conventions: profile.profile.conventions(),
        repo_conventions: cfg.repo.conventions.as_deref(),
        module_summary: Some(&map),
    });

    let tools = McpTools::connect(&tool_specs(cfg, tree.path(), &job_dir)?).await?;
    let session = Session::new(
        system,
        Message::user_text(first_message),
        CacheTtl::FiveMinutes,
    );
    let result = drive(
        platform,
        ledger,
        profile,
        issue,
        &tools,
        session,
        tree.path(),
        &base,
        &job_dir,
        cancel,
    )
    .await;
    tools.shutdown().await;
    drop(tree);
    result
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

fn allowed_packages(cfg: &RunConfig) -> Result<BTreeSet<String>, LifecycleError> {
    let Some(path) = &cfg.repo.allowed_packages else {
        return Ok(BTreeSet::new());
    };
    let text = std::fs::read_to_string(path).map_err(|e| LifecycleError::io(path, &e))?;
    Ok(AllowList::from_toml(&text)?.names().clone())
}

/// The gate on the job's tree, off the async runtime; `cancel` stops it.
#[allow(clippy::too_many_arguments)]
async fn gate(
    cfg: &RunConfig,
    profile: &ResolvedProfile,
    tree: &Path,
    base: &str,
    job_dir: &Path,
    answer: &FinalAnswer,
    cancel: &CancellationToken,
) -> Result<GateReport, LifecycleError> {
    let allowed = allowed_packages(cfg)?;
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
        profile.clone(),
        allowed,
        answer.repro.clone(),
        job_dir.join("gate"),
        Duration::from_secs(cfg.run.step_timeout_s),
    );
    let report = tokio::task::spawn_blocking(move || {
        let (repo, base, profile, allowed, repro, work_dir, step_timeout) = owned;
        run_gate(&GateRequest {
            repo: &repo,
            base: &base,
            profile: &profile,
            allowed: &allowed,
            repro: repro.as_ref(),
            work_dir: &work_dir,
            env: vec![],
            step_timeout,
            cancel: gate_cancel,
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

fn gate_run(report: &GateReport, took: Duration) -> GateRun {
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

/// The Core/Expert loop with gate runs between attempts.
#[allow(clippy::too_many_arguments)]
async fn drive<P, S>(
    platform: &Platform<P, S>,
    ledger: &mut IssueLedger<'_, S>,
    profile: &ResolvedProfile,
    issue: &IssueInput,
    tools: &McpTools,
    session: Session,
    tree: &Path,
    base: &str,
    job_dir: &Path,
    cancel: &CancellationToken,
) -> Result<Ending, LifecycleError>
where
    P: ModelProvider + Send + Sync + 'static,
    S: LedgerStore + Send + Sync + 'static,
{
    let cfg = &platform.config;
    let regional = matches!(cfg.provider, ProviderConfig::Bedrock { .. });
    let mut router = Router::new(cfg.budget.rules());
    let mut session = session;
    let mut answer_retries = 0;
    let mut dearest_call_usd: f64 = 0.0;
    let mut first_expert_call = false;
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
        }
        let (after_edits, _) = fold_edits(router, &outcome.turns);
        if after_edits.tier() != router.tier() {
            // Escalated on the loop's last turn: Expert takes over from here.
            first_expert_call = true;
        }
        router = after_edits;
        session = outcome.session;

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
                Some(Halt::Escalate(_)) => {
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

        let gate_started = Instant::now();
        let report = gate(cfg, profile, tree, base, job_dir, &answer, cancel).await?;
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
            let published =
                publish(platform, ledger, profile, tree, issue, &answer, &report).await?;
            return Ok(Ending {
                outcome: Outcome::PrOpened,
                reason: None,
                pr_url: Some(published.url),
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
        if let Decision::Escalate(_) = decision {
            first_expert_call = true;
        }
        if cancel.is_cancelled() {
            return Ok(Ending::stop(Outcome::StoppedSteps, "cancelled"));
        }
        let (summary, response) =
            light::summarize_gate(&platform.provider, &cfg.models.light, &report.to_string()).await;
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
