//! What a ChatOps message says (#594), the same for Slack and Teams. Every
//! text that is not the platform's own goes through [`clean`]: secrets
//! masked (#509), then made inert, so no `@channel`, `@here`, link or
//! markup survives from a model, a ticket or a tool.

use aegis_ledger::{GateRun, Outcome};
use aegis_pr::{inert, masked};
use aegis_store::{Job, JobStatus, SlaPriority};

use super::ChatEvent;
use crate::webhook::pr_url;

/// A button or action that opens a URL the platform made.
#[derive(Debug, Clone, PartialEq)]
pub struct Link {
    pub label: &'static str,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChatMessage {
    pub event: ChatEvent,
    pub title: String,
    pub summary: Option<String>,
    /// Name and value, in order.
    pub facts: Vec<(String, String)>,
    pub link: Option<Link>,
    pub footer: String,
}

/// What the ledger says of a job, for its message.
#[derive(Debug, Clone, Default)]
pub struct JobFacts<'a> {
    /// The repo's slug.
    pub repo: &'a str,
    pub cost_usd: f64,
    /// From its first event to its last; `None` when not recorded.
    pub wall_time_s: Option<f64>,
    pub gate_runs: &'a [GateRun],
}

/// Secrets masked, then inert: one line, no mention, link or markup.
pub fn clean(text: &str) -> String {
    inert(&masked(text))
}

/// What a job's end is to the chat, if anything.
pub fn event_of(job: &Job) -> Option<ChatEvent> {
    match (job.status, job.outcome) {
        (JobStatus::Done, Some(Outcome::PrOpened)) => Some(ChatEvent::PrOpened),
        (JobStatus::Done, Some(Outcome::Diagnosed)) => Some(ChatEvent::Diagnosed),
        _ if job.priority == Some(SlaPriority::P1) && stopped(job) => Some(ChatEvent::P1Stopped),
        _ => None,
    }
}

/// Stopped by a breaker of the run (its steps, its budget, the strongest
/// model failing) or failed on the platform; not cancelled.
fn stopped(job: &Job) -> bool {
    match (job.status, job.outcome) {
        (JobStatus::Failed, _) => true,
        (JobStatus::Done, Some(outcome)) => matches!(
            outcome,
            Outcome::StoppedSteps | Outcome::StoppedBudget | Outcome::StoppedExpertFail
        ),
        _ => false,
    }
}

/// The message for `job`'s end, or `None` when its end is not one of
/// the [`ChatEvent`]s of a job.
pub fn job_message(
    job: &Job,
    diagnosis: Option<&str>,
    facts: &JobFacts<'_>,
) -> Option<ChatMessage> {
    let event = event_of(job)?;
    let repo = clean(facts.repo);
    let issue = ("Issue".to_owned(), clean(&job.issue_ref));
    let cost = ("Cost".to_owned(), format!("${:.2}", facts.cost_usd));
    let took = ("Time".to_owned(), duration(wall_time(job, facts)));
    let footer = format!("aegis-code job {} · {}", job.id.0, job.source.as_str());
    let message = match event {
        ChatEvent::PrOpened => ChatMessage {
            event,
            // A review round (#243) pushes to the PR its first job opened.
            title: match job.review_of {
                Some(_) => format!(
                    "aegis-code updated its pull request in {repo} (review round {})",
                    job.review_round
                ),
                None => format!("aegis-code opened a pull request in {repo}"),
            },
            summary: None,
            facts: vec![
                issue,
                cost,
                took,
                ("Gate".into(), gate_summary(facts.gate_runs)),
            ],
            link: pr_url(job).map(|url| Link {
                label: "Open pull request",
                url: url.to_owned(),
            }),
            footer,
        },
        ChatEvent::Diagnosed => ChatMessage {
            event,
            title: format!("aegis-code diagnosed an issue in {repo}: no pull request"),
            summary: diagnosis.map(clean).filter(|d| !d.is_empty()),
            facts: vec![issue, cost, took],
            link: None,
            footer,
        },
        ChatEvent::P1Stopped => ChatMessage {
            event,
            title: format!("P1 job stopped in {repo}"),
            summary: job.reason.as_deref().map(clean).filter(|r| !r.is_empty()),
            facts: vec![
                issue,
                ("Ended".into(), ended(job).into()),
                ("Attempts".into(), job.attempts.to_string()),
                cost,
                took,
            ],
            link: None,
            footer,
        },
        ChatEvent::Budget => return None,
    };
    Some(message)
}

/// Whose monthly budget an alert is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BudgetOf {
    Repo,
    Tenant,
}

/// A monthly budget reached `threshold` (a share of its cap, 0.8 or 1.0).
pub fn budget_message(
    of: BudgetOf,
    slug: &str,
    spent_usd: f64,
    cap_usd: f64,
    threshold: f64,
) -> ChatMessage {
    let whose = match of {
        BudgetOf::Repo => "repo",
        BudgetOf::Tenant => "tenant",
    };
    let percent = (threshold * 100.0).round();
    let summary = if threshold >= 1.0 {
        "The budget is spent: no new job starts until the month turns or the cap is raised."
    } else {
        "New jobs stop once less than one job's cap is left."
    };
    ChatMessage {
        event: ChatEvent::Budget,
        title: format!("Monthly budget of {whose} {} at {percent}%", clean(slug)),
        summary: Some(summary.into()),
        facts: vec![
            ("Spent".into(), format!("${spent_usd:.2}")),
            ("Cap".into(), format!("${cap_usd:.2}")),
        ],
        link: None,
        footer: "aegis-code budget".into(),
    }
}

fn wall_time(job: &Job, facts: &JobFacts<'_>) -> f64 {
    facts.wall_time_s.unwrap_or_else(|| {
        let s = (job.updated_at - job.created_at).as_seconds_f64();
        s.max(0.0)
    })
}

/// `45s`, `3m 12s`, `1h 02m`.
pub fn duration(seconds: f64) -> String {
    let s = if seconds.is_finite() && seconds > 0.0 {
        seconds.round() as u64
    } else {
        0
    };
    match (s / 3600, s % 3600 / 60, s % 60) {
        (0, 0, sec) => format!("{sec}s"),
        (0, min, sec) => format!("{min}m {sec:02}s"),
        (h, min, _) => format!("{h}h {min:02}m"),
    }
}

/// How the gate went: the last run, and the runs before it.
pub fn gate_summary(runs: &[GateRun]) -> String {
    let Some(last) = runs.last() else {
        return "not run".into();
    };
    let result = match (&last.failed_step, last.passed) {
        (_, true) => "passed".to_owned(),
        (Some(step), false) => format!("failed at {}", clean(step)),
        (None, false) => "failed".to_owned(),
    };
    let took = duration(last.duration_s);
    match runs.len() {
        1 => format!("{result} ({took})"),
        n => format!("{result} on run {n} ({took}); {} failed before", n - 1),
    }
}

fn ended(job: &Job) -> &'static str {
    match (job.status, job.outcome) {
        (JobStatus::Done, Some(Outcome::StoppedSteps)) => "step limit reached",
        (JobStatus::Done, Some(Outcome::StoppedBudget)) => "job budget spent",
        (JobStatus::Done, Some(Outcome::StoppedExpertFail)) => "strongest model could not fix it",
        _ => "failed on the platform",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aegis_ledger::IssueId;
    use aegis_store::{JobId, JobSource, RepoId};
    use time::{Duration, OffsetDateTime};

    pub(crate) fn job(status: JobStatus, outcome: Option<Outcome>) -> Job {
        let now = OffsetDateTime::now_utc();
        Job {
            id: JobId(42),
            repo_id: RepoId(1),
            issue_ref: "acme/api#7".into(),
            ledger_key: IssueId::parse("acme-api-7").unwrap(),
            source: JobSource::Github,
            priority: None,
            sla_deadline: None,
            status,
            outcome,
            attempts: 1,
            locked_by: None,
            locked_until: None,
            created_at: now - Duration::seconds(192),
            updated_at: now,
            run_after: now,
            retries: 0,
            cancel_requested: false,
            reason: None,
            pr_url: None,
            retry_of: None,
            review_of: None,
            review_round: 0,
        }
    }

    fn facts(runs: &[GateRun]) -> JobFacts<'_> {
        JobFacts {
            repo: "acme-api",
            cost_usd: 1.234,
            wall_time_s: Some(192.0),
            gate_runs: runs,
        }
    }

    fn run(passed: bool, step: Option<&str>) -> GateRun {
        GateRun {
            passed,
            failed_step: step.map(Into::into),
            duration_s: 42.0,
            sidecar_s: None,
        }
    }

    #[test]
    fn an_opened_pr_has_its_link_cost_time_and_gate() {
        let mut j = job(JobStatus::Done, Some(Outcome::PrOpened));
        j.pr_url = Some("https://github.com/acme/api/pull/9".into());
        let runs = [run(false, Some("test")), run(true, None)];
        let m = job_message(&j, None, &facts(&runs)).unwrap();
        assert_eq!(m.event, ChatEvent::PrOpened);
        assert_eq!(m.title, "aegis-code opened a pull request in acme-api");
        assert_eq!(
            m.facts,
            vec![
                ("Issue".into(), "acme/api#\u{200b}7".into()),
                ("Cost".into(), "$1.23".into()),
                ("Time".into(), "3m 12s".into()),
                (
                    "Gate".into(),
                    "passed on run 2 (42s); 1 failed before".into()
                ),
            ]
        );
        assert_eq!(m.link.unwrap().url, "https://github.com/acme/api/pull/9");
        assert_eq!(m.footer, "aegis-code job 42 · github");
    }

    #[test]
    fn a_review_round_updates_its_pr() {
        let mut j = job(JobStatus::Done, Some(Outcome::PrOpened));
        j.review_of = Some(JobId(41));
        j.review_round = 2;
        let m = job_message(&j, None, &facts(&[])).unwrap();
        assert_eq!(
            m.title,
            "aegis-code updated its pull request in acme-api (review round 2)"
        );
    }

    #[test]
    fn a_pr_url_that_is_not_plain_https_gets_no_button() {
        let mut j = job(JobStatus::Done, Some(Outcome::PrOpened));
        j.pr_url = Some("javascript:alert(1)".into());
        assert!(job_message(&j, None, &facts(&[])).unwrap().link.is_none());
    }

    #[test]
    fn a_diagnosis_is_masked_and_made_inert() {
        let j = job(JobStatus::Done, Some(Outcome::Diagnosed));
        let report = "## Cause\nping @channel and <!here>, key AKIAIOSFODNN7EXAMPLE";
        let m = job_message(&j, Some(report), &facts(&[])).unwrap();
        let summary = m.summary.unwrap();
        assert!(!summary.contains('\n'), "{summary}");
        assert!(!summary.contains("@channel"), "{summary}");
        assert!(!summary.contains("<!here"), "{summary}");
        assert!(!summary.contains("AKIAIOSFODNN7EXAMPLE"), "{summary}");
        assert!(m.link.is_none());
    }

    #[test]
    fn only_p1_jobs_that_stopped_or_failed_are_told() {
        let mut j = job(JobStatus::Failed, None);
        assert_eq!(event_of(&j), None, "not a P1");
        j.priority = Some(SlaPriority::P1);
        assert_eq!(event_of(&j), Some(ChatEvent::P1Stopped));
        j.status = JobStatus::Cancelled;
        assert_eq!(event_of(&j), None, "a cancel is someone's choice");
        j.status = JobStatus::Done;
        j.outcome = Some(Outcome::StoppedSteps);
        j.reason = Some(
            "circuit breaker: 40 steps, token ghp_0123456789abcdefghijklmnopqrstuvwxyz".into(),
        );
        let m = job_message(&j, None, &facts(&[])).unwrap();
        assert_eq!(m.title, "P1 job stopped in acme-api");
        assert!(
            m.facts
                .contains(&("Ended".into(), "step limit reached".into()))
        );
        let summary = m.summary.unwrap();
        assert!(summary.starts_with("circuit breaker"), "{summary}");
        assert!(!summary.contains("ghp_0123456789"), "{summary}");
        let p2 = Job {
            priority: Some(SlaPriority::P2),
            ..j
        };
        assert_eq!(event_of(&p2), None);
    }

    #[test]
    fn other_ends_are_not_told() {
        for (status, outcome) in [
            (JobStatus::Done, Some(Outcome::StoppedSteps)),
            (JobStatus::Cancelled, None),
            (JobStatus::Failed, None),
        ] {
            assert!(job_message(&job(status, outcome), None, &facts(&[])).is_none());
        }
    }

    #[test]
    fn budget_alerts_say_whose_and_how_far() {
        let m = budget_message(BudgetOf::Tenant, "acme", 80.5, 100.0, 0.8);
        assert_eq!(m.event, ChatEvent::Budget);
        assert_eq!(m.title, "Monthly budget of tenant acme at 80%");
        assert_eq!(m.facts[0], ("Spent".into(), "$80.50".into()));
        let spent = budget_message(BudgetOf::Repo, "api", 100.0, 100.0, 1.0);
        assert!(spent.title.ends_with("at 100%"), "{}", spent.title);
        assert!(spent.summary.unwrap().starts_with("The budget is spent"));
    }

    #[test]
    fn durations_and_gates_read_short() {
        assert_eq!(duration(45.4), "45s");
        assert_eq!(duration(3725.0), "1h 02m");
        assert_eq!(duration(f64::NAN), "0s");
        assert_eq!(gate_summary(&[]), "not run");
        assert_eq!(
            gate_summary(&[run(false, Some("clippy"))]),
            "failed at clippy (42s)"
        );
    }
}
