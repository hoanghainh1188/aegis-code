//! The lifecycle of one issue (design §2): Light classifies it, Core works
//! on it with the ACI tools, the gate checks the patch, failures go back
//! summarized, the router escalates to Expert or stops, and a passing
//! patch becomes a PR on the fork. Every model call and gate run is in the
//! issue's ledger record.
//!
//! Backends are chosen with enums (`aegis_provider::Provider`, ledger
//! stores), not trait objects, and the job's future is `Send + 'static`
//! (ADR 0003).

mod answer;
mod checks;
mod config;
mod error;
mod issue;
mod job;
mod light;
mod publish;
mod run;

pub use answer::{FinalAnswer, Status, parse_answer};
pub use checks::{Halt, JobCheck, fold_edits};
pub use config::{
    BudgetSection, ExtraServer, ForkConfig, IdentityConfig, RepoConfig, RunConfig, RunSection,
};
pub use error::LifecycleError;
pub use issue::IssueInput;
pub use run::{IssueReport, Platform, job_dir, run_issue};
