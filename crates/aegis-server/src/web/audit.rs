//! The audit log for the admin (#368): who did what, when, filtered by
//! repo, by who and by action, a page at a time; the filters are in the
//! URL so a view can be shared. Only an admin: for anyone else it is not
//! found.

use std::collections::HashMap;

use aegis_store::{ActorKind, AuditEntry, AuditFilter, Page, RepoId, Store};
use askama::Template;
use axum::Extension;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use serde::Deserialize;

use super::admin::admin_only;
use super::dashboard::all_repos;
use super::{Me, Nonce, WebError, WebResult, render};
use crate::auth::Access;

const PAGE_SIZE: u32 = 50;
/// What an entry's detail shows, in characters; the rest is cut.
const MAX_DETAIL_CHARS: usize = 300;
/// The longest name or action a filter takes.
const MAX_FILTER_LEN: usize = 320;

#[derive(Debug, Default, Deserialize)]
pub struct AuditQuery {
    #[serde(default)]
    repo_id: String,
    #[serde(default)]
    actor: String,
    #[serde(default)]
    action: String,
    #[serde(default)]
    cursor: String,
}

struct EntryRow {
    at: String,
    who: String,
    /// The actor's kind when it is not a person in a session.
    via: Option<&'static str>,
    action: String,
    repo: Option<(i64, String)>,
    job: Option<i64>,
    detail: String,
}

struct Choice {
    value: String,
    label: String,
    selected: bool,
}

#[derive(Template)]
#[template(path = "admin_audit.html")]
struct AuditPage<'a> {
    nonce: &'a str,
    me: &'a Me,
    repos: Vec<Choice>,
    actions: Vec<Choice>,
    actor: &'a str,
    rows: Vec<EntryRow>,
    next: Option<String>,
    filtered: bool,
}

fn bad(what: &str) -> WebError {
    WebError::new(
        StatusCode::BAD_REQUEST,
        format!("invalid filter value: {what}"),
    )
}

/// `text` trimmed, `None` when empty; refused when over the limit.
fn text_filter(text: &str, what: &str) -> Result<Option<String>, WebError> {
    let text = text.trim();
    if text.len() > MAX_FILTER_LEN {
        return Err(bad(what));
    }
    Ok((!text.is_empty()).then(|| text.to_owned()))
}

/// `entry.detail` on one line, cut to [`MAX_DETAIL_CHARS`].
fn detail_of(entry: &AuditEntry) -> String {
    if entry.detail.as_object().is_some_and(|o| o.is_empty()) {
        return String::new();
    }
    let text = entry.detail.to_string();
    if text.chars().count() > MAX_DETAIL_CHARS {
        let cut: String = text.chars().take(MAX_DETAIL_CHARS).collect();
        format!("{cut}…")
    } else {
        text
    }
}

fn via(kind: ActorKind) -> Option<&'static str> {
    match kind {
        ActorKind::User => None,
        ActorKind::Token => Some("API token"),
        ActorKind::Cli => Some("cli"),
        ActorKind::Webhook => Some("webhook"),
        ActorKind::System => Some("system"),
    }
}

/// Percent-encodes `value` for a query string.
fn encoded(value: &str) -> String {
    value
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

pub async fn list(
    State(store): State<Store>,
    Extension(nonce): Extension<Nonce>,
    Extension(access): Extension<Access>,
    Extension(me): Extension<Me>,
    Query(q): Query<AuditQuery>,
) -> WebResult {
    admin_only(&access)?;
    let repo = match q.repo_id.trim() {
        "" => None,
        id => Some(RepoId(id.parse().map_err(|_| bad("repo"))?)),
    };
    let cursor = match q.cursor.trim() {
        "" => None,
        c => Some(c.parse::<i64>().map_err(|_| bad("cursor"))?),
    };
    let filter = AuditFilter {
        repo,
        actor: text_filter(&q.actor, "who")?,
        action: text_filter(&q.action, "action")?,
    };
    let page = cursor.map_or(Page::first(PAGE_SIZE), |c| Page::after(c, PAGE_SIZE));
    let entries = store.audit_log_where(&filter, page).await?;
    let repos = all_repos(&store).await?;
    let slugs: HashMap<RepoId, String> =
        repos.iter().map(|(id, r)| (*id, r.slug.clone())).collect();

    let next = entries.next_page().and_then(Page::cursor).map(|c| {
        format!(
            "/admin/audit?repo_id={}&actor={}&action={}&cursor={c}",
            filter.repo.map_or(String::new(), |r| r.0.to_string()),
            encoded(filter.actor.as_deref().unwrap_or("")),
            encoded(filter.action.as_deref().unwrap_or("")),
        )
    });
    let rows = entries
        .items
        .iter()
        .map(|e| EntryRow {
            at: e.at.format("%Y-%m-%d %H:%M:%S").to_string(),
            who: e.actor.name.clone(),
            via: via(e.actor.kind),
            action: e.action.clone(),
            repo: e.repo_id.map(|r| {
                let slug = slugs
                    .get(&r)
                    .cloned()
                    .unwrap_or_else(|| format!("repo {}", r.0));
                (r.0, slug)
            }),
            job: e.job_id.map(|j| j.0),
            detail: detail_of(e),
        })
        .collect();
    let mut repo_choices: Vec<Choice> = repos
        .values()
        .map(|r| Choice {
            value: r.id.0.to_string(),
            label: r.slug.clone(),
            selected: filter.repo == Some(r.id),
        })
        .collect();
    repo_choices.sort_by(|a, b| a.label.cmp(&b.label));
    let chosen = filter.action.clone().unwrap_or_default();
    let mut actions: Vec<Choice> = Vec::new();
    for action in store.audit_actions().await? {
        // Each family once, as `job.` for every `job.*`.
        if let Some((family, _)) = action.split_once('.') {
            let value = format!("{family}.");
            if !actions.iter().any(|c| c.value == value) {
                actions.push(Choice {
                    selected: chosen == value,
                    label: format!("{family}.* (all)"),
                    value,
                });
            }
        }
        actions.push(Choice {
            selected: chosen == action,
            label: action.clone(),
            value: action,
        });
    }
    let filtered = filter.repo.is_some() || filter.actor.is_some() || filter.action.is_some();
    render(&AuditPage {
        nonce: nonce.as_str(),
        me: &me,
        repos: repo_choices,
        actions,
        actor: filter.actor.as_deref().unwrap_or(""),
        rows,
        next,
        filtered,
    })
}
