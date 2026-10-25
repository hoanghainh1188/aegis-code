//! A repo's tracker, in its settings (`repos.config.tracker`).

use serde::{Deserialize, Serialize};

use crate::TrackerError;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum TrackerConfig {
    Jira(JiraConfig),
    Backlog(BacklogConfig),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Deployment {
    #[default]
    Cloud,
    /// Data Center or Server.
    DataCenter,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JiraConfig {
    /// The site: `https://acme.atlassian.net`, or a Data Center's root
    /// with its context path, `https://jira.acme.com/jira`.
    pub url: String,
    #[serde(default)]
    pub deployment: Deployment,
    /// Cloud: the account's e-mail, with its API token (basic auth).
    /// Without it the token is sent as a bearer token: a Data Center
    /// personal access token.
    #[serde(default)]
    pub user: Option<String>,
    /// The secret holding the token.
    pub token_env: String,
    /// Which tickets get a job, e.g. `project = APP AND labels = aegis
    /// AND statusCategory = "To Do"`.
    pub jql: String,
    /// The transition (by name, any case) to make once a PR is open.
    #[serde(default)]
    pub pr_transition: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BacklogConfig {
    /// The space: `https://acme.backlog.com` or `https://acme.backlog.jp`.
    pub url: String,
    /// The secret holding the API key.
    pub token_env: String,
    /// The filter: tickets of these projects…
    pub project_ids: Vec<u64>,
    /// …with one of these statuses, types, categories (any when empty)…
    #[serde(default)]
    pub status_ids: Vec<u64>,
    #[serde(default)]
    pub issue_type_ids: Vec<u64>,
    #[serde(default)]
    pub category_ids: Vec<u64>,
    /// …and holding this word.
    #[serde(default)]
    pub keyword: Option<String>,
    /// The status to set once a PR is open.
    #[serde(default)]
    pub pr_status_id: Option<u64>,
}

impl TrackerConfig {
    pub fn token_env(&self) -> &str {
        match self {
            Self::Jira(c) => &c.token_env,
            Self::Backlog(c) => &c.token_env,
        }
    }

    pub fn check(&self) -> Result<(), TrackerError> {
        let bad = |m: String| Err(TrackerError::Config(m));
        match self {
            Self::Jira(c) => {
                site_url(&c.url)?;
                token_env(&c.token_env)?;
                if c.jql.trim().is_empty() {
                    return bad("jira: `jql` is empty".into());
                }
                if c.deployment == Deployment::Cloud
                    && c.user.as_deref().is_none_or(|u| u.trim().is_empty())
                {
                    return bad(
                        "jira: Cloud needs `user`, the e-mail of the API token's account".into(),
                    );
                }
            }
            Self::Backlog(c) => {
                site_url(&c.url)?;
                token_env(&c.token_env)?;
                if c.project_ids.is_empty() {
                    return bad("backlog: `project_ids` is empty".into());
                }
            }
        }
        Ok(())
    }
}

/// `https://…` (or `http://` to this machine, for tests), without a query
/// or fragment: the token goes there. Returned without a trailing `/`.
pub(crate) fn site_url(url: &str) -> Result<&str, TrackerError> {
    let url = url.trim_end_matches('/');
    let local = ["http://127.0.0.1", "http://localhost", "http://[::1]"]
        .iter()
        .any(|p| url.starts_with(p));
    if !url.starts_with("https://") && !local {
        return Err(TrackerError::Config(format!("`{url}` must use https")));
    }
    if url.contains(['?', '#']) || url.contains(char::is_whitespace) {
        return Err(TrackerError::Config(format!(
            "`{url}`: a site URL, without a query or fragment"
        )));
    }
    Ok(url)
}

/// The name of a secret, as the secret store takes it.
fn token_env(name: &str) -> Result<(), TrackerError> {
    let ok = !name.is_empty()
        && name.len() <= 128
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b'/'));
    if ok {
        Ok(())
    } else {
        Err(TrackerError::Config(format!(
            "`token_env` `{name}` is not a secret's name"
        )))
    }
}
