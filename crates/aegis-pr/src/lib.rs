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
mod review;

use std::path::Path;

use aegis_gate::GateReport;

pub use access::RepoAccess;
pub use actions::{FailedJob, FailedRun};
pub use body::{
    AllowedReferenceChange, AllowedTestChange, ChangeNote, Cost, PrContent, gate_line, inert,
    render,
};
pub use branch::{AgentBranch, EvalBranch};
pub use commit_message::{CommitInput, CommitMessage, checked, write_commit_message};
pub use coverage::{cobertura_lines, cobertura_percent, lcov_lines, lcov_percent};
pub use error::PrError;
pub use forge::Forge;
pub use git::{GITHUB_USER, GITLAB_USER, Identity, Remote, token_env, token_env_as};
pub use github::{GitHub, IssueComment, PullRequest};
pub use gitlab::{GitLab, GitLabIssue, IssueNote, MergeRequest};
pub use hooks::HookDone;
pub use review::{GITLAB_DEVELOPER, NoteAt, PullState, REPLY_MARKER, ReviewNote};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pushed {
    pub branch: AgentBranch,
    pub commit: String,
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
    let commit = git::commit_tree(&staging, &gate.tree, &gate.base, message, who)?;
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
    let commit = git::commit_tree(&staging, &gate.tree, parent, message, who)?;
    git::push(&staging, remote, &branch, &commit)?;
    Ok(Pushed { branch, commit })
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
