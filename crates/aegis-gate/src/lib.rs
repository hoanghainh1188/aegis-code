//! Gate runner (design §6.1): the steps of the repo's language profile in
//! order (ADR 0004), stopping at the first failure, with a structured
//! report and a short text for the agent.
//!
//! This first version runs on the machine with a clean environment (#11);
//! the same steps move into the sandbox container with #5. Sonar is
//! reported as not configured until it has a server.

mod command;
mod deps;
mod error;
mod failures;
mod gate;
mod git;
mod integrity;
mod report;
mod sandbox;

pub use command::{CancelToken, INHERITED_ENV};
pub use error::GateError;
pub use gate::{GateRequest, Repro, run_gate};
pub use report::{GateReport, Skip, StepName, StepReport, StepStatus};
pub use sandbox::{Sandbox, WORKDIR};
