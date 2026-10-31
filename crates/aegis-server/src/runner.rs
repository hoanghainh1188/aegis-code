//! What runs one attempt at a job. The worker owns the queue, the lease
//! and the retries; a runner only says how the attempt went. Static
//! dispatch (ADR 0003 §7): the worker is generic over its runner.

use std::future::Future;

use aegis_ledger::Outcome;
use aegis_store::{Claim, Patch};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, PartialEq)]
pub enum Attempt {
    /// The lifecycle reached its outcome: a PR or a stop. Never retried.
    /// `patch`: what the PR holds (#312).
    Ended {
        outcome: Outcome,
        reason: Option<String>,
        pr_url: Option<String>,
        patch: Option<Patch>,
    },
    /// Infrastructure failed (provider, container, network, database):
    /// retried with backoff while retries remain.
    Infra(String),
    /// The job cannot run as configured (no run configuration for its
    /// repo, a source without a connector yet): retrying will not help.
    Rejected(String),
}

pub trait JobRunner: Send + Sync + 'static {
    /// One attempt at `claim`'s job. `cancel` fires when a cancel of the
    /// job is asked for; the attempt should then end soon. The future may
    /// also be dropped, when the lease is lost or the drain runs out.
    fn run(&self, claim: &Claim, cancel: CancellationToken)
    -> impl Future<Output = Attempt> + Send;
}
