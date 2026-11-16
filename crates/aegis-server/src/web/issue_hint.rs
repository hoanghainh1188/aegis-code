//! Help with the issue on the jobs page's form (#564). A maintainer picks
//! the repo; its issues are then asked for by number:
//!
//! - the source "repo" takes `12` or `#12` (or a whole `owner/repo#12`),
//!   completed from the repo's URL on its forge ([`crate::repo_issue`]);
//! - `GET /jobs/issue-choices?repo_id=` lists a GitHub repo's open issues
//!   for the field to offer (a `<datalist>`), those with a job marked;
//! - `GET /jobs/issue-preview?repo_id=&source=&issue_ref=` names the issue
//!   entered, or says it cannot be read, before the job is created.
//!
//! GitHub is read through the repo's own connection, as its clone is made
//! ([`crate::github_app::read_token`]): the fork's token, or a read token
//! of the GitHub App. Both fragments answer 200 whatever happens, so the
//! form never breaks on a forge that does not answer.

use std::collections::HashSet;

use aegis_lifecycle::{ForkPlatform, IssueInput, IssueSummary, open_github_issues, upstream_of};

use crate::repo_issue::{issues_of, resolve};
use aegis_store::{JobFilter, JobSource, Page, Repo, RepoId, RepoRole, Store};
use askama::Template;
use axum::Extension;
use axum::extract::{Query, State};
use serde::Deserialize;

use super::repo_admin::{RepoAdmin, RepoAdminExt};
use super::{WebError, WebResult, render};
use crate::auth::Access;
use crate::repo_cli::RepoFile;

/// How many open issues the field offers.
const OFFERED: u8 = 50;

/// How the server reads a GitHub repo: its API root, owner and name, and
/// the token of its connection (none for a public repo on the token path).
struct GitHubAccess {
    api: String,
    owner: String,
    name: String,
    token: Option<String>,
}

async fn github_access(admin: &RepoAdmin, repo: &Repo) -> Result<GitHubAccess, String> {
    let file = RepoFile::of(repo)?;
    let (owner, name) =
        upstream_of(&repo.url).ok_or_else(|| format!("`{}` is not a GitHub repo", repo.url))?;
    let api = match (&file.settings.app, &file.settings.fork) {
        (Some(app), _) => match (&app.api, admin.github_app.as_deref()) {
            (Some(api), _) => api.clone(),
            (None, Some(handle)) => handle.api().to_owned(),
            (None, None) => return Err("the instance has no GitHub App".into()),
        },
        (None, Some(fork)) if fork.platform == ForkPlatform::GitHub => fork.api.clone(),
        _ => "https://api.github.com".to_owned(),
    };
    let token = crate::github_app::read_token(
        &admin.secrets,
        admin.github_app.as_deref(),
        &file.settings,
        &repo.url,
        repo.tenant_id,
    )
    .await?;
    Ok(GitHubAccess {
        api,
        owner,
        name,
        token: token.filter(|t| !t.is_empty()),
    })
}

/// The repo `id` when the viewer may create its jobs; `None` otherwise, so
/// the fragments say nothing of a repo they may not see.
async fn creatable(store: &Store, access: &Access, id: i64) -> Result<Option<Repo>, WebError> {
    let id = RepoId(id);
    if !access.can(id, RepoRole::Maintainer) {
        return Ok(None);
    }
    Ok(store.repo(id).await?)
}

#[derive(Debug, Deserialize)]
pub struct ChoicesQuery {
    repo_id: i64,
}

pub struct Offered {
    pub value: String,
    pub label: String,
}

#[derive(Template)]
#[template(path = "jobs_issue_choices.html")]
struct ChoicesPart {
    offered: Vec<Offered>,
    note: Option<String>,
}

pub async fn choices(
    State(store): State<Store>,
    Extension(access): Extension<Access>,
    Extension(admin): RepoAdminExt,
    Query(q): Query<ChoicesQuery>,
) -> WebResult {
    let part = match creatable(&store, &access, q.repo_id).await? {
        None => ChoicesPart {
            offered: Vec::new(),
            note: None,
        },
        Some(repo) => offered(&store, admin.as_ref(), &repo).await?,
    };
    render(&part)
}

async fn offered(
    store: &Store,
    admin: Option<&RepoAdmin>,
    repo: &Repo,
) -> Result<ChoicesPart, WebError> {
    let quiet = |note: Option<String>| ChoicesPart {
        offered: Vec::new(),
        note,
    };
    match issues_of(repo) {
        Some(i) if i.source == JobSource::Github => {}
        Some(i) => {
            return Ok(quiet(Some(format!(
                "Enter the number of an issue of {}.",
                i.project
            ))));
        }
        None => return Ok(quiet(None)),
    }
    let Some(admin) = admin else {
        return Ok(quiet(None));
    };
    let listed = match github_access(admin, repo).await {
        Ok(gh) => open_github_issues(&gh.api, &gh.owner, &gh.name, gh.token.as_deref(), OFFERED)
            .await
            .map_err(|e| e.to_string()),
        Err(why) => Err(why),
    };
    let issues: Vec<IssueSummary> = match listed {
        Ok(issues) => issues,
        Err(why) => {
            tracing::warn!(repo = %repo.slug, error = %why, "listing the repo's issues");
            return Ok(quiet(Some(
                "Its open issues could not be listed; enter the number.".into(),
            )));
        }
    };
    let with_job = issue_refs_with_jobs(store, repo.id).await?;
    let project = issues_of(repo).map(|i| i.project).unwrap_or_default();
    Ok(ChoicesPart {
        offered: issues
            .into_iter()
            .map(|i| {
                let taken = with_job.contains(&format!("{project}#{}", i.number));
                Offered {
                    value: format!("#{}", i.number),
                    label: if taken {
                        format!("{} (has a job)", i.title)
                    } else {
                        i.title
                    },
                }
            })
            .collect(),
        note: None,
    })
}

/// The issue references of the repo's latest jobs.
async fn issue_refs_with_jobs(store: &Store, repo: RepoId) -> Result<HashSet<String>, WebError> {
    let filter = JobFilter {
        repo_id: Some(repo),
        status: None,
    };
    let jobs = store
        .list_jobs(&filter, Page::first(Page::MAX_LIMIT))
        .await?;
    Ok(jobs.items.into_iter().map(|j| j.issue_ref).collect())
}

#[derive(Debug, Deserialize)]
pub struct PreviewQuery {
    repo_id: i64,
    #[serde(default)]
    source: String,
    #[serde(default)]
    issue_ref: String,
}

#[derive(Template)]
#[template(path = "jobs_issue_preview.html")]
struct PreviewPart {
    /// The issue named, with its reference.
    found: Option<String>,
    /// Why it is not, or a hint.
    note: Option<String>,
    bad: bool,
}

pub async fn preview(
    State(store): State<Store>,
    Extension(access): Extension<Access>,
    Extension(admin): RepoAdminExt,
    Query(q): Query<PreviewQuery>,
) -> WebResult {
    let empty = PreviewPart {
        found: None,
        note: None,
        bad: false,
    };
    let Some(repo) = creatable(&store, &access, q.repo_id).await? else {
        return render(&empty);
    };
    if q.issue_ref.trim().is_empty() {
        return render(&empty);
    }
    let part = match resolve(&repo, &q.source, &q.issue_ref) {
        Err(why) => PreviewPart {
            found: None,
            note: Some(why),
            bad: true,
        },
        Ok((source, issue_ref)) => named(admin.as_ref(), &repo, &source, &issue_ref).await,
    };
    render(&part)
}

/// What the preview says of `issue_ref`: its title on GitHub, else the
/// reference the job will take.
async fn named(
    admin: Option<&RepoAdmin>,
    repo: &Repo,
    source: &str,
    issue_ref: &str,
) -> PreviewPart {
    let plain = || PreviewPart {
        found: None,
        note: Some(format!("The job will take {issue_ref}.")),
        bad: false,
    };
    if source != JobSource::Github.as_str() {
        return plain();
    }
    let Some(admin) = admin else {
        return plain();
    };
    let Some((owner, name, number)) = github_ref(issue_ref) else {
        return PreviewPart {
            found: None,
            note: Some(format!("`{issue_ref}` is not owner/repo#12")),
            bad: true,
        };
    };
    let gh = match github_access(admin, repo).await {
        Ok(gh) => gh,
        Err(why) => {
            tracing::warn!(repo = %repo.slug, error = %why, "previewing an issue");
            return plain();
        }
    };
    match IssueInput::from_github(&gh.api, &owner, &name, number, gh.token.as_deref()).await {
        Ok(issue) => PreviewPart {
            found: Some(format!("{issue_ref} · {}", issue.title)),
            note: None,
            bad: false,
        },
        Err(e) => PreviewPart {
            found: None,
            note: Some(format!("{issue_ref} could not be read: {e}")),
            bad: true,
        },
    }
}

/// `owner/repo#12` as its parts.
fn github_ref(issue_ref: &str) -> Option<(String, String, u64)> {
    let (repo, number) = issue_ref.split_once('#')?;
    let (owner, name) = repo.split_once('/')?;
    let number = number.parse().ok().filter(|n| *n > 0)?;
    (!owner.is_empty() && !name.is_empty() && !name.contains('/'))
        .then(|| (owner.to_owned(), name.to_owned(), number))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_references_are_split_strictly() {
        assert_eq!(
            github_ref("acme/shop#12"),
            Some(("acme".into(), "shop".into(), 12))
        );
        for bad in ["acme/shop", "acme#12", "a/b/c#1", "acme/shop#0", "/shop#1"] {
            assert_eq!(github_ref(bad), None, "{bad}");
        }
    }
}
