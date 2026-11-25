//! Gate runner (design §6.1): the steps of the repo's language profile in
//! order (ADR 0004), stopping at the first failure, with a structured
//! report and a short text for the agent.
//!
//! This first version runs on the machine with a clean environment (#11);
//! the same steps move into the sandbox container with #5. Sonar runs when
//! the request has a server for it (#91), and is reported as not
//! configured otherwise.

mod build_cache;
mod command;
mod dependency_policy;
mod deps;
mod egress;
mod error;
mod failures;
mod findings;
mod gate;
mod git;
mod integrity;
mod parts;
mod report;
mod sandbox;
mod scope;
mod secret_scan;
pub mod secrets;
mod sidecar;
mod sonar;

pub use build_cache::{BuildCache, LAST_USED, REFRESHED, mark_refreshed, mark_used};
pub use command::{CancelToken, Exit, INHERITED_ENV, Ran, Runner, out_of_memory};
pub use dependency_policy::{DependencyPolicy, Upgrade, same_package};
pub use egress::{Egress, EgressSession};
pub use error::GateError;
pub use failures::{DID_NOT_FINISH, failing_tests, failing_tests_with};
pub use gate::{AuditPolicy, FetchStep, GateRequest, ReferenceGroup, Repro, run_gate};
pub use parts::GatePart;
pub use report::{
    GateReport, SidecarReport, Skip, StepName, StepReport, StepStatus, UpdatedReferences, tail,
};
pub use sandbox::{CLIENT_ENV, Hardening, Limits, Sandbox, WORKDIR, check_runtime};
pub use scope::{ScopePolicy, ScopeViolations, analyze_scope, is_test_file};
pub use secret_scan::{SecretException, SecretFinding, SecretPolicy, SecretScan, scan_patch};
pub use sidecar::{SIDECAR_PREFIX, SidecarSession, SidecarSpec, Sidecars, check_sidecars};
pub use sonar::{Flow, FlowLocation, SonarIssue, SonarStep, TextRange, open_issues};
