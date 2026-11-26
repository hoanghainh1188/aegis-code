//! Turns a gated patch into a pull request (design §2 steps 2, 7, 10;
//! §6.4; §7): the commit holds exactly the tree the gate checked, on top
//! of the base it was compared with, on the branch `agent/<issue-id>`,
//! and the PR is opened in the agent's own fork.
//!
//! The token should only be allowed to push `agent/*` and open PRs (a
//! ruleset on the fork protects every other branch); this crate also never
//! pushes any other ref, and pushes only to the repo the PR is opened in
//! ([`GitHub::remote`]).
//!
//! The PR's base branch must still point at the gate's base revision when
//! the PR is opened; the lifecycle (#16) checks that before calling.

mod access;
mod actions;
mod app;
mod azure;
mod bitbucket;
mod bitbucket_dc;
mod body;
mod branch;
mod commit_message;
mod coverage;
mod error;
mod forge;
mod git;
mod github;
mod gitlab;
mod hooks;
mod locale;
mod review;

use std::path::Path;

use aegis_gate::GateReport;

pub use access::RepoAccess;
pub use actions::{FailedJob, FailedRun};
pub use app::{AppKey, Gate, GitHubApp, TokenSource, TokenUse};
pub use azure::{
    AzureDevOps, AzurePull, ENTRA_AUTHORITY, ENTRA_REFRESH_MARGIN, EntraToken, WRITER_GROUPS,
    azure_name, azure_segment, azure_unescaped, entra_token,
};
pub use bitbucket::{BITBUCKET_API, Bitbucket, BitbucketPermission, BitbucketPull};
pub use bitbucket_dc::{BitbucketDataCenter, DataCenterPull};
pub use body::{
    AllowedReferenceChange, AllowedTestChange, ChangeNote, Cost, Evidence, InputFlagNote,
    PrContent, gate_line, inert, render,
};
pub use branch::{AgentBranch, EvalBranch};
pub use commit_message::{CommitInput, CommitMessage, checked, write_commit_message};
pub use coverage::{cobertura_lines, cobertura_percent, lcov_lines, lcov_percent};
pub use error::PrError;
pub use forge::Forge;
pub use git::{
    AZURE_DEVOPS_USER, BEARER_USER, BITBUCKET_USER, GITHUB_USER, GITLAB_USER, Identity, Remote,
    is_entra_token, token_env, token_env_as,
};
pub use github::{GitHub, IssueComment, PullRequest};
pub use gitlab::{GitLab, GitLabIssue, IssueNote, MergeRequest};
pub use hooks::HookDone;
pub use locale::{Locale, PrStrings, UnknownLocale};
pub use review::{GITLAB_DEVELOPER, NoteAt, PullState, REPLY_MARKER, ReviewNote};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pushed {
    pub branch: AgentBranch,
    pub commit: String,
}

/// `text` with every secret value masked (#509): the last check before
/// anything leaves for a forge, under what the lifecycle already masked
/// and counted. A commit message, a PR, a comment: none is sent unscanned.
pub fn masked(text: &str) -> String {
    aegis_gate::secrets::redact(text).text
}

/// Commits the tree the gate passed and pushes it to `agent/<issue_id>`.
pub fn push_gated(
    repo: &Path,
    issue_id: &str,
    gate: &GateReport,
    message: &str,
    who: &Identity,
    remote: &Remote,
) -> Result<Pushed, PrError> {
    if !gate.passed() {
        return Err(PrError::GateNotPassed);
    }
    let branch = AgentBranch::for_issue(issue_id)?;
    let staging = git::Staging::new(repo)?;
    let commit = git::commit_tree(&staging, &gate.tree, &gate.base, &masked(message), who)?;
    git::push(&staging, remote, &branch, &commit)?;
    Ok(Pushed { branch, commit })
}

/// A review round's commit (#243): the tree the gate passed on top of
/// `parent`, the PR branch's head the round started from, pushed to
/// `agent/<issue_id>` without force. If someone pushed to the branch
/// since, the push is refused and their commits stay.
pub fn push_gated_onto(
    repo: &Path,
    issue_id: &str,
    gate: &GateReport,
    parent: &str,
    message: &str,
    who: &Identity,
    remote: &Remote,
) -> Result<Pushed, PrError> {
    if !gate.passed() {
        return Err(PrError::GateNotPassed);
    }
    let branch = AgentBranch::for_issue(issue_id)?;
    let staging = git::Staging::new(repo)?;
    let commit = git::commit_tree(&staging, &gate.tree, parent, &masked(message), who)?;
    git::push(&staging, remote, &branch, &commit)?;
    Ok(Pushed { branch, commit })
}

/// Moves `agent/<issue_id>` on the fork to `commit`, without force (#678):
/// a feature's integration branch, after a part is merged on its tip. A
/// branch that moved since its tip was fetched is left as it is, and the
/// push refused. The commit must be in `repo`'s objects.
pub fn push_commit(
    repo: &Path,
    issue_id: &str,
    commit: &str,
    remote: &Remote,
) -> Result<AgentBranch, PrError> {
    let branch = AgentBranch::for_issue(issue_id)?;
    let staging = git::Staging::new(repo)?;
    git::push(&staging, remote, &branch, commit)?;
    Ok(branch)
}

/// Sets `eval/<run_id>` on the fork at `commit`, the base the eval run's
/// PR targets, so the PR shows only the agent's patch. The commit must be in
/// `repo`'s objects.
pub fn push_eval_base(
    repo: &Path,
    run_id: &str,
    commit: &str,
    remote: &Remote,
) -> Result<EvalBranch, PrError> {
    let branch = EvalBranch::for_run(run_id)?;
    let staging = git::Staging::new(repo)?;
    git::push_ref(&staging, remote, &branch.full_ref(), commit)?;
    Ok(branch)
}

/// Deletes the agent's branch when it stops, only while the branch still
/// holds the commit the agent pushed.
pub fn abandon(repo: &Path, pushed: &Pushed, remote: &Remote) -> Result<(), PrError> {
    let staging = git::Staging::new(repo)?;
    git::delete(&staging, remote, &pushed.branch, &pushed.commit)
}

#[cfg(test)]
mod tests {
    #[test]
    fn masked_text_keeps_no_secret_and_the_rest_as_it_was() {
        // Built at run time, so no secret-shaped string sits in the source.
        let token = format!("ghp_{}", "Q".repeat(36));
        let text = format!("fixed the cart; token {token} was in the log\n");
        let out = super::masked(&text);
        assert!(!out.contains(&token), "{out}");
        assert_eq!(
            out,
            "fixed the cart; token [REDACTED:github-token] was in the log\n"
        );
        assert_eq!(super::masked("plain words"), "plain words");
    }
}
