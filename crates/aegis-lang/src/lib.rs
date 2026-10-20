//! Language profiles for target repos (ADR 0004).
//!
//! Everything that depends on the language of the repo being maintained
//! (ACI grammar, edit checks, gate steps, dependencies, sandbox, Sonar,
//! prompt conventions) comes from a [`Profile`]. Defaults live in
//! `profiles/<language>.toml`; a repo overrides them with
//! [`ProfileOverrides`]. `aegis-aci`, `aegis-gate` and the issue lifecycle
//! read the resolved profile and name no tool of any language.

mod command;
mod deps;
mod error;
mod go_lock;
mod grammar;
mod language;
mod lockfile;
mod maven_lock;
mod npm_lock;
mod nuget_lock;
mod profile;
mod python_lock;
mod resolve;
mod symbols;
mod syntax;

pub use command::{CommandTemplate, Placeholder};
pub use deps::{DependencyViolation, LockedDependency, ViolationKind, new_dependency_violations};
pub use error::LangError;
pub use language::Language;
pub use lockfile::{AllowList, read_lockfile};
pub use profile::{
    Coverage, CoverageFormat, Dependencies, Detect, GateSteps, Profile, Profiles, Sandbox,
    TestOutput, glob_match,
};
pub use resolve::{
    GateOverrides, GatePlan, ProfileOverrides, ResolvedProfile, SkipReason, Step, StepOverride,
};
pub use symbols::{Symbol, SymbolKind, has_symbol_query, symbols, symbols_in};
pub use syntax::{MAX_SOURCE_BYTES, SyntaxError, has_grammar, syntax_errors, syntax_errors_in};
