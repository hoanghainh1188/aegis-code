//! Gate runner in Docker: fmt, clippy, repro test, workspace test, coverage, Sonar (design §6.1).

mod crate_policy;

pub use crate_policy::{
    AllowedCrates, CRATES_IO, CratePolicyError, CrateViolation, ViolationKind,
    new_crate_violations, touches_cargo_files,
};
