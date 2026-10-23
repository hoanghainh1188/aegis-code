//! Step 6 of design §6.1 (#91): the scanner on the checked tree, then the
//! server's verdict through the Web API: quality gate ("Sonar way") on the
//! new code, and the new issues for the agent.
//!
//! SonarQube Server and SonarQube Cloud take the same calls; they differ
//! in URL, token and organization. The token goes to the scanner commands
//! and the Web API only, never into another step's environment.
//!
//! The patch's fault is a failed step: the scanner fails, or the quality
//! gate does. Anything else, a server that cannot be reached, refuses the
//! token, fails or does not finish its analysis, or has no quality gate,
//! is a [`GateError::Sonar`]: the agent could not fix it.

mod api;
mod scanner;

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use aegis_lang::Profile;

use crate::GateError;
use crate::command::{Exit, Ran, Runner};
use crate::report::{Skip, StepName, StepReport, StepStatus, tail};

use api::{Api, Condition, Issues, QualityGate, Waited};
pub use api::{Flow, FlowLocation, Issue as SonarIssue, TextRange};
use scanner::PullRequest;

/// Issues listed for the agent; the rest are counted.
const MAX_LISTED_ISSUES: usize = 20;

/// Where and how the gate has the patch analysed.
#[derive(Clone, PartialEq, Eq)]
pub struct SonarStep {
    /// `https://sonarcloud.io`, or the SonarQube Server's base URL.
    pub url: String,
    pub token: String,
    /// SonarQube Cloud's organization key; `None` on a Server.
    pub organization: Option<String>,
    pub project_key: String,
    /// Analyse the patch as a pull request into the base branch, so the
    /// quality gate judges the changed code (Cloud, Server Developer
    /// Edition and up). Off: a main-branch analysis judged on the
    /// project's new-code period (Community Build).
    pub pull_request: bool,
    /// The CLI scanner's program and first arguments.
    pub scanner: Vec<String>,
    /// Image of the scanner when the gate runs in containers; `None`
    /// takes the profile's (the .NET scanner is a tool of the SDK).
    pub image: Option<String>,
    /// Docker network of that container: its way to the server.
    pub network: String,
    /// Host directory the scanner caches its analyzers in.
    pub cache: Option<PathBuf>,
    /// Added after the profile's `[sonar.properties]`.
    pub properties: BTreeMap<String, String>,
    /// How long the server may take on the analysis once the scanner is
    /// done, and how often to ask.
    pub wait: Duration,
    pub poll: Duration,
}

impl fmt::Debug for SonarStep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SonarStep")
            .field("url", &self.url)
            .field("token", &"<redacted>")
            .field("organization", &self.organization)
            .field("project_key", &self.project_key)
            .field("pull_request", &self.pull_request)
            .field("scanner", &self.scanner)
            .field("image", &self.image)
            .field("network", &self.network)
            .field("cache", &self.cache)
            .field("properties", &self.properties)
            .field("wait", &self.wait)
            .field("poll", &self.poll)
            .finish()
    }
}

/// What the step needs of the gate run.
pub(crate) struct Context<'a> {
    pub repo: &'a Path,
    /// Branch the patch goes into, as the request names it.
    pub base: &'a str,
    /// Tree under check; names the pull request analysis.
    pub tree: &'a str,
    pub profile: &'a Profile,
    pub runner: &'a Runner,
    pub logs: &'a Path,
}

fn report(status: StepStatus, ran: &[Ran], detail: String) -> StepReport {
    StepReport {
        name: StepName::Sonar,
        status,
        duration: ran.iter().map(|r| r.duration).sum(),
        detail,
        logs: ran.iter().map(|r| r.log.clone()).collect(),
    }
}

/// `new_coverage = 42.1 (fails when LT 80)`.
fn condition(c: &Condition) -> String {
    let actual = c.actual_value.as_deref().unwrap_or("?");
    match (&c.comparator, &c.error_threshold) {
        (Some(op), Some(limit)) => format!("{} = {actual} (fails when {op} {limit})", c.metric_key),
        _ => format!("{} = {actual}", c.metric_key),
    }
}

/// One line per issue, the path without the project key.
fn issues_text(issues: &Issues) -> String {
    let mut lines: Vec<String> = issues
        .issues
        .iter()
        .take(MAX_LISTED_ISSUES)
        .map(|i| {
            let path = i
                .component
                .split_once(':')
                .map_or(&*i.component, |(_, p)| p);
            let place = i.line.map_or(path.to_owned(), |l| format!("{path}:{l}"));
            let severity = i
                .severity
                .as_deref()
                .map(|s| format!(" {s}"))
                .unwrap_or_default();
            format!("  {place} {}{severity}: {}", i.rule, i.message)
        })
        .collect();
    let listed = lines.len() as u64;
    if issues.total > listed {
        lines.push(format!("  … and {} more", issues.total - listed));
    }
    lines.join("\n")
}

fn verdict(gate: &QualityGate, issues: &Issues, dashboard: &str) -> (StepStatus, String) {
    let mut text = Vec::new();
    let status = match gate.status.as_str() {
        "OK" | "WARN" => {
            text.push(format!("quality gate {} on the new code", gate.status));
            StepStatus::Passed
        }
        _ => {
            text.push("the quality gate fails on the new code:".to_owned());
            text.extend(
                gate.conditions
                    .iter()
                    .filter(|c| c.status == "ERROR")
                    .map(|c| format!("  {}", condition(c))),
            );
            StepStatus::Failed
        }
    };
    if issues.total > 0 {
        text.push(format!("{} new issues:", issues.total));
        text.push(issues_text(issues));
    }
    text.push(format!("dashboard: {dashboard}"));
    (status, text.join("\n"))
}

/// Open issues of the step's project, at most `limit`, with the total the
/// server counts: the warnings a Sonar batch works through (#17).
pub fn open_issues(step: &SonarStep, limit: usize) -> Result<(Vec<SonarIssue>, u64), GateError> {
    let api = Api::new(&step.url, &step.token, step.organization.as_deref())?;
    let found = api.open_issues(&step.project_key, limit)?;
    Ok((found.issues, found.total))
}

pub(crate) fn run(step: &SonarStep, cx: &Context<'_>) -> Result<StepReport, GateError> {
    let pr = step.pull_request.then(|| PullRequest::of(cx.tree, cx.base));
    let _cleanup = scanner::prepare(cx.repo)?;
    let scan = scanner::scan(step, cx.profile, pr.as_ref(), cx.runner, cx.repo, cx.logs)?;
    if let Some(failed) = &scan.failed {
        return Ok(match failed.exit {
            Exit::Cancelled => StepReport::skipped(StepName::Sonar, Skip::Cancelled),
            e => report(
                StepStatus::Failed,
                &scan.ran,
                format!(
                    "`{}` failed ({}):\n{}",
                    failed.log.file_stem().unwrap_or_default().to_string_lossy(),
                    match e {
                        Exit::Code(c) => format!("exit code {c}"),
                        Exit::Signal => "killed by a signal".into(),
                        _ => "timed out".into(),
                    },
                    tail(&failed.log)
                ),
            ),
        });
    }
    let task_id = scanner::task_id(&scan.report_task)?;
    let api = Api::new(&step.url, &step.token, step.organization.as_deref())?;
    let task = match api.wait_for(&task_id, step.wait, step.poll, &cx.runner.cancel)? {
        Waited::Cancelled => return Ok(StepReport::skipped(StepName::Sonar, Skip::Cancelled)),
        Waited::Done(task) => task,
    };
    // The id came from a file in the tree: it must be this project's
    // analysis, of this pull request.
    if task.component_key.as_deref() != Some(&step.project_key)
        || task.pull_request.as_deref() != pr.as_ref().map(|p| p.key.as_str())
    {
        return Err(GateError::Sonar(format!(
            "task {task_id} is not the analysis of `{}` the gate asked for",
            step.project_key
        )));
    }
    if task.status != "SUCCESS" {
        return Err(GateError::Sonar(format!(
            "the server's analysis {} ({}): {}",
            task_id,
            task.status,
            task.error_message.as_deref().unwrap_or("no message")
        )));
    }
    let analysis = task
        .analysis_id
        .ok_or_else(|| GateError::Sonar(format!("task {task_id} has no analysis id")))?;
    let gate = api.quality_gate(&analysis)?;
    if gate.status == "NONE" {
        return Err(GateError::Sonar(format!(
            "project `{}` has no quality gate to judge the patch with",
            step.project_key
        )));
    }
    let issues = api.new_issues(&step.project_key, pr.as_ref().map(|p| p.key.as_str()))?;
    let url = step.url.trim_end_matches('/');
    let dashboard = match &pr {
        Some(p) => format!(
            "{url}/dashboard?id={}&pullRequest={}",
            step.project_key, p.key
        ),
        None => format!("{url}/dashboard?id={}", step.project_key),
    };
    let (status, detail) = verdict(&gate, &issues, &dashboard);
    Ok(report(status, &scan.ran, detail))
}

#[cfg(test)]
mod tests;
