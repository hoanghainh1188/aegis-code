//! Tier selection Light/Core/Expert, escalation and stop rules, $4 / 40-call budget (design §5).
//!
//! Everything is passed in as values (ADR 0003): this crate never reads
//! `config/*.toml` or the environment.

mod budget;
mod classify;
mod models;
mod rules;

pub use budget::{BudgetCheck, BudgetLimits, BudgetStop, Spent};
pub use classify::{Classification, ClassifyError, parse_classification};
pub use models::{ModelsConfig, TierModel};
pub use rules::{Decision, EscalationReason, EscalationRules, Router, Signal, StopReason};
