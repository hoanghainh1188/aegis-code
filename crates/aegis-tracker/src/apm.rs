//! Read-only APM telemetry enrichment (#452, ADR 0011): pulls stack
//! traces, release commit SHAs, error frequency, and recent breadcrumbs
//! from Sentry, Datadog or AWS CloudWatch Logs (#704) when a ticket lacks
//! full production logs.
//!
//! Telemetry text is untrusted input (ADR 0009): every fetched block is
//! size-capped and wrapped in a fence it cannot close before reaching `BP3`.

mod cloudwatch;
mod datadog;
mod sentry;

use serde::{Deserialize, Serialize};

use crate::TrackerError;
use crate::config::{site_url, token_env};
use crate::ticket::longest_run;

use cloudwatch::CloudWatch;
use datadog::Datadog;
use sentry::Sentry;

/// Maximum characters kept for a stack trace.
const MAX_STACKTRACE_CHARS: usize = 4_000;
/// Maximum breadcrumbs kept per telemetry snapshot.
const MAX_BREADCRUMBS: usize = 10;
/// Maximum characters per breadcrumb line.
const MAX_BREADCRUMB_CHARS: usize = 240;
/// Maximum total characters emitted by [`Telemetry::to_markdown`].
const MAX_TELEMETRY_CHARS: usize = 6_000;

/// Per-repository APM configuration (`repos.config.apm`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ApmConfig {
    Sentry(SentryConfig),
    Datadog(DatadogConfig),
    #[serde(rename = "cloudwatch")]
    CloudWatch(CloudWatchConfig),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SentryConfig {
    /// Sentry API root, e.g. `https://sentry.io`.
    pub url: String,
    /// Organization slug.
    pub organization: String,
    /// Optional project slug to narrow searches.
    #[serde(default)]
    pub project: Option<String>,
    /// Secret holding the Sentry bearer auth token.
    pub token_env: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatadogConfig {
    /// Datadog API root, e.g. `https://api.datadoghq.com`.
    pub url: String,
    /// Secret holding `DD-API-KEY`.
    pub token_env: String,
    /// Secret holding `DD-APPLICATION-KEY`.
    pub app_key_env: String,
    /// Optional service filter (`service:<name>`).
    #[serde(default)]
    pub service: Option<String>,
    /// Optional environment filter (`env:<name>`).
    #[serde(default)]
    pub env: Option<String>,
}

/// AWS CloudWatch Logs (#704): Logs Insights over `log_groups`, with the
/// standard AWS credential chain. No secret is named here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloudWatchConfig {
    /// The log groups' region, e.g. `ap-northeast-1`.
    pub region: String,
    /// Log groups to query (1 to 50), e.g. `/ecs/checkout`.
    pub log_groups: Vec<String>,
    /// A named profile from the AWS config files; the default chain (the
    /// machine's role) when absent.
    #[serde(default)]
    pub profile: Option<String>,
    /// How far before the ticket's time to read; the window also runs an
    /// hour past it.
    #[serde(default = "default_lookback_minutes")]
    pub lookback_minutes: u32,
}

/// Logs Insights accepts at most 50 log groups per query.
const MAX_LOG_GROUPS: usize = 50;
/// Two weeks: Logs Insights bills by the data it scans.
const MAX_LOOKBACK_MINUTES: u32 = 14 * 24 * 60;

fn default_lookback_minutes() -> u32 {
    24 * 60
}

impl CloudWatchConfig {
    fn check(&self) -> Result<(), TrackerError> {
        let bad = |what: &str| Err(TrackerError::Config(format!("cloudwatch: {what}")));
        let region = self.region.trim();
        if region.is_empty()
            || !region
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return bad("`region` is not a region name");
        }
        if self.log_groups.is_empty() || self.log_groups.len() > MAX_LOG_GROUPS {
            return bad("`log_groups` needs 1 to 50 log groups");
        }
        if self.log_groups.iter().any(|g| g.trim().is_empty()) {
            return bad("`log_groups` has an empty name");
        }
        if self.profile.as_deref().is_some_and(|p| p.trim().is_empty()) {
            return bad("`profile` is empty");
        }
        if self.lookback_minutes == 0 || self.lookback_minutes > MAX_LOOKBACK_MINUTES {
            return bad("`lookback_minutes` must be 1 to 20160 (two weeks)");
        }
        Ok(())
    }
}

impl ApmConfig {
    /// The secret holding the provider's token; `None` for CloudWatch, which
    /// takes the standard AWS credential chain.
    pub fn token_env(&self) -> Option<&str> {
        match self {
            Self::Sentry(c) => Some(&c.token_env),
            Self::Datadog(c) => Some(&c.token_env),
            Self::CloudWatch(_) => None,
        }
    }

    pub fn app_key_env(&self) -> Option<&str> {
        match self {
            Self::Sentry(_) | Self::CloudWatch(_) => None,
            Self::Datadog(c) => Some(&c.app_key_env),
        }
    }

    pub fn check(&self) -> Result<(), TrackerError> {
        match self {
            Self::Sentry(c) => {
                site_url(&c.url)?;
                token_env(&c.token_env)?;
                if c.organization.trim().is_empty() {
                    return Err(TrackerError::Config(
                        "sentry: `organization` is empty".into(),
                    ));
                }
            }
            Self::Datadog(c) => {
                site_url(&c.url)?;
                token_env(&c.token_env)?;
                token_env(&c.app_key_env)?;
            }
            Self::CloudWatch(c) => c.check()?,
        }
        Ok(())
    }
}

/// Identifiers and search terms extracted from a ticket/issue.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TelemetryQuery {
    /// Numeric Sentry issue ID found in a URL or `SENTRY-<id>`.
    pub sentry_issue_id: Option<String>,
    /// Correlation, trace, or request ID from the ticket text.
    pub correlation_id: Option<String>,
    /// Error code or exception symbol (e.g. `ERR_TIMEOUT`, `NullPointerException`).
    pub error_code: Option<String>,
    /// Fallback search text (typically the ticket title).
    pub fallback_text: String,
    /// When the ticket arrived: CloudWatch reads the logs around it. `None`
    /// is now.
    pub around: Option<time::OffsetDateTime>,
}

impl TelemetryQuery {
    /// Extracts APM lookup hints from a ticket's title and body.
    pub fn from_ticket(title: &str, body: &str) -> Self {
        let combined = format!("{title}\n{body}");
        Self {
            sentry_issue_id: find_sentry_issue_id(&combined),
            correlation_id: find_tagged_id(
                &combined,
                &["correlation_id", "trace_id", "request_id"],
            ),
            error_code: find_error_code(&combined),
            fallback_text: title.trim().chars().take(120).collect(),
            around: None,
        }
    }

    /// Best search string for APM APIs when no direct issue ID is known.
    pub(crate) fn search_term(&self) -> &str {
        self.correlation_id
            .as_deref()
            .or(self.error_code.as_deref())
            .unwrap_or(&self.fallback_text)
    }
}

/// Sanitized, size-capped production telemetry returned by an APM provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Telemetry {
    pub provider: &'static str,
    pub summary: String,
    pub release_commit: Option<String>,
    pub frequency: Option<String>,
    pub stacktrace: Option<String>,
    pub breadcrumbs: Vec<String>,
    pub url: Option<String>,
}

impl Telemetry {
    /// Formats telemetry as Markdown with a fence the untrusted payload cannot close.
    pub fn to_markdown(&self) -> String {
        let mut out = format!("\n\n## APM telemetry ({})\n", self.provider);
        out.push_str(&format!("\n- Summary: {}", self.summary.trim()));
        if let Some(freq) = &self.frequency {
            out.push_str(&format!("\n- Frequency: {}", freq.trim()));
        }
        if let Some(release) = &self.release_commit {
            out.push_str(&format!("\n- Release/Commit: `{}`", release.trim()));
        }
        if let Some(url) = &self.url {
            out.push_str(&format!("\n- Reference: {url}"));
        }
        if let Some(trace) = &self.stacktrace {
            let capped = cap_chars(trace.trim(), MAX_STACKTRACE_CHARS);
            let fence = "`".repeat(longest_run(&capped, '`').max(2) + 1);
            out.push_str(&format!(
                "\n\nStack trace:\n\n{fence}text\n{capped}\n{fence}"
            ));
        }
        if !self.breadcrumbs.is_empty() {
            let lines: Vec<String> = self
                .breadcrumbs
                .iter()
                .take(MAX_BREADCRUMBS)
                .map(|b| cap_chars(b.trim(), MAX_BREADCRUMB_CHARS))
                .collect();
            let joined = lines.join("\n");
            let fence = "`".repeat(longest_run(&joined, '`').max(2) + 1);
            out.push_str(&format!(
                "\n\nRecent breadcrumbs:\n\n{fence}text\n{joined}\n{fence}"
            ));
        }
        out.push('\n');
        cap_chars(&out, MAX_TELEMETRY_CHARS)
    }
}

/// Connected APM client.
#[derive(Debug, Clone)]
pub enum ApmClient {
    Sentry(Sentry),
    Datadog(Datadog),
    CloudWatch(CloudWatch),
}

impl ApmClient {
    /// `token` is the secret named by [`ApmConfig::token_env`], `app_key`
    /// the one named by [`ApmConfig::app_key_env`]; CloudWatch takes neither.
    pub fn new(
        config: &ApmConfig,
        token: Option<&str>,
        app_key: Option<&str>,
    ) -> Result<Self, TrackerError> {
        config.check()?;
        let token = || {
            token
                .filter(|t| !t.is_empty())
                .ok_or_else(|| TrackerError::Config("apm: missing the `token_env` secret".into()))
        };
        Ok(match config {
            ApmConfig::Sentry(c) => Self::Sentry(Sentry::new(c, token()?)?),
            ApmConfig::CloudWatch(c) => Self::CloudWatch(CloudWatch::new(c)),
            ApmConfig::Datadog(c) => {
                let app_key = app_key.filter(|k| !k.is_empty()).ok_or_else(|| {
                    TrackerError::Config("datadog: missing `app_key_env` secret".into())
                })?;
                Self::Datadog(Datadog::new(c, token()?, app_key)?)
            }
        })
    }

    /// Fetches matching production telemetry, returning `None` when no matching
    /// issue or log event is found.
    pub async fn fetch(&self, query: &TelemetryQuery) -> Result<Option<Telemetry>, TrackerError> {
        match self {
            Self::Sentry(s) => s.fetch(query).await,
            Self::Datadog(d) => d.fetch(query).await,
            Self::CloudWatch(c) => c.fetch(query).await,
        }
    }
}

pub(crate) fn cap_chars(text: &str, max: usize) -> String {
    let mut chars = text.chars();
    let head: String = chars.by_ref().take(max).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

fn find_sentry_issue_id(text: &str) -> Option<String> {
    // Match `SENTRY-<digits>` or `.../issues/<digits>`
    for token in text.split_whitespace() {
        let trimmed =
            token.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '/' && c != '-');
        if let Some(rest) = trimmed.strip_prefix("SENTRY-")
            && !rest.is_empty()
            && rest.bytes().all(|b| b.is_ascii_digit())
        {
            return Some(rest.to_owned());
        }
        if let Some((_, after)) = trimmed.split_once("/issues/") {
            let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
            if digits.len() >= 4 {
                return Some(digits);
            }
        }
    }
    None
}

fn find_tagged_id(text: &str, keys: &[&str]) -> Option<String> {
    for line in text.lines() {
        let lower = line.to_ascii_lowercase();
        for key in keys {
            if let Some(pos) = lower.find(key) {
                let rest = line[pos + key.len()..].trim_start_matches([' ', ':', '=', '"', '\'']);
                let val: String = rest
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
                    .collect();
                if val.len() >= 6 {
                    return Some(val);
                }
            }
        }
    }
    None
}

fn find_error_code(text: &str) -> Option<String> {
    for token in text.split_whitespace() {
        let clean = token.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '_');
        if clean.starts_with("ERR_") && clean.len() >= 6 {
            return Some(clean.to_owned());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_sentry_and_correlation_ids() {
        let q = TelemetryQuery::from_ticket(
            "Checkout 500",
            "See https://sentry.io/organizations/acme/issues/123456/events/latest/\n\
             trace_id: req-998877\n\
             code: ERR_CART_EMPTY",
        );
        assert_eq!(q.sentry_issue_id.as_deref(), Some("123456"));
        assert_eq!(q.correlation_id.as_deref(), Some("req-998877"));
        assert_eq!(q.error_code.as_deref(), Some("ERR_CART_EMPTY"));
    }

    #[test]
    fn telemetry_markdown_caps_size_and_fences_backticks() {
        let t = Telemetry {
            provider: "sentry",
            summary: "RuntimeError: boom".into(),
            release_commit: Some("abc1234".into()),
            frequency: Some("42 events, 12 users".into()),
            stacktrace: Some(format!("```\n{}\n```", "x".repeat(5_000))),
            breadcrumbs: vec!["[http] GET /checkout -> 500".into()],
            url: Some("https://sentry.io/issues/123456/".into()),
        };
        let md = t.to_markdown();
        assert!(md.contains("## APM telemetry (sentry)"));
        assert!(md.contains("````text\n"));
        assert!(md.chars().count() <= MAX_TELEMETRY_CHARS + 1);
    }
}
