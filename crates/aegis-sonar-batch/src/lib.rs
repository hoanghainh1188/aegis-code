//! Sonar warnings as one batch (design §3.4, §6.3, #17).
//!
//! [`group`] splits the warnings: those at one place go together by the
//! function (or impl, class) around them, found with the profile's
//! tree-sitter grammar; a data-flow warning (source → propagation → sink)
//! is a group of its own. Each group is one agent session on its own
//! branch; the lifecycle runs them side by side.
//!
//! [`integrate`] then merges the group branches one at a time into
//! `agent/integration-<batch-id>`, running the gate after each merge. A
//! conflict or a failed gate drops the merge, and the group runs again on
//! the integration branch as it is. The batch ends in one PR.
//!
//! The warnings are the gate's own Sonar type ([`SonarIssue`]), read from
//! the server with [`aegis_gate::open_issues`] or from a saved
//! `/api/issues/search` response with [`parse_search`].
//!
//! [`poll`] asks the server for the new warnings of a project's main
//! branch on the repo's schedule (#27); the server turns each poll's new
//! warnings into one batch, whose issue [`issue_body`] writes.

mod error;
mod git;
mod group;
mod integrate;
pub mod poll;
mod warning;

pub use aegis_gate::SonarIssue;
pub use error::BatchError;
pub use git::{Author, IntegrationTree, commit_tree, integration_branch};
pub use group::{Group, Scope, group};
pub use integrate::{Checked, Dropped, Integrated, Steps, integrate};
pub use poll::{PollConfig, Severity, SonarServer, issue_body};
pub use warning::{is_flow, parse_search, path_of};
