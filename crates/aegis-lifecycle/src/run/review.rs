//! A review round on the agent's own PR (#244): the PR's head fetched and
//! worked on.

use super::*;

/// A review round: the PR's branch fetched into the clone, the review as
/// the first message, no triage (it is the issue's).
pub(super) async fn review_and_work<P, S>(
    platform: &Platform<P, S>,
    ledger: &mut IssueLedger<'_, S>,
    issue: &IssueInput,
    round: ReviewRound,
    cancel: &CancellationToken,
) -> Result<Ending, LifecycleError>
where
    P: ModelProvider + Send + Sync + 'static,
    S: LedgerStore + Send + Sync + 'static,
{
    let cfg = &platform.config;
    if cancel.is_cancelled() {
        return Ok(Ending::stop(Outcome::StoppedSteps, "cancelled"));
    }
    let (title, body) = (redact(&issue.title), redact(&issue.body));
    let (round, in_notes) = round.redacted();
    ledger
        .secrets_redacted(count(title.count() + body.count() + in_notes))
        .await?;
    ledger.issue_read(&title.text, &issue.url).await?;
    ledger.ticket_meta(ticket_meta(cfg, issue)).await?;
    let issue = &IssueInput {
        title: title.text,
        body: body.text,
        ..issue.clone()
    };
    ledger.classified(IssueKind::Bug).await?;
    let head = match round.local_base {
        // A local round (#439): its head is in the clone already.
        Some(_) => round.pull.head_sha.clone(),
        None => fetch_head(platform, &round).await?,
    };
    // The round from the head just fetched, so its base is where that head
    // meets the branch.
    let round = ReviewRound {
        pull: aegis_pr::PullState {
            head_sha: head,
            ..round.pull
        },
        ..round
    };
    let base = round.base(&cfg.repo.path, &cfg.repo.base)?;
    let diff = job::diff(&cfg.repo.path, &base, &round.pull.head_sha, MAX_DIFF_CHARS)?;
    let diff = redact(&diff);
    ledger.secrets_redacted(count(diff.count())).await?;
    let first = round.first_message(issue, &diff.text);
    let profiles = cfg.repo.profiles(&platform.profiles)?;
    work(
        platform,
        ledger,
        &profiles,
        issue,
        first,
        &Finish::Update(Box::new(round)),
        cancel,
    )
    .await
}

/// The PR's branch as it is on the fork now, reviewers' commits
/// included, fetched into the clone: its head commit.
pub(super) async fn fetch_head<P, S>(
    platform: &Platform<P, S>,
    round: &ReviewRound,
) -> Result<String, LifecycleError> {
    let cfg = &platform.config;
    let remote = fork_remote(platform).await?;
    let (clone, branch) = (cfg.repo.path.clone(), round.pull.head_ref.clone());
    let url = remote.url().to_owned();
    let auth = remote
        .credentials()
        .filter(|(_, t)| !t.is_empty())
        .map(|(u, t)| (u, t.to_owned()));
    let into = format!("review/{}", round.branch_id());
    tokio::task::spawn_blocking(move || {
        let source = crate::clone::Source {
            url: &url,
            token: auth.as_ref().map(|(_, t)| t.as_str()),
            user: auth.as_ref().map_or(aegis_pr::GITHUB_USER, |(u, _)| u),
        };
        crate::clone::fetch_ref(&clone, source, &branch, &into)
    })
    .await
    .map_err(|e| LifecycleError::Config(format!("fetching the PR's branch: {e}")))?
}
