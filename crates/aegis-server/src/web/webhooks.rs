//! What became of a repo's recent webhooks (#331, #373), as rows for the
//! admin's repo page and its connection page alike: the job a delivery
//! made or found, and for one that made none why, with what to do about
//! it. Read from the store's record of outcomes, never a delivery's
//! content.

use aegis_store::{RepoId, Store, WebhookEvent, WebhookOutcome};

use super::WebError;

pub(crate) struct WebhookRow {
    pub received_at: String,
    pub source: &'static str,
    pub delivery_id: String,
    pub event: String,
    pub outcome: &'static str,
    pub good: bool,
    pub reason: String,
    /// The job it made or found, linked.
    pub job_id: Option<i64>,
    /// What to do when it made no job.
    pub hint: Option<&'static str>,
}

/// What to do about a delivery that made no job, from its outcome and the
/// reason the server gave; `None` when there is nothing to do.
fn hint(outcome: WebhookOutcome, reason: &str) -> Option<&'static str> {
    match outcome {
        WebhookOutcome::Queued | WebhookOutcome::Ping => None,
        WebhookOutcome::AlreadyLive => {
            Some("The issue already has a queued or running job: follow that one.")
        }
        WebhookOutcome::Duplicate => {
            Some("The forge sent this delivery again; only the first counts.")
        }
        WebhookOutcome::Refused => Some(
            "The monthly budget of the repo is spent: raise its monthly cap, or wait for next month.",
        ),
        WebhookOutcome::Malformed => Some(
            "The server could not read it: check that the webhook sends JSON (application/json).",
        ),
        WebhookOutcome::BadSignature => {
            Some("Check that the webhook's secret is the value the connection page names.")
        }
        WebhookOutcome::Ignored => ignored_hint(reason),
    }
}

fn ignored_hint(reason: &str) -> Option<&'static str> {
    if reason.starts_with("not the label") {
        Some("Put the label the reason names on an issue; other label changes are ignored.")
    } else if reason.contains("cannot write") {
        Some(
            "Only someone who can write to the repository (owner, member or collaborator) can ask for this.",
        )
    } else if reason.contains("rights could not be checked") || reason.contains("client to check") {
        Some("Check the repo's forge token on its connection page.")
    } else if reason.starts_with("event `") {
        Some(
            "The forge sends an event the server does not use: turn on only the events the connection page lists.",
        )
    } else if reason.contains("another branch") {
        Some("Only CI failures on the default branch make a job.")
    } else if reason == "sent by a bot" {
        Some("Deliveries sent by bots are ignored, so the agent does not answer itself.")
    } else {
        None
    }
}

impl From<WebhookEvent> for WebhookRow {
    fn from(e: WebhookEvent) -> Self {
        let outcome = match e.outcome {
            WebhookOutcome::Queued => "job queued",
            WebhookOutcome::AlreadyLive => "already has a job",
            WebhookOutcome::Refused => "refused",
            WebhookOutcome::Duplicate => "received before",
            WebhookOutcome::Ignored => "ignored",
            WebhookOutcome::Ping => "ping: connected",
            WebhookOutcome::Malformed => "not understood",
            WebhookOutcome::BadSignature => "bad signature",
        };
        let reason = e.reason.unwrap_or_default();
        Self {
            received_at: e.received_at.format("%Y-%m-%d %H:%M:%S").to_string(),
            source: e.source.as_str(),
            delivery_id: e.delivery_id.unwrap_or_default(),
            event: e.event.unwrap_or_default(),
            outcome,
            good: matches!(
                e.outcome,
                WebhookOutcome::Queued
                    | WebhookOutcome::AlreadyLive
                    | WebhookOutcome::Duplicate
                    | WebhookOutcome::Ping
            ),
            hint: hint(e.outcome, &reason),
            reason,
            job_id: e.job_id.map(|j| j.0),
        }
    }
}

/// `repo`'s last `limit` webhooks as rows, the latest first.
pub(crate) async fn rows(
    store: &Store,
    repo: RepoId,
    limit: u32,
) -> Result<Vec<WebhookRow>, WebError> {
    Ok(store
        .repo_webhooks(repo, limit)
        .await?
        .into_iter()
        .map(WebhookRow::from)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_delivery_that_made_no_job_says_what_to_do() {
        use WebhookOutcome::*;
        assert_eq!(hint(Queued, "job 1 for x/y#1"), None);
        assert_eq!(hint(Ping, ""), None);
        for (outcome, reason, says) in [
            (
                Ignored,
                "not the label `aegis` put on an issue",
                "Put the label",
            ),
            (
                Ignored,
                "the sender cannot write to the repository",
                "collaborator",
            ),
            (
                Ignored,
                "the sender's rights could not be checked",
                "forge token",
            ),
            (
                Ignored,
                "no GitHub client to check the sender's rights",
                "forge token",
            ),
            (Ignored, "event `push`", "only the events"),
            (
                Ignored,
                "CI failed on another branch than the default one",
                "default branch",
            ),
            (Ignored, "sent by a bot", "bots"),
            (
                Refused,
                "the repo's monthly budget is spent: …",
                "monthly cap",
            ),
            (Duplicate, "", "only the first counts"),
            (AlreadyLive, "x/y#1 already has job 3", "follow that one"),
            (Malformed, "expected value at line 1", "application/json"),
            (BadSignature, "", "secret"),
        ] {
            let got = hint(outcome, reason).unwrap_or_default();
            assert!(got.contains(says), "{reason}: {got}");
        }
        assert_eq!(hint(Ignored, "a pull request, not an issue"), None);
    }
}
