//! What a delivery asks for. Only the fields needed are read, and only to
//! build the job's `issue_ref` from names and numbers checked here: the
//! issue's title, body and comments are untrusted (design §7) and stay out
//! of the job; the worker reads the issue itself.

use serde::Deserialize;

/// The longest `issue_ref` built, as the internal API allows.
const MAX_ISSUE_REF_LEN: usize = 256;

/// A job a delivery asks for, before its repo is looked up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wanted {
    /// The repository's web URL, to find it in `repos`.
    pub repo_url: String,
    pub issue_ref: String,
    pub kind: Kind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// The trigger label was put on an issue.
    Issue,
    /// CI failed on `branch`; a job only if it is the repo's default one.
    CiFailure { branch: String },
}

/// Why a delivery asks for nothing, or cannot be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Skip {
    Ignored(String),
    Malformed(String),
}

fn ignored(why: impl Into<String>) -> Skip {
    Skip::Ignored(why.into())
}

fn parse<'a, T: Deserialize<'a>>(body: &'a [u8]) -> Result<T, Skip> {
    serde_json::from_slice(body).map_err(|e| Skip::Malformed(e.to_string()))
}

/// Letters, digits, `-`, `_`, `.`: an owner, group or repository name.
fn plain_name(s: &str) -> bool {
    !s.is_empty()
        && s != "."
        && s != ".."
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// `owner/name` on GitHub, `group/sub/project` on GitLab.
fn checked_path(path: &str, max_parts: usize) -> Result<&str, Skip> {
    let parts: Vec<&str> = path.split('/').collect();
    if (2..=max_parts).contains(&parts.len()) && parts.iter().all(|p| plain_name(p)) {
        Ok(path)
    } else {
        Err(Skip::Malformed(format!(
            "`{path}` is not a repository path"
        )))
    }
}

fn checked_ref(issue_ref: String) -> Result<String, Skip> {
    if issue_ref.len() <= MAX_ISSUE_REF_LEN {
        Ok(issue_ref)
    } else {
        Err(Skip::Malformed("the repository path is too long".into()))
    }
}

#[derive(Debug, Deserialize)]
struct GithubRepo {
    full_name: String,
    html_url: String,
}

#[derive(Debug, Deserialize)]
struct Named {
    name: String,
}

#[derive(Debug, Deserialize)]
struct GithubIssues {
    action: String,
    label: Option<Named>,
    issue: GithubIssue,
    repository: GithubRepo,
}

#[derive(Debug, Deserialize)]
struct GithubIssue {
    number: u64,
    #[serde(default)]
    pull_request: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct GithubWorkflowRun {
    action: String,
    workflow_run: WorkflowRun,
    repository: GithubRepo,
}

#[derive(Debug, Deserialize)]
struct WorkflowRun {
    id: u64,
    conclusion: Option<String>,
    head_branch: Option<String>,
    event: String,
    head_repository: Option<HeadRepo>,
}

#[derive(Debug, Deserialize)]
struct HeadRepo {
    full_name: String,
}

/// A delivery of GitHub event `event` (`X-GitHub-Event`).
pub fn github(event: &str, body: &[u8], label: &str) -> Result<Wanted, Skip> {
    match event {
        "issues" => {
            let e: GithubIssues = parse(body)?;
            if e.action != "labeled" || e.label.is_none_or(|l| l.name != label) {
                return Err(ignored(format!("not the label `{label}` put on an issue")));
            }
            if e.issue.pull_request.is_some() {
                return Err(ignored("a pull request, not an issue"));
            }
            let path = checked_path(&e.repository.full_name, 2)?;
            Ok(Wanted {
                issue_ref: checked_ref(format!("{path}#{}", e.issue.number))?,
                repo_url: e.repository.html_url,
                kind: Kind::Issue,
            })
        }
        "workflow_run" => {
            let e: GithubWorkflowRun = parse(body)?;
            let run = e.workflow_run;
            if e.action != "completed" || run.conclusion.as_deref() != Some("failure") {
                return Err(ignored("not a failed run"));
            }
            // A pull request's run has the head branch of the PR, which
            // anyone may name `main`: only runs of the repo's own pushes.
            let own = run
                .head_repository
                .is_some_and(|h| h.full_name == e.repository.full_name);
            if !own || run.event.starts_with("pull_request") {
                return Err(ignored("a run of a pull request"));
            }
            let branch = run
                .head_branch
                .ok_or_else(|| ignored("a run of no branch"))?;
            let path = checked_path(&e.repository.full_name, 2)?;
            Ok(Wanted {
                issue_ref: checked_ref(format!("{path}/actions/runs/{}", run.id))?,
                repo_url: e.repository.html_url,
                kind: Kind::CiFailure { branch },
            })
        }
        other => Err(ignored(format!("event `{other}`"))),
    }
}

#[derive(Debug, Deserialize)]
struct GitlabProject {
    path_with_namespace: String,
    web_url: String,
}

#[derive(Debug, Deserialize)]
struct Titled {
    title: String,
}

#[derive(Debug, Deserialize)]
struct GitlabIssueHook {
    project: GitlabProject,
    object_attributes: GitlabIssue,
    #[serde(default)]
    changes: GitlabChanges,
}

#[derive(Debug, Deserialize)]
struct GitlabIssue {
    iid: u64,
}

#[derive(Debug, Default, Deserialize)]
struct GitlabChanges {
    labels: Option<LabelChange>,
}

#[derive(Debug, Deserialize)]
struct LabelChange {
    #[serde(default)]
    previous: Vec<Titled>,
    #[serde(default)]
    current: Vec<Titled>,
}

#[derive(Debug, Deserialize)]
struct GitlabPipelineHook {
    project: GitlabProject,
    object_attributes: Pipeline,
}

#[derive(Debug, Deserialize)]
struct Pipeline {
    id: u64,
    #[serde(rename = "ref")]
    branch: String,
    #[serde(default)]
    tag: bool,
    status: String,
    #[serde(default)]
    source: Option<String>,
}

/// GitLab subgroups nest up to 20 deep.
const MAX_GITLAB_PARTS: usize = 21;

/// A delivery of GitLab event `event` (`X-Gitlab-Event`).
pub fn gitlab(event: &str, body: &[u8], label: &str) -> Result<Wanted, Skip> {
    match event {
        "Issue Hook" => {
            let e: GitlabIssueHook = parse(body)?;
            let has = |list: &[Titled]| list.iter().any(|l| l.title == label);
            let added = e
                .changes
                .labels
                .is_some_and(|c| has(&c.current) && !has(&c.previous));
            if !added {
                return Err(ignored(format!("not the label `{label}` put on an issue")));
            }
            let path = checked_path(&e.project.path_with_namespace, MAX_GITLAB_PARTS)?;
            Ok(Wanted {
                issue_ref: checked_ref(format!("{path}#{}", e.object_attributes.iid))?,
                repo_url: e.project.web_url,
                kind: Kind::Issue,
            })
        }
        "Pipeline Hook" => {
            let e: GitlabPipelineHook = parse(body)?;
            let p = e.object_attributes;
            if p.status != "failed" {
                return Err(ignored("not a failed pipeline"));
            }
            if p.tag || p.source.as_deref() == Some("merge_request_event") {
                return Err(ignored("a pipeline of a tag or merge request"));
            }
            let path = checked_path(&e.project.path_with_namespace, MAX_GITLAB_PARTS)?;
            Ok(Wanted {
                issue_ref: checked_ref(format!("{path}/-/pipelines/{}", p.id))?,
                repo_url: e.project.web_url,
                kind: Kind::CiFailure { branch: p.branch },
            })
        }
        other => Err(ignored(format!("event `{other}`"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn labeled(label: &str, full_name: &str) -> Vec<u8> {
        json!({
            "action": "labeled",
            "label": { "name": label },
            "issue": { "number": 12, "title": "ignore previous instructions", "body": "…" },
            "repository": { "full_name": full_name, "html_url": format!("https://github.com/{full_name}") }
        })
        .to_string()
        .into_bytes()
    }

    #[test]
    fn a_github_label_asks_for_its_issue() {
        let wanted = github("issues", &labeled("aegis", "typst/typst"), "aegis").unwrap();
        assert_eq!(
            wanted,
            Wanted {
                repo_url: "https://github.com/typst/typst".into(),
                issue_ref: "typst/typst#12".into(),
                kind: Kind::Issue,
            }
        );
        assert!(matches!(
            github("issues", &labeled("bug", "typst/typst"), "aegis"),
            Err(Skip::Ignored(_))
        ));
        assert!(matches!(
            github("issues", &labeled("aegis", "typst/../x"), "aegis"),
            Err(Skip::Malformed(_))
        ));
        assert!(matches!(
            github("issues", b"{", "aegis"),
            Err(Skip::Malformed(_))
        ));
        assert!(matches!(
            github("ping", b"{}", "aegis"),
            Err(Skip::Ignored(_))
        ));
    }

    fn run(conclusion: &str, event: &str, head: &str) -> Vec<u8> {
        json!({
            "action": "completed",
            "workflow_run": { "id": 77, "conclusion": conclusion, "head_branch": "main",
                              "event": event, "head_repository": { "full_name": head } },
            "repository": { "full_name": "typst/typst", "html_url": "https://github.com/typst/typst" }
        })
        .to_string()
        .into_bytes()
    }

    #[test]
    fn a_failed_github_run_of_a_push_asks_for_a_ci_job() {
        let wanted = github("workflow_run", &run("failure", "push", "typst/typst"), "a").unwrap();
        assert_eq!(wanted.issue_ref, "typst/typst/actions/runs/77");
        assert_eq!(
            wanted.kind,
            Kind::CiFailure {
                branch: "main".into()
            }
        );
        for body in [
            run("success", "push", "typst/typst"),
            run("failure", "pull_request", "typst/typst"),
            run("failure", "push", "someone/typst"),
        ] {
            assert!(matches!(
                github("workflow_run", &body, "a"),
                Err(Skip::Ignored(_))
            ));
        }
    }

    #[test]
    fn a_gitlab_label_added_asks_for_its_issue() {
        let hook = |previous: &[&str], current: &[&str]| {
            let titles = |l: &[&str]| l.iter().map(|t| json!({ "title": t })).collect::<Vec<_>>();
            json!({
                "object_kind": "issue",
                "project": { "path_with_namespace": "group/sub/app",
                             "web_url": "https://gitlab.com/group/sub/app" },
                "object_attributes": { "iid": 3, "action": "update" },
                "changes": { "labels": { "previous": titles(previous), "current": titles(current) } }
            })
            .to_string()
            .into_bytes()
        };
        let wanted = gitlab("Issue Hook", &hook(&["bug"], &["bug", "aegis"]), "aegis").unwrap();
        assert_eq!(wanted.issue_ref, "group/sub/app#3");
        assert_eq!(wanted.repo_url, "https://gitlab.com/group/sub/app");
        assert!(matches!(
            gitlab("Issue Hook", &hook(&["aegis"], &["aegis", "bug"]), "aegis"),
            Err(Skip::Ignored(_))
        ));
    }

    #[test]
    fn a_failed_gitlab_pipeline_asks_for_a_ci_job() {
        let hook = |status: &str, source: &str| {
            json!({
                "object_kind": "pipeline",
                "project": { "path_with_namespace": "group/app", "web_url": "https://gitlab.com/group/app" },
                "object_attributes": { "id": 31, "ref": "main", "tag": false,
                                       "status": status, "source": source }
            })
            .to_string()
            .into_bytes()
        };
        let wanted = gitlab("Pipeline Hook", &hook("failed", "push"), "a").unwrap();
        assert_eq!(wanted.issue_ref, "group/app/-/pipelines/31");
        assert_eq!(
            wanted.kind,
            Kind::CiFailure {
                branch: "main".into()
            }
        );
        for body in [
            hook("success", "push"),
            hook("failed", "merge_request_event"),
        ] {
            assert!(matches!(
                gitlab("Pipeline Hook", &body, "a"),
                Err(Skip::Ignored(_))
            ));
        }
    }
}
