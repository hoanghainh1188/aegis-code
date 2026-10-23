//! Merging the group branches (design §6.3): one at a time into the
//! integration branch, the gate after each merge. A conflict or a failed
//! gate drops that merge and the group runs once more on the integration
//! branch as it is then; a second miss drops the group.

use std::fmt;
use std::future::Future;
use std::path::Path;

use crate::{BatchError, Group, IntegrationTree};

/// One gate run on the integration tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checked<R> {
    pub passed: bool,
    pub report: R,
}

/// What the batch needs from the platform: the gate, and a group's agent
/// session again.
pub trait Steps {
    type Report;
    type Error: From<BatchError>;

    /// The gate on the tree at `tree`, as it is after a merge.
    fn gate(
        &self,
        tree: &Path,
    ) -> impl Future<Output = Result<Checked<Self::Report>, Self::Error>> + Send;

    /// The group's session again, from `base` (the integration tip):
    /// `Ok(commit)` of the tree its gate passed, or why it stopped.
    fn rerun(
        &self,
        group: &Group,
        base: &str,
    ) -> impl Future<Output = Result<Result<String, String>, Self::Error>> + Send;
}

/// Why a group's fix is not in the batch's PR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dropped {
    /// Its session stopped without a passing patch.
    Stopped(String),
    /// Its rerun conflicted with the integration branch again.
    Conflict,
    /// The gate failed on the merge of its rerun too.
    GateFailed,
    /// The rerun after a conflict or a failed gate stopped.
    RerunStopped(String),
}

impl fmt::Display for Dropped {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stopped(why) => write!(f, "the session stopped: {why}"),
            Self::Conflict => f.write_str("conflicts with the other groups' changes"),
            Self::GateFailed => f.write_str("the gate fails once merged with the other groups"),
            Self::RerunStopped(why) => {
                write!(f, "the rerun on the integration branch stopped: {why}")
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Integrated<R> {
    /// Tip of the integration branch.
    pub head: String,
    /// Groups in the branch, in merge order.
    pub merged: Vec<String>,
    /// Of those, the ones that needed a rerun.
    pub retried: Vec<String>,
    pub dropped: Vec<(String, Dropped)>,
    /// The gate on `head`; `None` when no group was merged.
    pub report: Option<R>,
}

enum Attempt<R> {
    Passed(R),
    Conflict,
    Failed,
}

async fn try_merge<S: Steps>(
    steps: &S,
    tree: &IntegrationTree,
    group: &Group,
    commit: &str,
) -> Result<Attempt<S::Report>, S::Error> {
    let before = tree.head()?;
    let message = format!(
        "Merge Sonar group {} ({})",
        group.id,
        group.scope.describe()
    );
    if !tree.merge(commit, &message)? {
        return Ok(Attempt::Conflict);
    }
    let checked = steps.gate(tree.path()).await?;
    if checked.passed {
        tree.advance()?;
        Ok(Attempt::Passed(checked.report))
    } else {
        tree.reset(&before)?;
        Ok(Attempt::Failed)
    }
}

/// Merges each group's commit (or the reason its session stopped) in
/// order into `tree`'s integration branch.
pub async fn integrate<S: Steps>(
    steps: &S,
    tree: &IntegrationTree,
    branches: Vec<(Group, Result<String, String>)>,
) -> Result<Integrated<S::Report>, S::Error> {
    let mut merged = Vec::new();
    let mut retried = Vec::new();
    let mut dropped = Vec::new();
    let mut report = None;
    for (group, result) in branches {
        let commit = match result {
            Ok(commit) => commit,
            Err(why) => {
                dropped.push((group.id, Dropped::Stopped(why)));
                continue;
            }
        };
        if let Attempt::Passed(r) = try_merge(steps, tree, &group, &commit).await? {
            report = Some(r);
            merged.push(group.id);
            continue;
        }
        let head = tree.head()?;
        match steps.rerun(&group, &head).await? {
            Err(why) => dropped.push((group.id, Dropped::RerunStopped(why))),
            Ok(again) => match try_merge(steps, tree, &group, &again).await? {
                Attempt::Passed(r) => {
                    report = Some(r);
                    retried.push(group.id.clone());
                    merged.push(group.id);
                }
                Attempt::Conflict => dropped.push((group.id, Dropped::Conflict)),
                Attempt::Failed => dropped.push((group.id, Dropped::GateFailed)),
            },
        }
    }
    Ok(Integrated {
        head: tree.head()?,
        merged,
        retried,
        dropped,
        report,
    })
}
