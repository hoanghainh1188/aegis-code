//! Error spikes, found before any ticket (#706). A repo whose settings have
//! `apm` (Sentry or Datadog, #452) and `apm_anomaly` (the schedule and the
//! thresholds) is checked every `apm_anomaly.every_s`: its APM source's
//! errors of the last `window_minutes`, counted per error type, against the
//! same window `baseline_days` earlier. A type that is new, or grew past
//! `ratio` times its baseline, is a spike; right after a release the
//! thresholds are halved (`aegis_tracker::anomalies`).
//!
//! Each spike becomes an `apm` job, written with the spike in one
//! transaction (`Store::deliver_anomaly`): a fingerprint diagnosed within
//! `dedupe_days` is not diagnosed again, and a repo gets at most
//! `max_jobs_per_day`. The job diagnoses the spike in the diagnosis lane
//! (ADR 0012) whatever the repo's `triage_lane`, and never opens a PR; its
//! report is kept with the job for its page.
//!
//! Each repo's schedule is a row of the database (`apm:<repo id>`); the
//! `api` role checks every `apm.check_s` which are due, one process per
//! period (ADR 0003 §4).

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aegis_ledger::{IssueId, IssueKind};
use aegis_lifecycle::IssueInput;
use aegis_secrets::Secrets;
use aegis_store::{
    AnomalyLimits, AnomalySkipped, Delivered, Job, JobSource, NewAnomaly, NewJob, Page, Repo,
    Store, StoreError,
};
use aegis_tracker::{SpikeRules, anomalies};
use serde::Deserialize;
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;

use crate::runner::Attempt;
use crate::tracker::{Unusable, apm_client};

/// Repos read per page while checking.
const REPO_PAGE: u32 = 100;
/// Characters of the repo's slug in a spike's ledger key.
const SLUG_IN_KEY: usize = 16;

/// A repo's error spike settings (`repos.config.apm_anomaly`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnomalyConfig {
    /// Seconds between two checks of the repo: 300 to 86 400.
    #[serde(default = "default_every_s")]
    pub every_s: u64,
    /// The window counted: 5 to 1440 minutes.
    #[serde(default = "default_window_minutes")]
    pub window_minutes: u32,
    /// How far back the baseline window is: 1 to 30 days.
    #[serde(default = "default_baseline_days")]
    pub baseline_days: u32,
    /// A type at least this many times its baseline is a spike.
    #[serde(default = "default_ratio")]
    pub ratio: f64,
    /// Fewer errors than this in the window are never a spike.
    #[serde(default = "default_min_count")]
    pub min_count: u64,
    /// Spikes diagnosed per repo in any 24 hours: 1 to 50.
    #[serde(default = "default_max_jobs_per_day")]
    pub max_jobs_per_day: u32,
    /// A fingerprint diagnosed within this many days is not diagnosed
    /// again: 0 to 90.
    #[serde(default = "default_dedupe_days")]
    pub dedupe_days: u32,
}

fn default_every_s() -> u64 {
    900
}
fn default_window_minutes() -> u32 {
    60
}
fn default_baseline_days() -> u32 {
    7
}
fn default_ratio() -> f64 {
    SpikeRules::default().ratio
}
fn default_min_count() -> u64 {
    SpikeRules::default().min_count
}
fn default_max_jobs_per_day() -> u32 {
    3
}
fn default_dedupe_days() -> u32 {
    7
}

impl AnomalyConfig {
    pub fn rules(&self) -> SpikeRules {
        SpikeRules {
            ratio: self.ratio,
            min_count: self.min_count,
        }
    }

    pub fn check(&self) -> Result<(), String> {
        let range = |ok: bool, what: &str| if ok { Ok(()) } else { Err(what.to_owned()) };
        range(
            (300..=86_400).contains(&self.every_s),
            "apm_anomaly.every_s must be 300 to 86400",
        )?;
        range(
            (5..=1_440).contains(&self.window_minutes),
            "apm_anomaly.window_minutes must be 5 to 1440",
        )?;
        range(
            (1..=30).contains(&self.baseline_days),
            "apm_anomaly.baseline_days must be 1 to 30",
        )?;
        range(
            (1..=50).contains(&self.max_jobs_per_day),
            "apm_anomaly.max_jobs_per_day must be 1 to 50",
        )?;
        range(
            self.dedupe_days <= 90,
            "apm_anomaly.dedupe_days must be 0 to 90",
        )?;
        self.rules().check().map_err(|e| e.to_string())
    }

    pub fn every(&self) -> Duration {
        Duration::from_secs(self.every_s)
    }

    fn limits(&self) -> AnomalyLimits {
        AnomalyLimits {
            dedupe: Duration::from_secs(u64::from(self.dedupe_days) * 86_400),
            per_day: self.max_jobs_per_day,
        }
    }
}

/// `repo`'s spike settings, `None` when it is not checked.
pub fn anomaly_config(repo: &Repo) -> Result<Option<AnomalyConfig>, Unusable> {
    let bad = |e: String| Unusable::Config(format!("repo `{}`: {e}", repo.slug));
    let config: AnomalyConfig = match repo.config.get("apm_anomaly") {
        None | Some(serde_json::Value::Null) => return Ok(None),
        Some(value) => {
            serde_json::from_value(value.clone()).map_err(|e| bad(format!("apm_anomaly: {e}")))?
        }
    };
    config.check().map_err(bad)?;
    if repo
        .config
        .get("apm")
        .is_none_or(serde_json::Value::is_null)
    {
        return Err(bad("apm_anomaly needs `apm`".into()));
    }
    Ok(Some(config))
}

fn schedule(repo: &Repo) -> String {
    format!("apm:{}", repo.id.0)
}

fn millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis())
}

/// The `n`th spike of a check: its `issue_ref` and ledger key.
fn new_spike(repo: &Repo, ms: u128, n: usize) -> Option<(String, IssueId)> {
    let slug: String = repo
        .slug
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .take(SLUG_IN_KEY)
        .collect();
    let name = format!("apm-{ms}-{n}");
    let key = format!("{}-{}-{name}", slug.trim_matches('-'), repo.id.0);
    Some((name, IssueId::parse(key).ok()?))
}

fn minutes(n: u32) -> String {
    if n.is_multiple_of(60) {
        match n / 60 {
            1 => "the last hour".to_owned(),
            h => format!("the last {h} hours"),
        }
    } else {
        format!("the last {n} minutes")
    }
}

/// One check of `repo` at `now`: its spikes queued as `apm` jobs. The jobs
/// refused for the repo's budget are returned; a repo whose APM fails is
/// logged and skipped.
pub async fn check_repo_at(
    store: &Store,
    secrets: &Secrets,
    repo: &Repo,
    now: OffsetDateTime,
) -> Result<Vec<Job>, StoreError> {
    let config = match anomaly_config(repo) {
        Ok(Some(config)) => config,
        Ok(None) => return Ok(Vec::new()),
        Err(e) => {
            tracing::warn!(repo = %repo.slug, error = %e, "APM spikes not checked");
            return Ok(Vec::new());
        }
    };
    let client = match apm_client(repo, secrets).await {
        Ok(Some(client)) if client.counts_errors() => client,
        Ok(_) => {
            tracing::warn!(repo = %repo.slug, "APM spikes not checked: its APM source counts no errors per type");
            return Ok(Vec::new());
        }
        Err(e) => {
            tracing::warn!(repo = %repo.slug, error = %e, "APM spikes not checked");
            return Ok(Vec::new());
        }
    };
    let window = time::Duration::minutes(i64::from(config.window_minutes));
    let back = time::Duration::days(i64::from(config.baseline_days));
    let start = now - window;
    let counted = async {
        let current = client.error_counts(start, now).await?;
        let baseline = client.error_counts(start - back, now - back).await?;
        Ok::<_, aegis_tracker::TrackerError>((current, baseline))
    };
    let (current, baseline) = match counted.await {
        Ok(counts) => counts,
        Err(e) => {
            tracing::warn!(repo = %repo.slug, error = %e, "counting APM errors");
            return Ok(Vec::new());
        }
    };
    let release = client.latest_release().await.unwrap_or_else(|e| {
        tracing::warn!(repo = %repo.slug, error = %e, "reading the newest APM release");
        None
    });
    let spikes = anomalies(
        &current,
        &baseline,
        release.as_ref(),
        start,
        &config.rules(),
    );
    let mut refused = Vec::new();
    let ms = millis();
    let window_text = minutes(config.window_minutes);
    for (n, spike) in spikes.iter().enumerate() {
        let Some((name, ledger_key)) = new_spike(repo, ms, n) else {
            continue;
        };
        let job = NewJob {
            repo_id: repo.id,
            issue_ref: name.clone(),
            ledger_key,
            source: JobSource::Apm,
            priority: None,
            sla_deadline: None,
        };
        let anomaly = NewAnomaly {
            fingerprint: spike.fingerprint.chars().take(300).collect(),
            title: spike.headline(),
            body: spike.to_markdown(client.provider(), &window_text),
        };
        match store
            .deliver_anomaly(&job, &anomaly, config.limits())
            .await?
        {
            Ok(Delivered::Queued(job)) => {
                tracing::info!(job = job.id.0, repo = %repo.slug, fingerprint = %spike.fingerprint, count = spike.count, baseline = spike.baseline, "APM error spike queued for diagnosis");
            }
            Ok(Delivered::Refused(job)) => refused.push(*job),
            Ok(Delivered::Duplicate | Delivered::AlreadyLive(_))
            | Err(AnomalySkipped::Seen | AnomalySkipped::Live) => {}
            Err(AnomalySkipped::Capped) => {
                tracing::info!(repo = %repo.slug, "APM error spikes capped for the day");
                break;
            }
        }
    }
    Ok(refused)
}

/// Checks every repo whose schedule is due and this process claims. The
/// jobs refused for a repo's budget are returned.
pub async fn check_due(store: &Store, secrets: &Secrets) -> Result<Vec<Job>, StoreError> {
    let mut refused = Vec::new();
    let mut page = Page::first(REPO_PAGE);
    loop {
        let repos = store.list_repos(page).await?;
        for repo in &repos.items {
            let every = match anomaly_config(repo) {
                Ok(Some(config)) => config.every(),
                Ok(None) => continue,
                Err(e) => {
                    tracing::warn!(repo = %repo.slug, error = %e, "APM spikes not checked");
                    continue;
                }
            };
            if store.claim_poll(&schedule(repo), every).await? {
                refused
                    .extend(check_repo_at(store, secrets, repo, OffsetDateTime::now_utc()).await?);
            }
        }
        match repos.next_page() {
            Some(next) => page = next,
            None => return Ok(refused),
        }
    }
}

/// Checks the schedules every `check` until `shutdown`. Database errors
/// are logged and tried again.
pub async fn run_checker(
    store: Store,
    secrets: Secrets,
    check: Duration,
    shutdown: CancellationToken,
) {
    tracing::info!(check = ?check, "APM spike checker started");
    loop {
        match check_due(&store, &secrets).await {
            Ok(refused) => {
                for job in refused {
                    tracing::warn!(job = job.id.0, reason = ?job.reason, "APM spike not diagnosed");
                }
            }
            Err(e) => tracing::error!(error = %e, "checking APM spikes"),
        }
        tokio::select! {
            () = shutdown.cancelled() => break,
            () = tokio::time::sleep(check) => {}
        }
    }
    tracing::info!("APM spike checker stopped");
}

/// The issue of an `apm` job of `repo`, under its ledger key: the spike
/// as it was found, of kind Anomaly, for the diagnosis lane.
pub async fn apm_issue(job: &Job, repo: &Repo, store: &Store) -> Result<IssueInput, Attempt> {
    let (title, body) = store
        .anomaly(repo.id, &job.issue_ref)
        .await
        .map_err(|e| Attempt::Infra(e.to_string()))?
        .ok_or_else(|| {
            Attempt::Rejected(format!(
                "repo `{}` has no APM spike `{}`",
                repo.slug, job.issue_ref
            ))
        })?;
    Ok(IssueInput {
        cve: None,
        id: job.ledger_key.as_str().to_owned(),
        title,
        body,
        url: String::new(),
        base: None,
        hidden: None,
        kind: Some(IssueKind::Anomaly),
        source: Some("apm".into()),
        memory: String::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_settings_have_defaults_and_are_checked() {
        let config: AnomalyConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(config.every_s, 900);
        assert_eq!(config.window_minutes, 60);
        assert_eq!(config.rules(), SpikeRules::default());
        assert_eq!(config.limits().per_day, 3);
        assert!(config.check().is_ok());
        for bad in [
            r#"{"every_s": 60}"#,
            r#"{"window_minutes": 2}"#,
            r#"{"baseline_days": 0}"#,
            r#"{"max_jobs_per_day": 0}"#,
            r#"{"dedupe_days": 365}"#,
            r#"{"ratio": 0.5}"#,
            r#"{"min_count": 0}"#,
        ] {
            let config: AnomalyConfig = serde_json::from_str(bad).unwrap();
            assert!(config.check().is_err(), "{bad}");
        }
        assert!(serde_json::from_str::<AnomalyConfig>(r#"{"every": 60}"#).is_err());
    }

    #[test]
    fn the_window_reads_as_words() {
        assert_eq!(minutes(60), "the last hour");
        assert_eq!(minutes(180), "the last 3 hours");
        assert_eq!(minutes(45), "the last 45 minutes");
    }
}
