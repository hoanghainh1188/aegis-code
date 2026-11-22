//! A job's issue given by its number on the repo's own forge (#564, #570):
//! the source `repo` takes `12`, `#12` or a whole `owner/repo#12`, and the
//! reference is completed from the repo's URL, on GitHub or GitLab. The
//! jobs page's form and `POST /api/v1/jobs` (so `aegis submit`) share it.

use aegis_lifecycle::{ForkPlatform, upstream_of};
use aegis_store::{JobSource, Repo};

use crate::repo_cli::RepoFile;

/// The form's source that means "an issue of the chosen repo".
pub(crate) const REPO_SOURCE: &str = "repo";
/// Where a repo's issues are: the forge's source and the project's path
/// (`owner/repo` on GitHub, `group/sub/project` on GitLab).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RepoIssues {
    pub(crate) source: JobSource,
    pub(crate) project: String,
}

/// The forge of `repo`'s issues, from its URL and its fork's platform;
/// `None` for a URL that is neither GitHub's nor GitLab's shape.
pub(crate) fn issues_of(repo: &Repo) -> Option<RepoIssues> {
    let on_gitlab = RepoFile::of(repo)
        .ok()
        .and_then(|f| f.settings.fork)
        .is_some_and(|f| f.platform == ForkPlatform::GitLab)
        || host_of(&repo.url).is_some_and(|h| h.contains("gitlab"));
    if on_gitlab {
        let rest = repo.url.strip_prefix("https://")?;
        let (_, path) = rest.split_once('/')?;
        let path = path.trim_end_matches('/');
        let path = path.strip_suffix(".git").unwrap_or(path);
        return (!path.is_empty()).then(|| RepoIssues {
            source: JobSource::Gitlab,
            project: path.to_owned(),
        });
    }
    let (owner, name) = upstream_of(&repo.url)?;
    Some(RepoIssues {
        source: JobSource::Github,
        project: format!("{owner}/{name}"),
    })
}

fn host_of(url: &str) -> Option<&str> {
    url.strip_prefix("https://")?.split('/').next()
}

/// The source and issue reference a job is created with. With the source
/// [`REPO_SOURCE`], `12`, `#12` or a whole reference, on the repo's forge;
/// any other source as it was given.
pub(crate) fn resolve(repo: &Repo, source: &str, input: &str) -> Result<(String, String), String> {
    let input = input.trim();
    if source != REPO_SOURCE {
        return Ok((source.to_owned(), input.to_owned()));
    }
    let issues = issues_of(repo).ok_or(
        "this repo's issues are not on GitHub or GitLab: give the issue's tracker (the form's From, the CLI's --source)",
    )?;
    let forge = issues.source.as_str().to_owned();
    let number = input.strip_prefix('#').unwrap_or(input);
    if !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()) {
        let n: u64 = number
            .parse()
            .map_err(|_| "the issue's number is too long")?;
        if n == 0 {
            return Err("issue numbers start at 1".into());
        }
        return Ok((forge, format!("{}#{n}", issues.project)));
    }
    // A whole reference: a project path, `#`, a number from 1.
    let whole = input.split_once('#').is_some_and(|(project, n)| {
        project.contains('/')
            && !project.starts_with('/')
            && !project.ends_with('/')
            && n.parse::<u64>().is_ok_and(|n| n > 0)
    });
    if whole {
        return Ok((forge, input.to_owned()));
    }
    Err(format!(
        "enter the issue's number, as 12 or #12 (an issue of {})",
        issues.project
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use aegis_store::RepoId;

    fn repo(url: &str, config: serde_json::Value) -> Repo {
        Repo {
            id: RepoId(1),
            slug: "shop".into(),
            url: url.into(),
            default_branch: "main".into(),
            language: "python".into(),
            mode: aegis_store::ModeName::POC,
            max_concurrent_jobs: 1,
            config,
            created_at: time::OffsetDateTime::now_utc(),
            tenant_id: aegis_store::TenantId::DEFAULT,
        }
    }

    #[test]
    fn a_number_is_completed_from_the_repo_on_its_forge() {
        let github = repo(
            "https://github.com/acme/shop",
            serde_json::json!({"app": {}}),
        );
        for input in ["12", "#12", " #12 "] {
            assert_eq!(
                resolve(&github, REPO_SOURCE, input),
                Ok(("github".into(), "acme/shop#12".into())),
                "{input}"
            );
        }
        let gitlab = repo(
            "https://gitlab.example.com/group/sub/shop.git",
            serde_json::json!({}),
        );
        assert_eq!(
            resolve(&gitlab, REPO_SOURCE, "#4"),
            Ok(("gitlab".into(), "group/sub/shop#4".into()))
        );
        // A whole reference is kept, on the repo's forge.
        assert_eq!(
            resolve(&github, REPO_SOURCE, "acme/issues#7"),
            Ok(("github".into(), "acme/issues#7".into()))
        );
        // Another source is taken as it was given.
        assert_eq!(
            resolve(&github, "jira", " SHOP-3 "),
            Ok(("jira".into(), "SHOP-3".into()))
        );
    }

    #[test]
    fn what_is_not_a_number_is_said_plainly() {
        let github = repo(
            "https://github.com/acme/shop",
            serde_json::json!({"app": {}}),
        );
        for input in ["", "#", "twelve", "#0"] {
            let err = resolve(&github, REPO_SOURCE, input).unwrap_err();
            assert!(
                err.contains("12") || err.contains("start at 1"),
                "{input}: {err}"
            );
        }
        let elsewhere = repo("https://git.example.com/shop", serde_json::json!({}));
        assert!(
            resolve(&elsewhere, REPO_SOURCE, "12")
                .unwrap_err()
                .contains("give the issue's tracker")
        );
    }
}
