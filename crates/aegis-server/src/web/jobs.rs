//! `/jobs`: the jobs with their status, tiers, cost and time, filtered by
//! repo and status (htmx swaps the table); `/jobs/{id}`: one job's runs
//! and what its ledger holds, call by call.
//!
//! Hidden test results are never shown (ADR 0005): the reviewer must not
//! know them before giving a verdict.

use aegis_ledger::{IssueRecord, LedgerEvent, LedgerStore, Outcome};
use aegis_store::{Job, JobFilter, JobStatus, Page, RepoId, RepoRole, Store};
use askama::Template;
use axum::Extension;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, header};
use serde::Deserialize;

use super::dashboard::all_repos;
use super::view::{dollars, seconds, tiers};
use super::{Me, Nonce, WebError, WebResult, job_for, render};
use crate::auth::Access;

const PAGE_SIZE: u32 = 50;

/// A PR link only when it is one: the URL is shown as an `href`.
fn pr_link(job: &Job) -> Option<String> {
    job.pr_url
        .as_deref()
        .filter(|u| u.starts_with("https://"))
        .map(str::to_owned)
}

fn outcome(job: &Job) -> String {
    job.outcome.map_or("—".into(), |o| format!("{o:?}"))
}

pub(super) async fn record(store: &Store, job: &Job) -> Result<IssueRecord, WebError> {
    let events = store.ledger().events(&job.ledger_key).await?;
    Ok(IssueRecord::from_events(job.ledger_key.clone(), &events))
}

struct JobRow {
    id: i64,
    repo: String,
    issue_ref: String,
    status: &'static str,
    outcome: String,
    tiers: String,
    cost: String,
    wall_time: String,
    created_at: String,
    pr_url: Option<String>,
    can_review: bool,
}

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    #[serde(default)]
    repo_id: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    cursor: String,
}

struct Choice {
    value: String,
    label: String,
    selected: bool,
}

#[derive(Template)]
#[template(path = "jobs.html")]
struct JobsPage<'a> {
    nonce: &'a str,
    me: &'a Me,
    repos: Vec<Choice>,
    statuses: Vec<Choice>,
    rows: Vec<JobRow>,
    next: Option<String>,
}

#[derive(Template)]
#[template(path = "jobs_table.html")]
struct JobsTable {
    rows: Vec<JobRow>,
    next: Option<String>,
}

/// An empty select means "any"; anything else must parse.
fn optional<T>(text: &str, parse: impl Fn(&str) -> Option<T>) -> Result<Option<T>, WebError> {
    if text.is_empty() {
        return Ok(None);
    }
    parse(text).map(Some).ok_or_else(|| {
        WebError::new(
            axum::http::StatusCode::BAD_REQUEST,
            format!("giá trị lọc không hợp lệ: `{text}`"),
        )
    })
}

pub async fn list(
    State(store): State<Store>,
    Extension(nonce): Extension<Nonce>,
    Extension(access): Extension<Access>,
    Extension(me): Extension<Me>,
    headers: HeaderMap,
    Query(q): Query<ListQuery>,
) -> WebResult {
    let repo_id = optional(&q.repo_id, |t| t.parse().ok().map(RepoId))?;
    let status = optional(&q.status, JobStatus::parse)?;
    let cursor = optional(&q.cursor, |t| t.parse::<i64>().ok())?;
    let page = cursor.map_or(Page::first(PAGE_SIZE), |c| Page::after(c, PAGE_SIZE));
    let jobs = store
        .list_jobs_in(
            &JobFilter { repo_id, status },
            access.visible().as_deref(),
            page,
        )
        .await?;
    let mut repos = all_repos(&store).await?;
    repos.retain(|id, _| access.can(*id, RepoRole::Viewer));
    let next = jobs.next_page().and_then(Page::cursor).map(|c| {
        format!(
            "/jobs?repo_id={}&status={}&cursor={c}",
            repo_id.map_or(String::new(), |r| r.0.to_string()),
            status.map_or("", JobStatus::as_str),
        )
    });
    let mut rows = Vec::with_capacity(jobs.items.len());
    for job in &jobs.items {
        let record = record(&store, job).await?;
        rows.push(JobRow {
            id: job.id.0,
            repo: repos
                .get(&job.repo_id)
                .map_or(format!("repo {}", job.repo_id.0), |r| r.slug.clone()),
            issue_ref: job.issue_ref.clone(),
            status: job.status.as_str(),
            outcome: outcome(job),
            tiers: tiers(&record),
            cost: dollars(record.cost_usd),
            wall_time: seconds(record.wall_time_s),
            created_at: job.created_at.format("%Y-%m-%d %H:%M").to_string(),
            pr_url: pr_link(job),
            can_review: job.outcome == Some(Outcome::PrOpened)
                && access.can(job.repo_id, RepoRole::Reviewer),
        });
    }
    // A history restore wants the whole page, not the table.
    if headers.contains_key("hx-request") && !headers.contains_key("hx-history-restore-request") {
        let mut response = render(&JobsTable { rows, next })?;
        response
            .headers_mut()
            .insert(header::VARY, HeaderValue::from_static("HX-Request"));
        return Ok(response);
    }
    let repos = repos
        .values()
        .map(|r| Choice {
            value: r.id.0.to_string(),
            label: r.slug.clone(),
            selected: repo_id == Some(r.id),
        })
        .collect();
    let statuses = ["queued", "running", "done", "failed", "cancelled"]
        .into_iter()
        .map(|s| Choice {
            value: s.into(),
            label: s.into(),
            selected: status.map(JobStatus::as_str) == Some(s),
        })
        .collect();
    render(&JobsPage {
        nonce: nonce.as_str(),
        me: &me,
        repos,
        statuses,
        rows,
        next,
    })
}

struct EventRow {
    kind: &'static str,
    detail: String,
    cost: String,
}

/// One ledger event as a line of the history; `None` for what the
/// reviewer must not see.
fn event_row(event: &LedgerEvent) -> Option<EventRow> {
    let row = |kind, detail: String| EventRow {
        kind,
        detail,
        cost: String::new(),
    };
    Some(match event {
        LedgerEvent::Classified { kind } => row("phân loại", format!("{kind:?}")),
        LedgerEvent::ModelCall {
            tier,
            model,
            tokens,
            cost_usd,
        } => EventRow {
            kind: "gọi model",
            detail: format!(
                "{tier:?} · {model} · input {} · output {} · cache đọc {}",
                tokens.input, tokens.output, tokens.cache_read
            ),
            cost: dollars(*cost_usd),
        },
        LedgerEvent::ToolCalls { tier, calls } => {
            let calls: Vec<String> = calls
                .iter()
                .map(|c| {
                    let target = c
                        .target
                        .as_deref()
                        .map_or(String::new(), |t| format!("({t})"));
                    let error = if c.is_error { " ✘" } else { "" };
                    format!("{}{target}{error}", c.name)
                })
                .collect();
            row("công cụ", format!("{tier:?}: {}", calls.join(", ")))
        }
        LedgerEvent::TestReminded { tier, calls } => {
            row("nhắc chạy test", format!("{tier:?}, sau {calls} lượt"))
        }
        LedgerEvent::ImpactChecked { files, error } => {
            let text = match (error, files.is_empty()) {
                (Some(why), _) => format!("không hỏi được: {why}"),
                (None, true) => "không có nơi gọi ngoài bản vá".to_owned(),
                (None, false) => files.join(", "),
            };
            row("đồ thị gọi", text)
        }
        LedgerEvent::Escalated { reason, calls } => {
            row("lên Expert", format!("sau {calls} lượt: {reason}"))
        }
        LedgerEvent::GateRun(run) => {
            let result = match (&run.failed_step, run.passed) {
                (_, true) => "qua".to_owned(),
                (Some(step), false) => format!("không qua ở bước {step}"),
                (None, false) => "không qua".to_owned(),
            };
            row(
                "gate",
                format!("{result} · {}", seconds(Some(run.duration_s))),
            )
        }
        LedgerEvent::SecretsRedacted { count } => row("che secret", format!("{count} giá trị")),
        LedgerEvent::HiddenTests { .. } => return None,
        LedgerEvent::Finished {
            outcome,
            wall_time_s,
        } => row(
            "kết thúc",
            format!("{outcome:?} · {}", seconds(Some(*wall_time_s))),
        ),
        LedgerEvent::Reviewed(r) => row(
            "review",
            format!(
                "{:?} · sửa {} dòng · {:.1} phút",
                r.verdict, r.lines_edited, r.review_minutes
            ),
        ),
    })
}

struct RunRow {
    attempt: u32,
    status: &'static str,
    error: String,
    started_at: String,
    ended_at: String,
}

#[derive(Template)]
#[template(path = "job.html")]
struct JobPage<'a> {
    nonce: &'a str,
    me: &'a Me,
    id: i64,
    repo: String,
    issue_ref: String,
    ledger_key: String,
    status: &'static str,
    outcome: String,
    reason: String,
    pr_url: Option<String>,
    can_review: bool,
    tiers: String,
    model_calls: u32,
    cost: String,
    wall_time: String,
    runs: Vec<RunRow>,
    events: Vec<EventRow>,
}

pub async fn detail(
    State(store): State<Store>,
    Extension(nonce): Extension<Nonce>,
    Extension(access): Extension<Access>,
    Extension(me): Extension<Me>,
    Path(id): Path<i64>,
) -> WebResult {
    let job = job_for(&store, &access, id, RepoRole::Viewer).await?;
    let repo = store.repo(job.repo_id).await?;
    let events = store.ledger().events(&job.ledger_key).await?;
    let record = IssueRecord::from_events(job.ledger_key.clone(), &events);
    let time = |t: chrono::DateTime<chrono::Utc>| t.format("%Y-%m-%d %H:%M:%S").to_string();
    let runs = store
        .list_runs(job.id, Page::first(Page::MAX_LIMIT))
        .await?
        .items
        .into_iter()
        .map(|r| RunRow {
            attempt: r.attempt,
            status: r.status.as_str(),
            error: r.error.unwrap_or_default(),
            started_at: time(r.started_at),
            ended_at: r.ended_at.map_or("—".into(), time),
        })
        .collect();
    render(&JobPage {
        nonce: nonce.as_str(),
        me: &me,
        id,
        repo: repo.map_or(format!("repo {}", job.repo_id.0), |r| r.slug),
        issue_ref: job.issue_ref.clone(),
        ledger_key: job.ledger_key.as_str().to_owned(),
        status: job.status.as_str(),
        outcome: outcome(&job),
        reason: job.reason.clone().unwrap_or_default(),
        pr_url: pr_link(&job),
        can_review: job.outcome == Some(Outcome::PrOpened)
            && access.can(job.repo_id, RepoRole::Reviewer),
        tiers: tiers(&record),
        model_calls: record.model_calls,
        cost: dollars(record.cost_usd),
        wall_time: seconds(record.wall_time_s),
        runs,
        events: events.iter().filter_map(event_row).collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_tests_never_reach_the_history() {
        let hidden = LedgerEvent::HiddenTests {
            passed: true,
            duration_s: 1.0,
        };
        assert!(event_row(&hidden).is_none());
        let finished = LedgerEvent::Finished {
            outcome: Outcome::PrOpened,
            wall_time_s: 90.0,
        };
        assert_eq!(event_row(&finished).unwrap().detail, "PrOpened · 1.5 phút");
    }
}
