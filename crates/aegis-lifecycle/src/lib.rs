//! The lifecycle of one issue (design §2): Light classifies it, Core works
//! on it with the ACI tools, the gate checks the patch, failures go back
//! summarized, the router escalates to Expert or stops, and a passing
//! patch becomes a PR on the fork. Every model call and gate run is in the
//! issue's ledger record.
//!
//! Backends are chosen with enums (`aegis_provider::Provider`, ledger
//! stores), not trait objects, and the job's future is `Send + 'static`
//! (ADR 0003). Each registered repo runs with its mode's configuration and
//! its own settings ([`Modes`], #24).

mod answer;
mod batch;
mod checks;
mod code_graph;
mod config;
mod error;
mod eval;
mod issue;
mod job;
mod light;
mod publish;
mod registry;
mod replay;
mod run;

pub use answer::{FinalAnswer, Status, parse_answer};
pub use checks::{Halt, JobCheck, fold_tools, test_reminder};
pub use config::{
    BudgetSection, DirConfig, ExtraServer, ForkConfig, IdentityConfig, LedgerBackend, RepoConfig,
    RunConfig, RunSection, SonarConfig,
};
pub use error::LifecycleError;
pub use eval::{EvalIssue, hidden_command, load_set, next_run_id};
pub use issue::{HiddenTests, IssueInput};
pub use registry::{ModeConfig, Modes, RepoRun, RepoSettings, RepoSpec};
pub use replay::{Replay, ReplayProvider};
pub use run::{IssueReport, Platform, job_dir, run_issue};
