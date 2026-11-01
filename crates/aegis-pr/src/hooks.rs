//! A repo's webhook to the server, made from the admin's connection page
//! (#376) with a token the admin gives for that once: the one already
//! pointing at the payload URL is updated, or one is created, with the
//! events the server reads and its secret. Needs more than the agent's
//! tokens: `admin:repo_hook` (or admin of the repo) on GitHub, Maintainer
//! on GitLab.

use serde::Deserialize;
use serde_json::json;

use crate::{GitHub, GitLab, PrError};

/// What was done on the forge, with the webhook's id there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookDone {
    Created(u64),
    Updated(u64),
}

/// The same URL, give or take a trailing `/` and the case of its scheme
/// and host; its path is compared as written.
fn same_url(a: &str, b: &str) -> bool {
    let split = |u: &str| {
        let u = u.trim_end_matches('/');
        let (scheme, rest) = u.split_once("://").unwrap_or(("", u));
        let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
        (
            scheme.to_ascii_lowercase(),
            host.to_ascii_lowercase(),
            path.to_owned(),
        )
    };
    split(a) == split(b)
}

#[derive(Deserialize)]
struct GitHubHook {
    id: u64,
    #[serde(default)]
    config: GitHubHookConfig,
}

#[derive(Default, Deserialize)]
struct GitHubHookConfig {
    #[serde(default)]
    url: String,
}

#[derive(Deserialize)]
struct Made {
    id: u64,
}

impl GitHub {
    /// A webhook of this repo to `url` for `events` (`issues`,
    /// `workflow_run`, …), signed with `secret`.
    pub async fn ensure_hook(
        &self,
        url: &str,
        secret: &str,
        events: &[&str],
    ) -> Result<HookDone, PrError> {
        let hooks: Vec<GitHubHook> = self
            .call(reqwest::Method::GET, "hooks?per_page=100", None)
            .await?;
        let body = json!({
            "name": "web",
            "active": true,
            "events": events,
            "config": { "url": url, "content_type": "json", "secret": secret, "insecure_ssl": "0" },
        });
        match hooks.iter().find(|h| same_url(&h.config.url, url)) {
            Some(hook) => {
                let path = format!("hooks/{}", hook.id);
                let made: Made = self
                    .call(reqwest::Method::PATCH, &path, Some(&body))
                    .await?;
                Ok(HookDone::Updated(made.id))
            }
            None => {
                let made: Made = self
                    .call(reqwest::Method::POST, "hooks", Some(&body))
                    .await?;
                Ok(HookDone::Created(made.id))
            }
        }
    }
}

#[derive(Deserialize)]
struct GitLabHook {
    id: u64,
    #[serde(default)]
    url: String,
}

/// The kinds of event a GitLab project hook may send, each turned on or
/// off: all are named so an update turns off what is not asked for.
const GITLAB_EVENTS: [&str; 5] = [
    "issues_events",
    "pipeline_events",
    "note_events",
    "push_events",
    "merge_requests_events",
];

impl GitLab {
    /// A webhook of this project to `url` for `events` (`issues_events`,
    /// `pipeline_events`, `note_events`, …), with `secret` as its token.
    pub async fn ensure_hook(
        &self,
        url: &str,
        secret: &str,
        events: &[&str],
    ) -> Result<HookDone, PrError> {
        let hooks: Vec<GitLabHook> = self
            .call(reqwest::Method::GET, "hooks?per_page=100", None)
            .await?;
        let mut body = json!({ "url": url, "token": secret, "enable_ssl_verification": true });
        for kind in GITLAB_EVENTS {
            body[kind] = events.contains(&kind).into();
        }
        match hooks.iter().find(|h| same_url(&h.url, url)) {
            Some(hook) => {
                let path = format!("hooks/{}", hook.id);
                let made: Made = self.call(reqwest::Method::PUT, &path, Some(&body)).await?;
                Ok(HookDone::Updated(made.id))
            }
            None => {
                let made: Made = self
                    .call(reqwest::Method::POST, "hooks", Some(&body))
                    .await?;
                Ok(HookDone::Created(made.id))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hook_is_found_by_its_url_give_or_take_a_slash() {
        assert!(same_url(
            "https://A.example/webhooks/github/",
            "https://a.example/webhooks/github"
        ));
        assert!(!same_url(
            "https://a.example/webhooks/gitlab",
            "https://a.example/webhooks/github"
        ));
        assert!(!same_url(
            "https://a.example/Webhooks/GitHub",
            "https://a.example/webhooks/github"
        ));
    }
}
