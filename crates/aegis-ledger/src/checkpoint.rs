//! A job's progress, kept between its model calls (#662) so another
//! worker, on any machine, goes on where it stopped instead of from its
//! first turn: the tree as a patch from where it started, and the agent's
//! state as JSON the lifecycle reads back. Nothing in it names a path of
//! the machine that wrote it.

use serde::{Deserialize, Serialize};

/// The format of [`Checkpoint::state`]; a checkpoint of another one is
/// not resumed.
pub const CHECKPOINT_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Checkpoint {
    pub version: u32,
    /// The job's model calls when it was written.
    pub turn: u32,
    /// The commit the patch is checked against.
    pub base: String,
    /// The commit the job's tree started from (the base, or a PR's head
    /// for a review round).
    pub start: String,
    /// The tree's changes from `start`, as `git diff --binary`, paths from
    /// the repo's root.
    pub diff: String,
    /// What the lifecycle needs to go on: the session, the router's
    /// counters, the loop's own.
    pub state: serde_json::Value,
}
