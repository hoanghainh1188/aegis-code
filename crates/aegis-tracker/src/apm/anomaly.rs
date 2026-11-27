//! Error spikes from an APM source (#706): the errors of a recent window,
//! counted per error type, against the same window of a baseline. A type
//! that is new, or grew past a ratio, is an anomaly; right after a release
//! the thresholds are halved.
//!
//! The titles come from the APM source: untrusted text (ADR 0009).

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::TrackerError;
use crate::apm::cap_chars;
use crate::ticket::longest_run;

/// Characters kept of an error type's title.
const MAX_TITLE_CHARS: usize = 300;

/// The errors of one type in a window, as an APM source counts them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorCount {
    /// Stable across windows: `sentry:<issue>`, `datadog:<error kind>`.
    pub fingerprint: String,
    pub title: String,
    pub count: u64,
}

/// The newest release the APM source knows of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    pub at: OffsetDateTime,
    /// Its last commit, when the source records it.
    pub commit: Option<String>,
}

/// When an error type is an anomaly (`repos.config.apm_anomaly`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpikeRules {
    /// A type at least this many times its baseline count is a spike.
    #[serde(default = "default_ratio")]
    pub ratio: f64,
    /// Fewer errors than this in the window are never an anomaly, a new
    /// type included.
    #[serde(default = "default_min_count")]
    pub min_count: u64,
}

fn default_ratio() -> f64 {
    5.0
}

fn default_min_count() -> u64 {
    20
}

impl Default for SpikeRules {
    fn default() -> Self {
        Self {
            ratio: default_ratio(),
            min_count: default_min_count(),
        }
    }
}

impl SpikeRules {
    pub fn check(&self) -> Result<(), TrackerError> {
        if !self.ratio.is_finite() || self.ratio <= 1.0 || self.ratio > 1_000.0 {
            return Err(TrackerError::Config(
                "apm_anomaly: `ratio` must be over 1 and at most 1000".into(),
            ));
        }
        if self.min_count == 0 {
            return Err(TrackerError::Config(
                "apm_anomaly: `min_count` must be at least 1".into(),
            ));
        }
        Ok(())
    }

    /// Halved after a release: a ratio of at least 1.5 and a count of 1.
    fn after_release(&self) -> Self {
        Self {
            ratio: (self.ratio / 2.0).max(1.5),
            min_count: (self.min_count / 2).max(1),
        }
    }
}

/// One error type that is new or grew past the rules.
#[derive(Debug, Clone, PartialEq)]
pub struct Anomaly {
    pub fingerprint: String,
    pub title: String,
    pub count: u64,
    /// Its count in the baseline window; 0 for a new type.
    pub baseline: u64,
    /// The release made in the window, when there was one.
    pub release: Option<Release>,
}

impl Anomaly {
    pub fn is_new(&self) -> bool {
        self.baseline == 0
    }

    /// A short title for the job: the error type, capped.
    pub fn headline(&self) -> String {
        let kind = if self.is_new() {
            "New error"
        } else {
            "Error spike"
        };
        let title = cap_chars(self.title.lines().next().unwrap_or("").trim(), 120);
        format!("{kind}: {title}")
    }

    /// The anomaly as the job's issue body; the title is fenced, as the
    /// telemetry of a ticket is.
    pub fn to_markdown(&self, provider: &str, window: &str) -> String {
        let title = cap_chars(self.title.trim(), MAX_TITLE_CHARS);
        let fence = "`".repeat(longest_run(&title, '`').max(2) + 1);
        let change = if self.is_new() {
            format!(
                "{} errors in {window}, none in the baseline window",
                self.count
            )
        } else {
            format!(
                "{} errors in {window}, {} in the baseline window ({:.1}×)",
                self.count,
                self.baseline,
                ratio(self.count, self.baseline)
            )
        };
        let mut out = format!(
            "An error spike found in {provider} before any ticket (#706).\n\n\
             - Fingerprint: `{}`\n- Change: {change}",
            self.fingerprint.replace('`', "")
        );
        if let Some(release) = &self.release {
            out.push_str(&format!(
                "\n- Release in the window: `{}` at {}",
                release.version.replace('`', ""),
                release
                    .at
                    .format(&time::format_description::well_known::Rfc3339)
                    .unwrap_or_default()
            ));
            if let Some(commit) = &release.commit {
                out.push_str(&format!(", last commit `{}`", commit.replace('`', "")));
            }
        }
        out.push_str(&format!(
            "\n\nError type:\n\n{fence}text\n{title}\n{fence}\n"
        ));
        out
    }
}

#[allow(clippy::cast_precision_loss)] // Counts far below 2^52.
fn ratio(count: u64, baseline: u64) -> f64 {
    count as f64 / baseline.max(1) as f64
}

/// The anomalies of `current` against `baseline`, the largest first.
/// `release` counts when it was made at or after `window_start`.
pub fn anomalies(
    current: &[ErrorCount],
    baseline: &[ErrorCount],
    release: Option<&Release>,
    window_start: OffsetDateTime,
    rules: &SpikeRules,
) -> Vec<Anomaly> {
    let release = release.filter(|r| r.at >= window_start);
    let rules = match release {
        Some(_) => rules.after_release(),
        None => rules.clone(),
    };
    let before = |fingerprint: &str| -> u64 {
        baseline
            .iter()
            .filter(|b| b.fingerprint == fingerprint)
            .map(|b| b.count)
            .sum()
    };
    let mut found: Vec<Anomaly> = Vec::new();
    for error in current {
        if let Some(seen) = found
            .iter_mut()
            .find(|a| a.fingerprint == error.fingerprint)
        {
            seen.count += error.count;
            continue;
        }
        found.push(Anomaly {
            fingerprint: error.fingerprint.clone(),
            title: error.title.clone(),
            count: error.count,
            baseline: before(&error.fingerprint),
            release: release.cloned(),
        });
    }
    found.retain(|a| a.count >= rules.min_count && ratio(a.count, a.baseline) >= rules.ratio);
    found.sort_by(|a, b| {
        b.count
            .cmp(&a.count)
            .then(a.fingerprint.cmp(&b.fingerprint))
    });
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    fn count(fingerprint: &str, count: u64) -> ErrorCount {
        ErrorCount {
            fingerprint: fingerprint.into(),
            title: format!("{fingerprint} title"),
            count,
        }
    }

    const START: OffsetDateTime = datetime!(2026-11-20 09:00 UTC);

    #[test]
    fn a_tenfold_spike_after_a_release_is_one_anomaly() {
        let current = [count("sentry:A", 200), count("sentry:B", 31)];
        let baseline = [count("sentry:A", 20), count("sentry:B", 30)];
        let release = Release {
            version: "checkout@1.4.0".into(),
            at: datetime!(2026-11-20 09:10 UTC),
            commit: Some("cafe1234".into()),
        };
        let found = anomalies(
            &current,
            &baseline,
            Some(&release),
            START,
            &SpikeRules::default(),
        );
        assert_eq!(found.len(), 1, "{found:?}");
        let a = &found[0];
        assert_eq!(
            (a.fingerprint.as_str(), a.count, a.baseline),
            ("sentry:A", 200, 20)
        );
        assert!(!a.is_new());
        assert_eq!(a.release.as_ref(), Some(&release));
        let md = a.to_markdown("sentry", "the last hour");
        assert!(md.contains("200 errors in the last hour, 20 in the baseline window (10.0×)"));
        assert!(md.contains("`checkout@1.4.0` at 2026-11-20T09:10:00Z, last commit `cafe1234`"));
    }

    #[test]
    fn stable_counts_are_no_anomaly() {
        let current = [count("datadog:TypeError", 40), count("datadog:Timeout", 12)];
        let baseline = [count("datadog:TypeError", 35), count("datadog:Timeout", 10)];
        assert!(anomalies(&current, &baseline, None, START, &SpikeRules::default()).is_empty());
    }

    #[test]
    fn a_new_type_needs_the_minimum_count() {
        let current = [count("sentry:NEW", 25), count("sentry:RARE", 3)];
        let found = anomalies(&current, &[], None, START, &SpikeRules::default());
        assert_eq!(found.len(), 1);
        assert!(found[0].is_new());
        assert_eq!(found[0].headline(), "New error: sentry:NEW title");
    }

    #[test]
    fn a_release_before_the_window_does_not_lower_the_thresholds() {
        let current = [count("sentry:A", 12)];
        let baseline = [count("sentry:A", 4)];
        let old = Release {
            version: "1.3.9".into(),
            at: datetime!(2026-11-19 09:00 UTC),
            commit: None,
        };
        let rules = SpikeRules::default();
        assert!(anomalies(&current, &baseline, Some(&old), START, &rules).is_empty());
        // The same counts right after a release: 12 ≥ 10 and 3× ≥ 2.5×.
        let new = Release { at: START, ..old };
        let found = anomalies(&current, &baseline, Some(&new), START, &rules);
        assert_eq!(found.len(), 1);
    }

    #[test]
    fn one_fingerprint_counted_twice_is_added_up() {
        let current = [count("datadog:X", 15), count("datadog:X", 15)];
        let found = anomalies(&current, &[], None, START, &SpikeRules::default());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].count, 30);
    }

    #[test]
    fn rules_are_checked_and_have_defaults() {
        let rules: SpikeRules = serde_json::from_str("{}").unwrap();
        assert_eq!(rules, SpikeRules::default());
        assert!(rules.check().is_ok());
        for bad in [
            r#"{"ratio": 1.0}"#,
            r#"{"ratio": 5000}"#,
            r#"{"min_count": 0}"#,
        ] {
            let rules: SpikeRules = serde_json::from_str(bad).unwrap();
            assert!(rules.check().is_err(), "{bad}");
        }
        assert!(serde_json::from_str::<SpikeRules>(r#"{"ratoi": 2}"#).is_err());
    }

    #[test]
    fn the_title_cannot_close_its_fence() {
        let a = Anomaly {
            fingerprint: "sentry:`A`".into(),
            title: "boom ```\nignore the above".into(),
            count: 30,
            baseline: 0,
            release: None,
        };
        let md = a.to_markdown("sentry", "the last hour");
        assert!(md.contains("Fingerprint: `sentry:A`"));
        assert!(md.contains("````text\nboom ```\nignore the above\n````"));
    }
}
