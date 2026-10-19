//! Per-issue JSON record: tokens, cost, outcome, review for KPI (design §4.3, §8).
//!
//! Every model call and gate run is appended to a [`LedgerStore`] as it
//! happens (ADR 0003), so a restart loses no cost data. The per-issue
//! [`IssueRecord`] is a fold over those events.

mod error;
mod issue;
mod pricing;
mod record;
mod store;

pub use error::LedgerError;
pub use issue::IssueLedger;
pub use pricing::{ModelPrice, PriceTable};
pub use record::{
    GateRun, IssueId, IssueKind, IssueRecord, LedgerEvent, Outcome, Review, ReviewVerdict, Tier,
    TokenCounts,
};
pub use store::{JsonFileLedger, LedgerStore, MemoryLedger};
