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
pub mod cache_sweep;
mod checks;
pub mod clone;
mod code_graph;
mod config;
mod cve;
mod diagnose;
mod error;
mod eval;
mod evidence;
mod feature;
pub mod flags;
pub mod injection;
mod issue;
mod job;
mod language;
pub use language::resolve_locale;
mod light;
pub mod memory;
pub mod memory_learn;
pub mod memory_sources;
mod preflight;
mod publish;
mod registry;
mod replay;
mod review;
mod run;
mod shown;
mod untrusted;

pub use answer::{FinalAnswer, ReviewReply, Status, SuspiciousInput, parse_answer};
pub use checks::{Halt, JobCheck, fold_tools, test_reminder};
pub use config::{
    AppConnection, BudgetSection, DirConfig, EntraConfig, ExecutionPool, ExtraServer, ForkConfig,
    ForkPlatform, IdentityConfig, LedgerBackend, MemorySection, RemoteCache, RemoteExecution,
    RepoConfig, RunConfig, RunSection, SonarConfig, upstream_of,
};
pub use cve::CveTarget;
pub use diagnose::TriageLane;
pub use error::LifecycleError;
pub use eval::{EvalIssue, hidden_command, load_set, next_run_id};
pub use feature::{
    Criterion, FeatureDeliveryMode, FeatureLane, FeaturePlan, FeatureRevision,
    MAX_PLANNED_SUB_JOBS, PlannedSubJob, ProposedPackage, render as render_feature_plan,
};
pub use issue::{HiddenTests, IssueInput, IssueSummary, open_github_issues};
pub use memory::{
    ExtractedMemory, MemoryLesson, MergedPrFacts, PITFALL_KIND, extract_from_merged_pr,
    fallback_extract_from_merged_pr, format_for_bp3, redact_for_embedding,
};
pub use memory_learn::{
    DiagnosisFacts, FeatureDesignFacts, LearnedMemory, RejectedPrFacts,
    extract_confirmed_diagnosis, extract_feature_design, extract_rejected_pattern,
    fallback_confirmed_diagnosis, fallback_feature_design, fallback_rejected_pattern,
};
pub use memory_sources::{MemorySources, SyncedMemory};
pub use preflight::{CheckItem, CheckStatus, PreflightOptions, PreflightReport, run_preflight};
pub use publish::{fork_forge_of, fork_token, fork_token_source};
pub use registry::{
    IssueTrustConfig, IssueTrustPolicy, ModeConfig, Modes, PatchScopeConfig, RepoRun, RepoSettings,
    RepoSpec, check_tenant_secrets, tenant_state_dir,
};
pub use replay::{Replay, ReplayProvider};
pub use review::ReviewRound;
pub use run::{IssueReport, Platform, job_dir, run_feature_revision, run_issue, run_review};

/// The start of a stop reason when the platform, not the agent, ended a
/// run on an error (#557): its tools did not start, the store failed. The
/// outcome stays `StoppedSteps` for the ledger; the issue comment tells
/// the reporter the platform failed rather than the step limit, and keeps
/// the error, which names the platform's own parts, for the operators.
pub const PLATFORM_ERROR: &str = "error: ";
