//! Gate runner in Docker: fmt, clippy, repro test, workspace test, coverage, Sonar (design §6.1).
//!
//! Gate steps and the dependency check come from the repo's language
//! profile in `aegis-lang` (ADR 0004); the runner itself is #11.
