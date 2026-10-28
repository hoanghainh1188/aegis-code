//! A failed GitHub Actions run (#250) or GitLab pipeline (#253), as a CI
//! job needs it: the workflow, the commit, the jobs and steps that failed,
//! and the end of each failed job's log. The logs are untrusted (design §7): the caller gives them to
//! the model as data.

use serde::Deserialize;

use crate::{GitHub, GitLab, PrError};

/// Failed jobs read, at most; the rest are named only.
const MAX_JOBS: usize = 3;
/// The end of each failed job's log kept, in characters.
const LOG_TAIL_CHARS: usize = 6_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailedJob {
    pub name: String,
    pub url: String,
    /// The steps that failed, in order.
    pub steps: Vec<String>,
    /// The end of its log; empty when it could not be read.
    pub log_tail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailedRun {
    pub id: u64,
    /// The workflow's name.
    pub workflow: String,
    pub url: String,
    pub head_branch: String,
    pub head_sha: String,
    /// The run's conclusion: `failure` for one worth a job.
    pub conclusion: String,
    pub failed: Vec<FailedJob>,
    /// Failed jobs beyond [`MAX_JOBS`], by name.
    pub more: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct Run {
    id: u64,
    #[serde(default)]
    name: Option<String>,
    html_url: String,
    #[serde(default)]
    head_branch: Option<String>,
    head_sha: String,
    #[serde(default)]
    conclusion: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Jobs {
    jobs: Vec<Job>,
}

#[derive(Debug, Deserialize)]
struct Job {
    id: u64,
    name: String,
    html_url: String,
    #[serde(default)]
    conclusion: Option<String>,
    #[serde(default)]
    steps: Vec<Step>,
}

#[derive(Debug, Deserialize)]
struct Step {
    name: String,
    #[serde(default)]
    conclusion: Option<String>,
}

/// The last `chars` characters of `text`, from a line start.
fn tail(text: &str, chars: usize) -> String {
    let count = text.chars().count();
    if count <= chars {
        return text.to_owned();
    }
    let cut: String = text.chars().skip(count - chars).collect();
    match cut.split_once('\n') {
        Some((_, rest)) => rest.to_owned(),
        None => cut,
    }
}

fn parse<T: serde::de::DeserializeOwned>(value: serde_json::Value) -> Result<T, PrError> {
    serde_json::from_value(value).map_err(|e| PrError::GitHub {
        status: 200,
        message: format!("unexpected reply: {e}"),
    })
}

impl GitHub {
    /// Run `run_id` of this repo, with its failed jobs (of its latest
    /// attempt) and the end of their logs. A log that cannot be read is
    /// left empty: the steps still say what failed.
    pub async fn failed_run(&self, run_id: u64) -> Result<FailedRun, PrError> {
        let run: Run = parse(self.get(&format!("actions/runs/{run_id}")).await?)?;
        let jobs: Jobs = parse(
            self.get(&format!(
                "actions/runs/{run_id}/jobs?filter=latest&per_page=100"
            ))
            .await?,
        )?;
        let failed: Vec<Job> = jobs
            .jobs
            .into_iter()
            .filter(|j| j.conclusion.as_deref() == Some("failure"))
            .collect();
        let more = failed
            .iter()
            .skip(MAX_JOBS)
            .map(|j| j.name.clone())
            .collect();
        let mut read = Vec::new();
        for job in failed.into_iter().take(MAX_JOBS) {
            let log = self
                .get_text(&format!("actions/jobs/{}/logs", job.id))
                .await
                .map(|text| tail(&text, LOG_TAIL_CHARS))
                .unwrap_or_default();
            read.push(FailedJob {
                steps: job
                    .steps
                    .into_iter()
                    .filter(|s| s.conclusion.as_deref() == Some("failure"))
                    .map(|s| s.name)
                    .collect(),
                name: job.name,
                url: job.html_url,
                log_tail: log,
            });
        }
        Ok(FailedRun {
            id: run.id,
            workflow: run.name.unwrap_or_else(|| "CI".into()),
            url: run.html_url,
            head_branch: run.head_branch.unwrap_or_default(),
            head_sha: run.head_sha,
            conclusion: run.conclusion.unwrap_or_default(),
            failed: read,
            more,
        })
    }
}

#[derive(Debug, Deserialize)]
struct Pipeline {
    id: u64,
    #[serde(default)]
    name: Option<String>,
    web_url: String,
    #[serde(rename = "ref", default)]
    branch: Option<String>,
    sha: String,
    status: String,
}

#[derive(Debug, Deserialize)]
struct PipelineJob {
    id: u64,
    name: String,
    #[serde(default)]
    stage: Option<String>,
    web_url: String,
}

impl GitLab {
    /// Pipeline `id` of this project, as a [`FailedRun`]: its failed jobs
    /// (GitLab names a stage, not steps) and the end of their logs. A
    /// pipeline GitLab calls `failed` has the conclusion `failure`.
    pub async fn failed_pipeline(&self, id: u64) -> Result<FailedRun, PrError> {
        let get = reqwest::Method::GET;
        let pipeline: Pipeline = self
            .call(get.clone(), &format!("pipelines/{id}"), None)
            .await?;
        let jobs: Vec<PipelineJob> = self
            .call(
                get.clone(),
                &format!("pipelines/{id}/jobs?scope[]=failed&per_page=100"),
                None,
            )
            .await?;
        let more = jobs.iter().skip(MAX_JOBS).map(|j| j.name.clone()).collect();
        let mut failed = Vec::new();
        for job in jobs.into_iter().take(MAX_JOBS) {
            let log = self
                .text(get.clone(), &format!("jobs/{}/trace", job.id), None)
                .await
                .map(|(_, text)| tail(&text, LOG_TAIL_CHARS))
                .unwrap_or_default();
            failed.push(FailedJob {
                steps: job.stage.into_iter().collect(),
                name: job.name,
                url: job.web_url,
                log_tail: log,
            });
        }
        Ok(FailedRun {
            id: pipeline.id,
            workflow: pipeline.name.unwrap_or_else(|| "pipeline".into()),
            url: pipeline.web_url,
            head_branch: pipeline.branch.unwrap_or_default(),
            head_sha: pipeline.sha,
            conclusion: if pipeline.status == "failed" {
                "failure".into()
            } else {
                pipeline.status
            },
            failed,
            more,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_log_keeps_its_end_from_a_line_start() {
        assert_eq!(tail("short", 10), "short");
        let log = "line one\nline two\nFAIL: TestX\n";
        assert_eq!(tail(log, 18), "FAIL: TestX\n");
        assert_eq!(tail("ééééé", 2), "éé");
    }
}
