//! Untrusted text the job read that reads like an instruction to an AI
//! agent (#444): found by the platform, not reported by the model, and
//! shown to the reviewer in a fixed section of the PR.

mod collect;
mod detect;

pub use collect::{MAX_FLAGS, collect};
pub use detect::{Hit, MAX_EXCERPT_CHARS, detect};
