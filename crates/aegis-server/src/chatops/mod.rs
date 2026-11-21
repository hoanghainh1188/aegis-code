//! ChatOps (#594): Slack and Microsoft Teams told when a job ends, through
//! their incoming webhooks: a PR opened (its link, cost, time and gate), a
//! diagnosis with no PR, a P1 job stopped by a breaker of its run or by
//! the platform, and a repo's or tenant's monthly budget reaching its
//! warning share (`[budget] warn_ratio`, 80% by default) or 100%.
//!
//! Each `[[chatops.channels]]` names the secret that holds its webhook's
//! URL, never the URL itself: the URL is the token. It may be kept to some
//! tenants or repos (by slug) and to some events. Texts are masked and made
//! inert ([`message::clean`]). Each post has a hard 5 s timeout; a failure
//! is logged and counted (`aegis_chat_messages_total`), never the job's.
//! A budget alert is sent once per scope, share and month by each worker
//! process.

pub mod message;
pub mod payload;

use std::collections::HashSet;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use aegis_ledger::{IssueRecord, LedgerStore};
use aegis_secrets::Secrets;
use aegis_store::{Job, Repo, Store, Tenant};
use reqwest::header::CONTENT_TYPE;
use serde::Deserialize;

use crate::metrics::metrics;
use crate::worker::JobNotifier;
use message::{BudgetOf, ChatMessage, JobFacts, budget_message, job_message};

/// The longest a post may take: the notifier runs in the job's slot.
pub const POST_TIMEOUT: Duration = Duration::from_secs(5);

/// Where a channel's messages go.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatKind {
    /// A Slack incoming webhook (Block Kit).
    Slack,
    /// A Teams incoming webhook or Workflows URL (Adaptive Card).
    Teams,
}

impl ChatKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Slack => "slack",
            Self::Teams => "teams",
        }
    }
}

/// What a channel is told of.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatEvent {
    PrOpened,
    Diagnosed,
    P1Stopped,
    Budget,
}

const ALL_EVENTS: [ChatEvent; 4] = [
    ChatEvent::PrOpened,
    ChatEvent::Diagnosed,
    ChatEvent::P1Stopped,
    ChatEvent::Budget,
];

fn all_events() -> Vec<ChatEvent> {
    ALL_EVENTS.to_vec()
}

/// `[chatops]`: no channel, no message.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatopsSection {
    #[serde(default)]
    pub channels: Vec<ChannelSection>,
}

/// One `[[chatops.channels]]`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelSection {
    pub kind: ChatKind,
    /// The secret holding the webhook's URL.
    pub webhook_env: String,
    /// Tenant slugs; empty: every tenant.
    #[serde(default)]
    pub tenants: Vec<String>,
    /// Repo slugs; empty: every repo. A channel kept to repos is not told
    /// of tenants' budgets.
    #[serde(default)]
    pub repos: Vec<String>,
    /// Empty is refused; left out: all of them.
    #[serde(default = "all_events")]
    pub events: Vec<ChatEvent>,
}

/// A name the secret sources take as is: a file name, an environment
/// variable, the end of a Secrets Manager name.
fn is_secret_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

impl ChatopsSection {
    pub fn check(&self) -> Result<(), String> {
        for (i, c) in self.channels.iter().enumerate() {
            let at = format!("chatops.channels[{i}]");
            if !is_secret_name(&c.webhook_env) {
                return Err(format!(
                    "{at}: webhook_env `{}` must be a secret's name: letters, digits, `_`, `-`",
                    c.webhook_env
                ));
            }
            if c.events.is_empty() {
                return Err(format!("{at}: events is empty; leave it out for all"));
            }
            if c.tenants
                .iter()
                .chain(&c.repos)
                .any(|s| s.trim().is_empty())
            {
                return Err(format!("{at}: a tenant or repo slug is empty"));
            }
        }
        Ok(())
    }
}

impl ChannelSection {
    fn wants(&self, event: ChatEvent) -> bool {
        self.events.contains(&event)
    }

    /// Whether a job's message, or a repo's budget, goes here.
    pub fn takes_repo(&self, event: ChatEvent, tenant: &str, repo: &str) -> bool {
        self.wants(event)
            && (self.tenants.is_empty() || self.tenants.iter().any(|t| t == tenant))
            && (self.repos.is_empty() || self.repos.iter().any(|r| r == repo))
    }

    /// Whether a tenant's budget goes here.
    pub fn takes_tenant(&self, tenant: &str) -> bool {
        self.wants(ChatEvent::Budget)
            && self.repos.is_empty()
            && (self.tenants.is_empty() || self.tenants.iter().any(|t| t == tenant))
    }
}

/// Posts messages to webhooks whose URLs are in the secret store.
#[derive(Debug, Clone)]
pub struct ChatSender {
    client: reqwest::Client,
    secrets: Secrets,
    timeout: Duration,
}

/// Only `https`, or `http` to this machine (a relay, the tests): the URL
/// is a token and the message may say what a job did.
fn usable_url(text: &str) -> Result<reqwest::Url, String> {
    let url = reqwest::Url::parse(text.trim()).map_err(|_| "not a URL".to_owned())?;
    let local = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    match url.scheme() {
        "https" => Ok(url),
        "http" if local => Ok(url),
        other => Err(format!("`{other}` URLs are refused; use https")),
    }
}

impl ChatSender {
    pub fn new(secrets: Secrets) -> Self {
        Self {
            client: reqwest::Client::new(),
            secrets,
            timeout: POST_TIMEOUT,
        }
    }

    /// A shorter bound than [`POST_TIMEOUT`], for tests; never a longer one.
    pub fn with_timeout(self, timeout: Duration) -> Self {
        Self {
            timeout: timeout.min(POST_TIMEOUT),
            ..self
        }
    }

    /// Posts `message` to `channel`. The error never holds the URL.
    pub async fn send(
        &self,
        channel: &ChannelSection,
        message: &ChatMessage,
    ) -> Result<(), String> {
        let secret = self
            .secrets
            .get(&channel.webhook_env)
            .ok_or_else(|| format!("no secret {}", channel.webhook_env))?;
        let url = usable_url(secret.expose())
            .map_err(|e| format!("the secret {}: {e}", channel.webhook_env))?;
        let body = serde_json::to_vec(&payload::payload(channel.kind, message))
            .map_err(|e| e.to_string())?;
        let response = self
            .client
            .post(url)
            .header(CONTENT_TYPE, "application/json")
            .timeout(self.timeout)
            .body(body)
            .send()
            .await
            .map_err(|e| {
                let what = if e.is_timeout() {
                    "timed out"
                } else {
                    "failed"
                };
                format!("the post {what}: {}", e.without_url())
            })?;
        let status = response.status();
        if status.is_success() {
            Ok(())
        } else {
            Err(format!("the webhook answered {status}"))
        }
    }

    /// [`Self::send`], logged and counted.
    async fn post(&self, channel: &ChannelSection, message: &ChatMessage) {
        let kind = channel.kind.as_str();
        match self.send(channel, message).await {
            Ok(()) => {
                metrics().chat_message(kind, true);
                tracing::info!(kind, event = ?message.event, "chat message sent");
            }
            Err(e) => {
                metrics().chat_message(kind, false);
                tracing::warn!(kind, event = ?message.event, error = %e, "chat message not sent");
            }
        }
    }
}

/// `(scope, id, percent, month)`: a budget alert already sent.
type Alerted = Arc<Mutex<HashSet<(BudgetOf, i64, u32, String)>>>;

/// The [`JobNotifier`] of `[chatops]`.
#[derive(Debug, Clone)]
pub struct ChatNotifier {
    store: Store,
    sender: ChatSender,
    channels: Arc<Vec<ChannelSection>>,
    alerted: Alerted,
}

impl ChatNotifier {
    /// `None` without channels. Each channel's secret is read now, and
    /// again with the others; a missing one only logs.
    pub async fn from_section(
        section: &ChatopsSection,
        store: Store,
        secrets: &Secrets,
    ) -> Result<Option<Self>, String> {
        if section.channels.is_empty() {
            return Ok(None);
        }
        section.check()?;
        for channel in &section.channels {
            let found = secrets
                .load(&channel.webhook_env)
                .await
                .map_err(|e| e.to_string())?;
            if found.is_none() {
                tracing::warn!(
                    secret = %channel.webhook_env,
                    "no secret for a chat channel: it is told nothing until there is"
                );
            }
        }
        Ok(Some(Self::new(
            store,
            ChatSender::new(secrets.clone()),
            section.channels.clone(),
        )))
    }

    pub fn new(store: Store, sender: ChatSender, channels: Vec<ChannelSection>) -> Self {
        Self {
            store,
            sender,
            channels: Arc::new(channels),
            alerted: Arc::default(),
        }
    }

    async fn place(&self, job: &Job) -> Result<(Repo, Tenant), String> {
        let repo = self
            .store
            .repo(job.repo_id)
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("no repo {}", job.repo_id.0))?;
        let tenant = self
            .store
            .tenant(repo.tenant_id)
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("no tenant {}", repo.tenant_id.0))?;
        Ok((repo, tenant))
    }

    async fn record(&self, job: &Job) -> Result<IssueRecord, String> {
        let events = self
            .store
            .ledger()
            .events(&job.ledger_key)
            .await
            .map_err(|e| e.to_string())?;
        Ok(IssueRecord::from_events(job.ledger_key.clone(), &events))
    }

    async fn tell(&self, job: &Job, diagnosis: Option<&str>) -> Result<(), String> {
        let (repo, tenant) = self.place(job).await?;
        let record = self.record(job).await?;
        let facts = JobFacts {
            repo: &repo.slug,
            cost_usd: record.cost_usd,
            wall_time_s: record.wall_time_s,
            gate_runs: &record.gate_runs,
        };
        if let Some(message) = job_message(job, diagnosis, &facts) {
            for channel in self.channels.iter() {
                if channel.takes_repo(message.event, &tenant.slug, &repo.slug) {
                    self.sender.post(channel, &message).await;
                }
            }
        }
        if record.cost_usd > 0.0 {
            self.budgets(&repo, &tenant, record.cost_usd).await?;
        }
        Ok(())
    }

    /// Alerts for the budgets this job's cost took over a threshold.
    async fn budgets(&self, repo: &Repo, tenant: &Tenant, cost: f64) -> Result<(), String> {
        let warn = self.store.budget_rules().warn_ratio;
        let rb = self
            .store
            .repo_budget(repo.id)
            .await
            .map_err(|e| e.to_string())?;
        if let Some(rb) = rb
            && let Some(cap) = rb.monthly_usd
            && let Some(threshold) = crossed(rb.spent_usd, cost, cap, warn)
            && self.first_alert(BudgetOf::Repo, repo.id.0, threshold)
        {
            let message = budget_message(BudgetOf::Repo, &repo.slug, rb.spent_usd, cap, threshold);
            for channel in self.channels.iter() {
                if channel.takes_repo(ChatEvent::Budget, &tenant.slug, &repo.slug) {
                    self.sender.post(channel, &message).await;
                }
            }
        }
        let tb = self
            .store
            .tenant_budget(tenant.id)
            .await
            .map_err(|e| e.to_string())?;
        if let Some(cap) = tb.monthly_usd
            && let Some(threshold) = crossed(tb.spent_usd, cost, cap, warn)
            && self.first_alert(BudgetOf::Tenant, tenant.id.0, threshold)
        {
            let message =
                budget_message(BudgetOf::Tenant, &tenant.slug, tb.spent_usd, cap, threshold);
            for channel in self.channels.iter() {
                if channel.takes_tenant(&tenant.slug) {
                    self.sender.post(channel, &message).await;
                }
            }
        }
        Ok(())
    }

    /// Whether this alert is the first of its month here.
    fn first_alert(&self, of: BudgetOf, id: i64, threshold: f64) -> bool {
        let month = chrono::Utc::now().format("%Y-%m").to_string();
        let percent = (threshold * 100.0).round() as u32;
        self.alerted
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert((of, id, percent, month))
    }
}

/// The highest share of `cap` (`warn` or 1.0) that spending `cost` took
/// `spent` over: below it before the job, at or above it after.
pub fn crossed(spent: f64, cost: f64, cap: f64, warn: f64) -> Option<f64> {
    if !(cap.is_finite() && cap > 0.0 && spent.is_finite() && cost.is_finite()) {
        return None;
    }
    let before = (spent - cost) / cap;
    let after = spent / cap;
    [1.0, warn].into_iter().find(|&t| before < t && after >= t)
}

impl JobNotifier for ChatNotifier {
    async fn job_ended(&self, job: &Job, diagnosis: Option<&str>) {
        if let Err(e) = self.tell(job, diagnosis).await {
            tracing::warn!(job = job.id.0, error = %e, "chat messages for the job's end");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> ChatopsSection {
        toml::from_str(text).unwrap()
    }

    #[test]
    fn a_channel_takes_every_event_unless_told() {
        let s = parse("[[channels]]\nkind = \"slack\"\nwebhook_env = \"AEGIS_SLACK_WEBHOOK\"");
        assert!(s.check().is_ok());
        assert_eq!(s.channels[0].events, ALL_EVENTS);
        assert_eq!(s.channels[0].kind, ChatKind::Slack);
    }

    #[test]
    fn bad_channels_are_refused() {
        for (text, needle) in [
            ("kind = \"teams\"\nwebhook_env = \"../x\"", "secret's name"),
            ("kind = \"teams\"\nwebhook_env = \"\"", "secret's name"),
            (
                "kind = \"teams\"\nwebhook_env = \"X\"\nevents = []",
                "events is empty",
            ),
            (
                "kind = \"teams\"\nwebhook_env = \"X\"\nrepos = [\" \"]",
                "slug is empty",
            ),
        ] {
            let err = parse(&format!("[[channels]]\n{text}")).check().unwrap_err();
            assert!(err.contains(needle), "{text}: {err}");
        }
        assert!(
            toml::from_str::<ChatopsSection>("[[channels]]\nkind = \"irc\"\nwebhook_env = \"X\"")
                .is_err()
        );
        assert!(
            toml::from_str::<ChatopsSection>(
                "[[channels]]\nkind = \"slack\"\nwebhook_env = \"X\"\nurl = \"https://x\""
            )
            .is_err(),
            "no URL in the file"
        );
    }

    #[test]
    fn channels_are_kept_to_their_tenants_repos_and_events() {
        let c = parse(
            "[[channels]]\nkind = \"slack\"\nwebhook_env = \"X\"\ntenants = [\"acme\"]\n\
             repos = [\"api\"]\nevents = [\"pr_opened\", \"budget\"]",
        )
        .channels
        .remove(0);
        assert!(c.takes_repo(ChatEvent::PrOpened, "acme", "api"));
        assert!(!c.takes_repo(ChatEvent::PrOpened, "acme", "web"));
        assert!(!c.takes_repo(ChatEvent::PrOpened, "globex", "api"));
        assert!(!c.takes_repo(ChatEvent::Diagnosed, "acme", "api"));
        assert!(!c.takes_tenant("acme"), "kept to repos");
        let tenant_wide = ChannelSection { repos: vec![], ..c };
        assert!(tenant_wide.takes_tenant("acme"));
        assert!(!tenant_wide.takes_tenant("globex"));
    }

    #[test]
    fn a_threshold_is_crossed_once_by_the_job_that_takes_it_over() {
        assert_eq!(crossed(81.0, 5.0, 100.0, 0.8), Some(0.8));
        assert_eq!(crossed(85.0, 4.0, 100.0, 0.8), None, "over it already");
        assert_eq!(crossed(70.0, 5.0, 100.0, 0.8), None);
        assert_eq!(crossed(101.0, 30.0, 100.0, 0.8), Some(1.0), "the highest");
        assert_eq!(crossed(100.0, 1.0, 100.0, 0.8), Some(1.0));
        assert_eq!(crossed(5.0, 5.0, 0.0, 0.8), None, "no cap");
    }

    #[test]
    fn webhook_urls_must_be_https_or_local() {
        assert!(usable_url("https://hooks.slack.com/services/T/B/x").is_ok());
        assert!(usable_url("http://127.0.0.1:9/hook").is_ok());
        assert!(usable_url("http://localhost:9/hook").is_ok());
        assert!(usable_url("http://hooks.slack.com/services/T/B/x").is_err());
        assert!(usable_url("file:///etc/passwd").is_err());
        assert!(usable_url("nonsense").is_err());
    }
}
