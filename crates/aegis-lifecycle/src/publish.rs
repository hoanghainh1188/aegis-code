//! Step 7 of design §2: the gate passed, so the gated tree goes to
//! `agent/<issue-id>` on the fork and a PR is opened there.

use std::path::Path;

use aegis_gate::GateReport;
use aegis_lang::{CoverageFormat, ResolvedProfile};
use aegis_ledger::{IssueLedger, LedgerStore, Tier};
use aegis_pr::{
    CommitInput, Cost, GitHub, Identity, PrContent, Remote, abandon, cobertura_percent,
    lcov_percent, push_gated, render, write_commit_message,
};
use aegis_provider::ModelProvider;

use crate::answer::FinalAnswer;
use crate::{IssueInput, LifecycleError, Platform};

/// Line coverage from the report the gate's coverage step left in `tree`.
fn coverage_percent(profile: &ResolvedProfile, tree: &Path) -> Option<f64> {
    let coverage = profile.coverage.as_ref()?;
    let text = std::fs::read_to_string(tree.join(&coverage.path)).ok()?;
    match coverage.format {
        CoverageFormat::Lcov => lcov_percent(&text),
        CoverageFormat::Cobertura => cobertura_percent(&text),
        _ => None,
    }
}

pub(crate) struct Published {
    pub url: String,
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn publish<P, S>(
    platform: &Platform<P, S>,
    ledger: &mut IssueLedger<'_, S>,
    profile: &ResolvedProfile,
    tree: &Path,
    issue: &IssueInput,
    answer: &FinalAnswer,
    gate: &GateReport,
) -> Result<Published, LifecycleError>
where
    P: ModelProvider + Sync,
    S: LedgerStore + Sync,
{
    let cfg = &platform.config;
    let light = &cfg.models.light;
    let message = write_commit_message(
        &platform.provider,
        &light.id,
        light.effort,
        &CommitInput {
            issue_title: &issue.title,
            issue_url: &issue.url,
            cause: &answer.cause,
            changes: &answer.changes,
        },
    )
    .await;
    if let Some(response) = &message.response {
        ledger
            .model_call(Tier::Light, &light.id, &response.usage)
            .await?;
    }
    let fork = &cfg.repo.fork;
    let github = GitHub::new(&fork.api, &fork.owner, &fork.name, &platform.github_token)?;
    let remote = match &fork.push_to_local {
        Some(path) => Remote::local(path)?,
        None => github.remote()?,
    };
    let identity = Identity {
        name: cfg.identity.name.clone(),
        email: cfg.identity.email.clone(),
    };
    let pushed = push_gated(tree, &issue.id, gate, &message.text, &identity, &remote)?;

    let record = ledger.record();
    let body = render(&PrContent {
        issue_url: issue.url.clone(),
        cause: answer.cause.clone(),
        changes: answer.changes.clone(),
        repro_test: answer.repro.as_ref().map(|r| r.test.clone()),
        allowed_test_changes: answer.allowed_test_changes.clone(),
        gate,
        coverage_percent: coverage_percent(profile, tree),
        cost: Cost {
            models: record.tokens.keys().cloned().collect(),
            model_calls: record.model_calls,
            usd: record.cost_usd,
        },
    });
    let title = message.text.lines().next().unwrap_or("fix").to_owned();
    match github
        .open_pull_request(
            &pushed.branch,
            &cfg.repo.base,
            &title,
            &body,
            cfg.run.draft_prs,
        )
        .await
    {
        Ok(pr) => Ok(Published { url: pr.url }),
        Err(e) => {
            // No PR, no branch: the fork is left as it was.
            let _ = abandon(tree, &pushed, &remote);
            Err(e.into())
        }
    }
}
