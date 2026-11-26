//! A feature delivered as one integration PR (#678, ADR 0019 §4). Its
//! integration branch is the branch of the job that planned it,
//! `agent/<id>`, so the PR at the end is an agent PR like any other, and
//! its review rounds work on it.
//!
//! Each sub-job, once its own gate passed, merges its commit into the
//! branch's tip as it is then, runs the gate again on the merge, and moves
//! the branch there without force: parts that run side by side meet on the
//! branch one at a time, and a part that conflicts, or fails the gate once
//! merged, stops without moving it. The job that planned the feature, once
//! every part is in, gates the branch against the main branch and opens
//! the one PR.

use std::path::Path;
use std::process::Command;
use std::time::Instant;

use aegis_gate::{DependencyPolicy, GateReport, TestMode};
use aegis_ledger::{IssueLedger, LedgerStore, Outcome};
use aegis_pr::ChangeNote;
use aegis_provider::ModelProvider;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::answer::{FinalAnswer, Status};
use crate::job::{self, Worktree};
use crate::publish::{PublishContext, fork_remote, publish};
use crate::run::{Ending, GatePolicy, JobTree, fetch_branch, gate, gate_run};
use crate::{FeaturePlan, FeatureTask, IssueInput, LifecycleError, Platform};

/// How many times a part fetches the branch again when another part
/// moved it between its fetch and its push.
const MAX_PUSH_TRIES: usize = 3;

/// What the job that planned a feature opens its one PR from (#678): the
/// plan as approved, and its integration branch, every part merged.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FeatureDelivery {
    pub plan: FeaturePlan,
    /// `agent/<id>` of the job that planned it.
    pub branch: String,
}

impl FeatureDelivery {
    /// The feature's answer, for its PR: the plan's summary, each part.
    fn answer(&self) -> FinalAnswer {
        FinalAnswer {
            status: Status::Fixed,
            cause: self.plan.summary.clone(),
            evidence: vec![],
            changes: self
                .plan
                .sub_jobs
                .iter()
                .map(|s| ChangeNote {
                    location: s.scope.join(", "),
                    note: s.title.clone(),
                })
                .collect(),
            repro: None,
            allowed_test_changes: vec![],
            allowed_reference_changes: vec![],
            stop_reason: None,
            review_replies: vec![],
            suspicious_input: vec![],
        }
    }

    /// The whole feature's gate: the packages its plan proposed, and the
    /// repo's own tests; each part's acceptance tests passed with its
    /// merge.
    fn policy(&self) -> GatePolicy {
        GatePolicy {
            dependencies: DependencyPolicy::AllowProposed(
                self.plan
                    .dependencies
                    .iter()
                    .map(|d| aegis_gate::ProposedDependency {
                        package: d.package.clone(),
                        version: d.version.clone(),
                    })
                    .collect(),
            ),
            tests: TestMode::Reproduce,
        }
    }

    /// The section the feature's PR begins with.
    pub(crate) fn pr_section(&self, issue_url: &str, locale: aegis_pr::Locale) -> String {
        let t = locale.strings();
        format!(
            "## {}\n{}\n\n",
            t.feature_heading,
            (t.feature_whole)(self.plan.sub_jobs.len(), issue_url)
        )
    }
}

/// The id of an `agent/<id>` branch.
fn branch_id(branch: &str) -> Result<&str, LifecycleError> {
    branch
        .strip_prefix("agent/")
        .filter(|id| !id.is_empty())
        .ok_or_else(|| LifecycleError::Config(format!("`{branch}` is not an agent branch")))
}

/// What [`integrate`] merges: a sub-job's patch that passed its gate.
pub(crate) struct Part<'a> {
    pub profiles: &'a aegis_lang::RepoProfiles,
    pub issue: &'a IssueInput,
    pub task: &'a FeatureTask,
    pub answer: &'a FinalAnswer,
    pub report: &'a GateReport,
    pub policy: &'a GatePolicy,
    pub job_dir: &'a Path,
}

/// A sub-job's end in `integration_pr` (#678): its gated tree committed on
/// the commit it started from, merged into the integration branch's tip,
/// gated there, and the branch moved to the merge. It ends
/// [`Outcome::Integrated`], or stops with why the branch is left as it was.
pub(crate) async fn integrate<P, S>(
    platform: &Platform<P, S>,
    ledger: &mut IssueLedger<'_, S>,
    part: Part<'_>,
    cancel: &CancellationToken,
) -> Result<Ending, LifecycleError>
where
    P: ModelProvider + Send + Sync + 'static,
    S: LedgerStore + Send + Sync + 'static,
{
    let cfg = &platform.config;
    let Part {
        profiles,
        issue,
        task,
        answer,
        report,
        policy,
        job_dir,
    } = part;
    let Some(branch) = task.integration_branch.as_deref() else {
        return Err(LifecycleError::Config(
            "a part of an integration PR with no integration branch".into(),
        ));
    };
    let id = branch_id(branch)?;
    let title = task.planned().map(|s| s.title.clone()).unwrap_or_default();
    let message = aegis_gate::secrets::redact(&format!(
        "Feature part {}: {}",
        task.sub_job,
        title.lines().next().unwrap_or_default()
    ))
    .text;
    let author = aegis_sonar_batch::Author {
        name: cfg.identity.name.clone(),
        email: cfg.identity.email.clone(),
    };
    let clone = &cfg.repo.path;
    let commit =
        aegis_sonar_batch::commit_tree(clone, &report.tree, &report.base, &message, &author)?;
    let patch = Some(job::patch(clone, &report.base, &report.tree)?);
    let mut last = String::new();
    for attempt in 0..MAX_PUSH_TRIES {
        if cancel.is_cancelled() {
            return Ok(Ending::stop(Outcome::StoppedSteps, "cancelled"));
        }
        // No branch yet: this part is the first in.
        let tip = fetch_branch(platform, branch, &format!("{}-{attempt}", issue.id))
            .await
            .ok();
        let merged = match tip.as_deref() {
            None => commit.clone(),
            Some(tip) if tip == report.base => commit.clone(),
            Some(tip) => {
                let tree =
                    Worktree::create(clone, tip, &job_dir.join(format!("integration-{attempt}")))?;
                let merge_message = format!("Merge feature part {} into {branch}", task.sub_job);
                if !merge(tree.path(), &commit, &merge_message, &author)? {
                    return Ok(Ending {
                        patch,
                        ..Ending::stop(
                            Outcome::StoppedSteps,
                            format!(
                                "the part conflicts with the feature's integration branch \
                                 `{branch}`; run it again to start from the branch as it is now"
                            ),
                        )
                    });
                }
                let started = Instant::now();
                let job = JobTree {
                    tree: tree.path(),
                    base: tip,
                    job_dir,
                };
                let checked = gate(cfg, profiles, job, answer, policy, cancel).await?;
                ledger
                    .gate_run(gate_run(&checked, started.elapsed()))
                    .await?;
                if !checked.passed() {
                    let failed = checked
                        .steps
                        .iter()
                        .find(|s| s.status == aegis_gate::StepStatus::Failed)
                        .map_or("a step", |s| s.name.id());
                    return Ok(Ending {
                        patch,
                        ..Ending::stop(
                            Outcome::StoppedSteps,
                            format!(
                                "the gate fails once the part is merged into `{branch}` \
                                 ({failed}); the branch is left as it was"
                            ),
                        )
                    });
                }
                head(tree.path())?
            }
        };
        if !cfg.run.open_prs {
            return Ok(Ending {
                outcome: Outcome::Integrated,
                reason: Some("no branch pushed".into()),
                pr_url: None,
                kept: None,
                patch,
                diagnosis: None,
                plan: None,
            });
        }
        let remote = fork_remote(platform).await?;
        let (clone, id, merged_at) = (clone.clone(), id.to_owned(), merged.clone());
        let pushed = tokio::task::spawn_blocking(move || {
            aegis_pr::push_commit(&clone, &id, &merged_at, &remote)
        })
        .await
        .map_err(|e| LifecycleError::Config(format!("pushing the integration branch: {e}")))?;
        match pushed {
            Ok(_) => {
                return Ok(Ending {
                    outcome: Outcome::Integrated,
                    reason: Some(format!(
                        "merged into `{branch}`; the feature's PR opens once every part is in"
                    )),
                    pr_url: None,
                    kept: None,
                    patch,
                    diagnosis: None,
                    plan: None,
                });
            }
            // Another part moved the branch since: merge on its new tip.
            Err(e) => last = e.to_string(),
        }
    }
    Ok(Ending {
        patch,
        ..Ending::stop(
            Outcome::StoppedSteps,
            format!("the integration branch `{branch}` could not be moved: {last}"),
        )
    })
}

/// The feature's one PR (#678): its integration branch, every part in,
/// gated against the main branch as it is, and opened from that branch.
pub(crate) async fn deliver<P, S>(
    platform: &Platform<P, S>,
    ledger: &mut IssueLedger<'_, S>,
    issue: &IssueInput,
    delivery: &FeatureDelivery,
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
    let branch = &delivery.branch;
    if branch_id(branch)? != issue.id {
        return Err(LifecycleError::Config(format!(
            "`{branch}` is not the branch of `{}`",
            issue.id
        )));
    }
    let clone = &cfg.repo.path;
    let tip = match fetch_branch(platform, branch, &issue.id).await {
        Ok(tip) => tip,
        Err(e) => {
            return Ok(Ending::stop(
                Outcome::StoppedSteps,
                format!("the feature's integration branch `{branch}` cannot be read: {e}"),
            ));
        }
    };
    let main = job::resolve(clone, &cfg.repo.base)?;
    let base = job::merge_base(clone, &tip, &main)?;
    let job_dir = cfg.run.state_dir.join("jobs").join(&issue.id);
    let tree = Worktree::create(clone, &tip, &job_dir.join("delivery"))?;
    let profiles = cfg.repo.profiles(&platform.profiles)?;
    let answer = delivery.answer();
    let started = Instant::now();
    let job = JobTree {
        tree: tree.path(),
        base: &base,
        job_dir: &job_dir,
    };
    let report = gate(cfg, &profiles, job, &answer, &delivery.policy(), cancel).await?;
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
            format!(
                "the feature's integration gate fails on `{branch}` ({failed}); no PR is opened"
            ),
        ));
    }
    let patch = Some(job::patch(clone, &base, &report.tree)?);
    if !cfg.run.open_prs {
        return Ok(Ending {
            outcome: Outcome::PrOpened,
            reason: Some("no PR opened".into()),
            pr_url: None,
            kept: None,
            patch,
            diagnosis: None,
            plan: None,
        });
    }
    let fix = PublishContext {
        profiles: &profiles,
        tree: tree.path(),
        issue,
        answer: &answer,
        gate: &report,
        flags: &[],
        feature: None,
        whole: Some((delivery, &tip)),
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

/// Merges `commit` into the checkout at `tree`; `false` on a conflict,
/// with the merge undone.
fn merge(
    tree: &Path,
    commit: &str,
    message: &str,
    author: &aegis_sonar_batch::Author,
) -> Result<bool, LifecycleError> {
    let run = |args: &[&str]| {
        Command::new("git")
            .arg("-C")
            .arg(tree)
            .args(["-c", &format!("user.name={}", author.name)])
            .args(["-c", &format!("user.email={}", author.email)])
            .args(args)
            .output()
            .map_err(|e| LifecycleError::io(tree, &e))
    };
    let out = run(&["merge", "--no-ff", "--no-edit", "-m", message, commit])?;
    if out.status.success() {
        return Ok(true);
    }
    if run(&["rev-parse", "-q", "--verify", "MERGE_HEAD"])?
        .status
        .success()
    {
        run(&["merge", "--abort"])?;
        return Ok(false);
    }
    Err(LifecycleError::Config(format!(
        "merging the part: {}",
        String::from_utf8_lossy(&out.stderr).trim()
    )))
}

fn head(tree: &Path) -> Result<String, LifecycleError> {
    job::resolve(tree, "HEAD")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_agent_branch_is_an_integration_branch() {
        assert_eq!(branch_id("agent/shop-1").unwrap(), "shop-1");
        assert!(branch_id("main").is_err());
        assert!(branch_id("agent/").is_err());
    }
}
