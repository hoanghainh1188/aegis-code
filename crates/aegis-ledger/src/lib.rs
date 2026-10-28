//! Per-issue JSON record: tokens, cost, outcome, review for KPI (design §4.3, §8).
//!
//! Every model call and gate run is appended to a [`LedgerStore`] as it
//! happens (ADR 0003), so a restart loses no cost data. The per-issue
//! [`IssueRecord`] is a fold over those events.

mod error;
mod issue;
mod kpi;
mod pricing;
mod record;
pub mod report;
mod store;

pub use error::LedgerError;
pub use issue::IssueLedger;
pub use kpi::{
    ACCEPT_RATE_MIN, CACHE_READ_MIN, COST_PER_ISSUE_MAX, Kpi, LANGUAGE_ACCEPT_RATE_MIN,
    REVIEW_MINUTES_MAX, accepted,
};
pub use pricing::{LongPrompt, ModelPrice, PriceTable};
pub use record::{
    GateRun, IssueId, IssueKind, IssueRecord, LedgerEvent, Outcome, Review, ReviewVerdict, Tier,
    TokenCounts, ToolCall,
};
pub use report::{Report, SetIssue};
pub use store::{CopyReport, JsonFileLedger, LedgerStore, MemoryLedger, copy_ledger};
