//! A repo's deprecation warnings, cleaned up on a schedule (#705). A repo
//! whose settings have `deprecation_poll` gets a `deprecation` job every
//! `every_s`, only inside its off-hours window and only while no batch of
//! its own is queued or running. The job lists the warnings at the base
//! with the profile's `[deprecations]` command and fixes them as one batch
//! and PR (`aegis-lifecycle`), skipping the warnings earlier batches took
//! (those still running or fixed in a PR), with at most `max_groups`
//! groups. The repo's budget (#31) refuses a job past it, as for any job.
//!
//! Each repo's schedule is a row of the database (`deprecations:<repo
//! id>`), claimed only inside the window, by one process per period
//! (ADR 0003 §4); the `api` role looks every `deprecations.check_s`.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aegis_ledger::{IssueId, IssueKind};
use aegis_lifecycle::IssueInput;
use aegis_store::{Delivered, Job, JobSource, NewJob, Page, Repo, Store, StoreError};
use serde::Deserialize;
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;

use crate::runner::Attempt;
use crate::tracker::Unusable;

const REPO_PAGE: u32 = 100;
const SLUG_IN_KEY: usize = 16;
/// Groups one batch may have, as the lifecycle's own limit.
const MAX_GROUPS: u32 = 8;

/// A repo's deprecation schedule (`repos.config.deprecation_poll`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeprecationPoll {
    /// Seconds between two batches: one hour to one week.
    #[serde(default = "default_every_s")]
    pub every_s: u64,
    /// The off-hours window, in the repo's local time: from `from_hour`
    /// to `to_hour` (it may run past midnight).
    #[serde(default = "default_from_hour")]
    pub from_hour: u8,
    #[serde(default = "default_to_hour")]
    pub to_hour: u8,
    /// The repo's local time against UTC, in minutes (420 is UTC+7).
    #[serde(default)]
    pub utc_offset_minutes: i16,
    /// Groups of one batch: 1 to 8. Each group is an agent session.
    #[serde(default = "default_max_groups")]
    pub max_groups: u32,
}

fn default_every_s() -> u64 {
    86_400
}
fn default_from_hour() -> u8 {
    20
}
fn default_to_hour() -> u8 {
    6
}
fn default_max_groups() -> u32 {
    4
}

impl DeprecationPoll {
    pub fn check(&self) -> Result<(), String> {
        if !(3_600..=604_800).contains(&self.every_s) {
            return Err("deprecation_poll.every_s must be 3600 to 604800".into());
        }
        if self.from_hour > 23 || self.to_hour > 23 || self.from_hour == self.to_hour {
            return Err(
                "deprecation_poll: `from_hour` and `to_hour` are two different hours, 0 to 23"
                    .into(),
            );
        }
        if !(-720..=840).contains(&self.utc_offset_minutes) {
            return Err("deprecation_poll.utc_offset_minutes must be -720 to 840".into());
        }
        if !(1..=MAX_GROUPS).contains(&self.max_groups) {
            return Err("deprecation_poll.max_groups must be 1 to 8".into());
        }
        Ok(())
    }

    pub fn every(&self) -> Duration {
        Duration::from_secs(self.every_s)
    }

    /// Whether `now` is inside the repo's off-hours window.
    pub fn off_hours(&self, now: OffsetDateTime) -> bool {
        let local = now + time::Duration::minutes(i64::from(self.utc_offset_minutes));
        let hour = local.hour();
        if self.from_hour < self.to_hour {
            (self.from_hour..self.to_hour).contains(&hour)
        } else {
            hour >= self.from_hour || hour < self.to_hour
        }
    }
}

/// `repo`'s deprecation schedule, `None` when it has none.
pub fn poll_config(repo: &Repo) -> Result<Option<DeprecationPoll>, Unusable> {
    let bad = |e: String| Unusable::Config(format!("repo `{}`: {e}", repo.slug));
    let poll: DeprecationPoll = match repo.config.get("deprecation_poll") {
        None | Some(serde_json::Value::Null) => return Ok(None),
        Some(value) => serde_json::from_value(value.clone())
            .map_err(|e| bad(format!("deprecation_poll: {e}")))?,
    };
    poll.check().map_err(bad)?;
    Ok(Some(poll))
}

fn schedule(repo: &Repo) -> String {
    format!("deprecations:{}", repo.id.0)
}

fn new_batch(repo: &Repo) -> Option<(String, IssueId)> {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    let slug: String = repo
        .slug
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .take(SLUG_IN_KEY)
        .collect();
    let name = format!("deprecations-{ms}");
    let key = format!("{}-{}-dep-{ms}", slug.trim_matches('-'), repo.id.0);
    Some((name, IssueId::parse(key).ok()?))
}

/// Queues `repo`'s batch now, unless one is live; the job refused for the
/// repo's budget is returned.
pub async fn queue_batch(store: &Store, repo: &Repo) -> Result<Option<Job>, StoreError> {
    let Some((issue_ref, ledger_key)) = new_batch(repo) else {
        return Ok(None);
    };
    let job = NewJob {
        repo_id: repo.id,
        issue_ref,
        ledger_key,
        source: JobSource::Deprecation,
        priority: None,
        sla_deadline: None,
    };
    match store.deliver_deprecation_batch(&job).await? {
        Some(Delivered::Queued(job)) => {
            tracing::info!(job = job.id.0, repo = %repo.slug, "deprecation batch queued");
            Ok(None)
        }
        Some(Delivered::Refused(job)) => Ok(Some(*job)),
        Some(Delivered::Duplicate | Delivered::AlreadyLive(_)) | None => Ok(None),
    }
}

/// Queues a batch for every repo inside its window whose schedule is due
/// and this process claims, at `now`. The jobs refused for a repo's budget
/// are returned.
pub async fn check_due_at(store: &Store, now: OffsetDateTime) -> Result<Vec<Job>, StoreError> {
    let mut refused = Vec::new();
    let mut page = Page::first(REPO_PAGE);
    loop {
        let repos = store.list_repos(page).await?;
        for repo in &repos.items {
            let poll = match poll_config(repo) {
                Ok(Some(poll)) => poll,
                Ok(None) => continue,
                Err(e) => {
                    tracing::warn!(repo = %repo.slug, error = %e, "deprecations not scheduled");
                    continue;
                }
            };
            // Outside the window the schedule is not claimed: its next
            // run waits for the window, not for another period.
            if poll.off_hours(now) && store.claim_poll(&schedule(repo), poll.every()).await? {
                refused.extend(queue_batch(store, repo).await?);
            }
        }
        match repos.next_page() {
            Some(next) => page = next,
            None => return Ok(refused),
        }
    }
}

/// Checks the schedules every `check` until `shutdown`.
pub async fn run_poller(store: Store, check: Duration, shutdown: CancellationToken) {
    tracing::info!(check = ?check, "deprecation scheduler started");
    loop {
        match check_due_at(&store, OffsetDateTime::now_utc()).await {
            Ok(refused) => {
                for job in refused {
                    tracing::warn!(job = job.id.0, reason = ?job.reason, "deprecation batch not run");
                }
            }
            Err(e) => tracing::error!(error = %e, "scheduling deprecation batches"),
        }
        tokio::select! {
            () = shutdown.cancelled() => break,
            () = tokio::time::sleep(check) => {}
        }
    }
    tracing::info!("deprecation scheduler stopped");
}

/// The issue of a `deprecation` job of `repo`: the warnings earlier
/// batches took, to skip, and the repo's limit on groups.
pub async fn deprecation_issue(
    job: &Job,
    repo: &Repo,
    store: &Store,
) -> Result<IssueInput, Attempt> {
    let taken = store
        .deprecations_taken(repo.id)
        .await
        .map_err(|e| Attempt::Infra(e.to_string()))?;
    let max_groups = poll_config(repo)
        .ok()
        .flatten()
        .map_or(default_max_groups(), |p| p.max_groups);
    let asked = serde_json::json!({ "skip": taken, "max_groups": max_groups });
    Ok(IssueInput {
        cve: None,
        id: job.ledger_key.as_str().to_owned(),
        title: format!("Deprecation warnings in {}", repo.slug),
        body: format!(
            "The scheduled cleanup of the repo's deprecation warnings (#705): each call of a \
             deprecated API at the base, but those earlier batches took.\n\n```json\n{asked}\n```\n"
        ),
        url: repo.url.clone(),
        base: None,
        hidden: None,
        kind: Some(IssueKind::Deprecation),
        source: Some("deprecation".into()),
        memory: String::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    #[test]
    fn the_schedule_has_defaults_and_is_checked() {
        let poll: DeprecationPoll = serde_json::from_str("{}").unwrap();
        assert_eq!(
            (poll.every_s, poll.from_hour, poll.to_hour),
            (86_400, 20, 6)
        );
        assert_eq!(poll.max_groups, 4);
        assert!(poll.check().is_ok());
        for bad in [
            r#"{"every_s": 60}"#,
            r#"{"from_hour": 24}"#,
            r#"{"from_hour": 3, "to_hour": 3}"#,
            r#"{"utc_offset_minutes": 900}"#,
            r#"{"max_groups": 0}"#,
            r#"{"max_groups": 9}"#,
        ] {
            let poll: DeprecationPoll = serde_json::from_str(bad).unwrap();
            assert!(poll.check().is_err(), "{bad}");
        }
        assert!(serde_json::from_str::<DeprecationPoll>(r#"{"hours": 2}"#).is_err());
    }

    #[test]
    fn the_window_is_in_the_repos_local_time_and_may_cross_midnight() {
        // 20:00 to 06:00 at UTC+7.
        let poll: DeprecationPoll = serde_json::from_str(r#"{"utc_offset_minutes": 420}"#).unwrap();
        assert!(
            poll.off_hours(datetime!(2026-11-28 14:00 UTC)),
            "21:00 local"
        );
        assert!(
            poll.off_hours(datetime!(2026-11-28 22:30 UTC)),
            "05:30 local"
        );
        assert!(
            !poll.off_hours(datetime!(2026-11-28 23:00 UTC)),
            "06:00 local"
        );
        assert!(
            !poll.off_hours(datetime!(2026-11-28 03:00 UTC)),
            "10:00 local"
        );
        let day: DeprecationPoll =
            serde_json::from_str(r#"{"from_hour": 1, "to_hour": 4}"#).unwrap();
        assert!(day.off_hours(datetime!(2026-11-28 02:00 UTC)));
        assert!(!day.off_hours(datetime!(2026-11-28 04:00 UTC)));
    }
}
