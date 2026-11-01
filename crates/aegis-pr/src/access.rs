//! What a token may do in a repo, and the repo as the forge names it now
//! (#331): the admin's connection check of a registered repo.

use serde::Deserialize;

use crate::review::GITLAB_DEVELOPER;
use crate::{GitHub, GitLab, PrError};

/// A GitLab Reporter may read the project and comment on its issues.
const GITLAB_REPORTER: u32 = 20;

/// A repo as a token sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoAccess {
    /// `owner/name` (GitHub) or `group/sub/project` (GitLab), as the forge
    /// names it now: another than the one asked for when it was renamed or
    /// moved.
    pub path: String,
    /// Its web page.
    pub web_url: String,
    /// The token may push branches.
    pub push: bool,
    /// The token may comment on its issues.
    pub comment: bool,
    pub issues_enabled: bool,
    pub archived: bool,
}

#[derive(Deserialize)]
struct GitHubRepo {
    full_name: String,
    html_url: String,
    #[serde(default)]
    permissions: GitHubPermissions,
    #[serde(default)]
    has_issues: bool,
    #[serde(default)]
    archived: bool,
}

#[derive(Default, Deserialize)]
struct GitHubPermissions {
    #[serde(default)]
    push: bool,
    #[serde(default)]
    pull: bool,
}

impl GitHub {
    /// This repo as the token sees it: `GET /repos/{owner}/{repo}`, its
    /// redirect followed when the repo was renamed or moved.
    pub async fn access(&self) -> Result<RepoAccess, PrError> {
        let repo: GitHubRepo = self.get_root().await?;
        Ok(RepoAccess {
            path: repo.full_name,
            web_url: repo.html_url,
            push: repo.permissions.push,
            // Anyone who may read a repo may comment on its open issues.
            comment: repo.permissions.pull,
            issues_enabled: repo.has_issues,
            archived: repo.archived,
        })
    }
}

#[derive(Deserialize)]
struct GitLabProject {
    path_with_namespace: String,
    web_url: String,
    #[serde(default)]
    permissions: Option<GitLabPermissions>,
    #[serde(default)]
    issues_enabled: bool,
    #[serde(default)]
    archived: bool,
}

#[derive(Deserialize)]
struct GitLabPermissions {
    project_access: Option<GitLabLevel>,
    group_access: Option<GitLabLevel>,
}

#[derive(Deserialize)]
struct GitLabLevel {
    access_level: u32,
}

impl GitLab {
    /// This project as the token sees it: `GET /projects/:id`.
    pub async fn access(&self) -> Result<RepoAccess, PrError> {
        let project: GitLabProject = self.get_root().await?;
        let level = project
            .permissions
            .as_ref()
            .map(|p| {
                let of = |l: &Option<GitLabLevel>| l.as_ref().map_or(0, |l| l.access_level);
                of(&p.project_access).max(of(&p.group_access))
            })
            .unwrap_or(0);
        Ok(RepoAccess {
            path: project.path_with_namespace,
            web_url: project.web_url,
            push: level >= GITLAB_DEVELOPER,
            comment: level >= GITLAB_REPORTER,
            issues_enabled: project.issues_enabled,
            archived: project.archived,
        })
    }
}
