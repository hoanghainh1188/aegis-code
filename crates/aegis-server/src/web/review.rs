//! `/jobs/{id}/review`: the reviewer's verdict on a job's PR, written to
//! the ledger with [`IssueLedger::reviewed`] as `aegis review` does.
//!
//! The reviewer does not type the review time (ADR 0005): the form carries
//! the moment the page was served, and the time is counted from it to the
//! submit. The page shows neither the hidden test result nor anything
//! derived from it.

use aegis_ledger::{IssueLedger, Outcome, PriceTable, Review, ReviewVerdict};
use aegis_store::{Job, JobId, Store};
use askama::Template;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect};
use axum::{Extension, Form};
use serde::Deserialize;

use super::jobs::record;
use super::{Nonce, WebError, WebResult, render};

/// A page older than this is refused: its time would not be a review's.
pub const REVIEW_MAX_AGE_MS: i64 = 24 * 60 * 60 * 1000;
/// Clock difference tolerated between the page and the submit.
const CLOCK_SKEW_MS: i64 = 5_000;
/// `IssueLedger` prices model calls; a review prices nothing.
const UNPRICED: &str = "regional_surcharge = 0.0\n\
    [cache_write_multiplier]\n5m = 0.0\n1h = 0.0\n\
    [models.unpriced]\ninput = 0.0\noutput = 0.0\ncache_read_multiplier = 0.0\n";

/// Minutes from serving the form at `opened_at_ms` to its submit at
/// `now_ms`, both Unix milliseconds.
pub fn review_minutes(opened_at_ms: i64, now_ms: i64) -> Result<f64, &'static str> {
    let elapsed = now_ms - opened_at_ms;
    if elapsed < -CLOCK_SKEW_MS {
        return Err("thời điểm mở trang nằm ở tương lai; tải lại trang");
    }
    if elapsed > REVIEW_MAX_AGE_MS {
        return Err("trang đã mở quá 24 giờ; tải lại trang rồi duyệt");
    }
    Ok(elapsed.max(0) as f64 / 60_000.0)
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn verdict(text: &str) -> Option<ReviewVerdict> {
    match text {
        "accepted" => Some(ReviewVerdict::Accepted),
        "accepted_with_edits" => Some(ReviewVerdict::AcceptedWithEdits),
        "rejected" => Some(ReviewVerdict::Rejected),
        _ => None,
    }
}

#[derive(Template)]
#[template(path = "review.html")]
struct ReviewPage<'a> {
    nonce: &'a str,
    id: i64,
    issue_ref: String,
    pr_url: Option<String>,
    existing: Option<String>,
    opened_at: i64,
    error: Option<String>,
}

async fn reviewable(store: &Store, id: i64) -> Result<Job, WebError> {
    let job = store
        .job(JobId(id))
        .await?
        .ok_or_else(|| WebError::not_found(format!("job {id}")))?;
    if job.outcome != Some(Outcome::PrOpened) {
        return Err(WebError::new(
            StatusCode::CONFLICT,
            format!("job {id} không mở PR nên không có gì để duyệt"),
        ));
    }
    Ok(job)
}

async fn page(
    store: &Store,
    nonce: &Nonce,
    job: &Job,
    opened_at: i64,
    error: Option<String>,
) -> WebResult {
    let existing = record(store, job).await?.review.map(|r| {
        format!(
            "{:?}, sửa {} dòng, {:.1} phút",
            r.verdict, r.lines_edited, r.review_minutes
        )
    });
    let status = if error.is_some() {
        StatusCode::UNPROCESSABLE_ENTITY
    } else {
        StatusCode::OK
    };
    let html = render(&ReviewPage {
        nonce: nonce.as_str(),
        id: job.id.0,
        issue_ref: job.issue_ref.clone(),
        pr_url: job.pr_url.clone().filter(|u| u.starts_with("https://")),
        existing,
        opened_at,
        error,
    })?;
    Ok((status, html).into_response())
}

pub async fn form(
    State(store): State<Store>,
    Extension(nonce): Extension<Nonce>,
    Path(id): Path<i64>,
) -> WebResult {
    let job = reviewable(&store, id).await?;
    page(&store, &nonce, &job, now_ms(), None).await
}

/// The form's fields as text: a bad value is shown back, not a 400.
#[derive(Debug, Deserialize)]
pub struct ReviewForm {
    #[serde(default)]
    verdict: String,
    #[serde(default)]
    lines_edited: String,
    #[serde(default)]
    broke_tests: Option<String>,
    #[serde(default)]
    replace: Option<String>,
    #[serde(default)]
    opened_at: String,
}

fn parse(form: &ReviewForm, now: i64) -> Result<Review, String> {
    let verdict = verdict(&form.verdict).ok_or("chọn một kết luận")?;
    let lines_edited = form
        .lines_edited
        .trim()
        .parse::<u32>()
        .map_err(|_| "số dòng sửa là số nguyên từ 0")?;
    let opened_at = form
        .opened_at
        .parse::<i64>()
        .map_err(|_| "thiếu thời điểm mở trang; tải lại trang")?;
    Ok(Review {
        verdict,
        lines_edited,
        review_minutes: review_minutes(opened_at, now)?,
        broke_tests: form.broke_tests.is_some(),
    })
}

pub async fn submit(
    State(store): State<Store>,
    Extension(nonce): Extension<Nonce>,
    Path(id): Path<i64>,
    Form(form): Form<ReviewForm>,
) -> WebResult {
    let job = reviewable(&store, id).await?;
    let now = now_ms();
    let opened_at = form.opened_at.parse().unwrap_or(now);
    let review = match parse(&form, now) {
        Ok(review) => review,
        Err(why) => return page(&store, &nonce, &job, opened_at, Some(why)).await,
    };
    let prices = PriceTable::from_toml(UNPRICED)?;
    let ledger = store.ledger();
    // `regional` only changes the price of model calls.
    let mut issue = IssueLedger::open(&ledger, &prices, false, job.ledger_key.clone()).await?;
    if issue.record().review.is_some() && form.replace.is_none() {
        let why = "PR này đã có review; đánh dấu \"thay review cũ\" để đổi".to_owned();
        return page(&store, &nonce, &job, opened_at, Some(why)).await;
    }
    issue.reviewed(review).await?;
    Ok(Redirect::to(&format!("/jobs/{id}")).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_review_time_is_counted_from_the_page() {
        assert_eq!(review_minutes(0, 90_000), Ok(1.5));
        assert_eq!(review_minutes(1_000, 0), Ok(0.0));
        assert!(review_minutes(10_000, 0).is_err());
        assert!(review_minutes(0, REVIEW_MAX_AGE_MS + 1).is_err());
        PriceTable::from_toml(UNPRICED).unwrap();
    }

    #[test]
    fn a_form_needs_a_verdict_and_whole_lines() {
        let form = |verdict: &str, lines: &str| ReviewForm {
            verdict: verdict.into(),
            lines_edited: lines.into(),
            broke_tests: None,
            replace: None,
            opened_at: "0".into(),
        };
        let review = parse(&form("accepted_with_edits", " 3 "), 120_000).unwrap();
        assert_eq!(
            (review.verdict, review.lines_edited, review.review_minutes),
            (ReviewVerdict::AcceptedWithEdits, 3, 2.0)
        );
        assert!(parse(&form("ok", "3"), 0).is_err());
        assert!(parse(&form("rejected", "-1"), 0).is_err());
    }
}
