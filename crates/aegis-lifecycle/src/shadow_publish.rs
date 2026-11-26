//! A shadow job's PR, opened later from what it kept (#699): its patch on
//! the commit it was gated against, gated again, and published with its
//! answer. The agent does not run again.

use std::io::Write as _;
use std::process::{Command, Stdio};
use std::time::Instant;

use aegis_ledger::{IssueLedger, LedgerStore, Outcome, ShadowKept};
use aegis_provider::ModelProvider;
use tokio_util::sync::CancellationToken;

use crate::answer::{Status, parse_answer};
use crate::job::{self, Worktree};
use crate::publish::{PublishContext, publish};
use crate::run::{Ending, GatePolicy, JobTree, gate, gate_run};
use crate::{IssueInput, LifecycleError, Platform};

/// What a shadow job left to open its PR from: what its ledger kept, and
/// the patch the store kept for its page.
#[derive(Debug, Clone, PartialEq)]
pub struct ShadowPatch {
    pub kept: ShadowKept,
    /// The whole patch, as `git diff` wrote it; never one the store cut.
    pub diff: String,
}

/// Applies `diff` to the checkout at `tree`, left unstaged as the agent
/// leaves its edits; `Err` with git's reason.
fn apply(tree: &std::path::Path, diff: &str) -> Result<(), String> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(tree)
        .args(["apply", "--whitespace=nowarn", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    child
        .stdin
        .take()
        .ok_or("no stdin for git apply")?
        // The store keeps the diff as git printed it, trimmed: `git apply`
        // needs its last line ended.
        .write_all(format!("{}\n", diff.trim_end_matches('\n')).as_bytes())
        .map_err(|e| e.to_string())?;
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
    }
}

/// The PR of the shadow job `shadow` came from, under `issue`'s own
/// ledger: the patch applied on the commit it was gated against, the gate
/// run again there, then published as any job's PR.
pub(crate) async fn open<P, S>(
    platform: &Platform<P, S>,
    ledger: &mut IssueLedger<'_, S>,
    issue: &IssueInput,
    shadow: &ShadowPatch,
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
    let answer = match parse_answer(&shadow.kept.answer) {
        Ok(a) if a.status == Status::Fixed => a,
        Ok(_) => {
            return Ok(Ending::stop(
                Outcome::StoppedSteps,
                "the shadow job's answer is not a fix; no PR is opened",
            ));
        }
        Err(e) => {
            return Ok(Ending::stop(
                Outcome::StoppedSteps,
                format!("the shadow job's answer cannot be read ({e}); no PR is opened"),
            ));
        }
    };
    let clone = &cfg.repo.path;
    let base = match job::resolve(clone, &shadow.kept.base) {
        Ok(base) => base,
        Err(_) => {
            return Ok(Ending::stop(
                Outcome::StoppedSteps,
                format!(
                    "the commit the shadow job was gated against, `{}`, is not in the \
                     repo any more; run the ticket again",
                    shadow.kept.base
                ),
            ));
        }
    };
    let job_dir = cfg.run.state_dir.join("jobs").join(&issue.id);
    let tree = Worktree::create(clone, &base, &job_dir.join("shadow"))?;
    if let Err(why) = apply(tree.path(), &shadow.diff) {
        return Ok(Ending::stop(
            Outcome::StoppedSteps,
            format!("the shadow job's patch does not apply on `{base}` ({why}); no PR is opened"),
        ));
    }
    let profiles = cfg.repo.profiles(&platform.profiles)?;
    let started = Instant::now();
    let job = JobTree {
        tree: tree.path(),
        base: &base,
        job_dir: &job_dir,
    };
    let report = gate(cfg, &profiles, job, &answer, &GatePolicy::locked(), cancel).await?;
    ledger
        .gate_run(gate_run(&report, started.elapsed()))
        .await?;
    if !report.passed() {
        let failed = report
            .steps
            .iter()
            .find(|s| s.status == aegis_gate::StepStatus::Failed)
            .map_or("a step", |s| s.name.id());
        return Ok(Ending::stop(
            Outcome::StoppedSteps,
            format!("the shadow job's patch fails the gate again ({failed}); no PR is opened"),
        ));
    }
    let patch = Some(job::patch(clone, &base, &report.tree)?);
    if !cfg.run.open_prs {
        return Ok(Ending::no_pr(cfg, patch));
    }
    let fix = PublishContext {
        profiles: &profiles,
        tree: tree.path(),
        issue,
        answer: &answer,
        gate: &report,
        flags: &[],
        feature: None,
        whole: None,
    };
    let published = publish(platform, ledger, fix).await?;
    Ok(Ending {
        outcome: Outcome::PrOpened,
        reason: published.note,
        pr_url: Some(published.url),
        kept: None,
        patch,
        diagnosis: None,
        plan: None,
    })
}
