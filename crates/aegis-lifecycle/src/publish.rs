//! Step 7 of design §2: the gate passed, so the gated tree goes to
//! `agent/<issue-id>` on the fork and a PR is opened there.

use std::path::Path;

use aegis_gate::GateReport;
use aegis_lang::{CoverageFormat, RepoProfiles};
use aegis_ledger::{InputFlag, IssueLedger, LedgerStore, Tier};
use aegis_pr::{
    Bitbucket, CommitInput, Cost, Forge, GitHub, GitLab, Identity, InputFlagNote, PrContent,
    Remote, TokenSource, abandon, cobertura_lines, cobertura_percent, lcov_lines, lcov_percent,
    push_eval_base, push_gated, push_gated_onto, render, write_commit_message,
};
use aegis_provider::ModelProvider;

use crate::answer::FinalAnswer;
use crate::review::ReviewRound;
use crate::{ForkConfig, ForkPlatform};
use crate::{IssueInput, LifecycleError, Platform};

/// `text` with every secret value masked before it leaves the platform
/// (#509), the masked ones counted in the ledger at `place`. Pure and
/// infallible: no text is sent unscanned.
async fn masked<S: LedgerStore + Sync>(
    ledger: &mut IssueLedger<'_, S>,
    place: &str,
    text: &str,
) -> Result<String, LifecycleError> {
    let r = aegis_gate::secrets::redact(text);
    ledger
        .secrets_blocked(place, u32::try_from(r.count()).unwrap_or(u32::MAX))
        .await?;
    Ok(r.text)
}

/// Line coverage from the reports the gate's coverage step left in
/// `tree`, of the parts it checked (`parts`, #199): a part it did not check
/// may hold a report of an earlier attempt. Several reports count their
/// lines together; when one gives only a rate there is no single number.
fn coverage_percent(profiles: &RepoProfiles, tree: &Path, parts: &[String]) -> Option<f64> {
    let reports: Vec<(f64, Option<(u64, u64)>)> = profiles
        .parts()
        .iter()
        .filter(|part| parts.contains(&part.dir))
        .filter_map(|part| {
            let coverage = part.profile.coverage.as_ref()?;
            let dir = tree.join(&part.dir);
            let matches = aegis_lang::find_reports(&dir, &coverage.path);
            let report = match matches.as_slice() {
                [single] => single,
                _ => return None,
            };
            let text = std::fs::read_to_string(report).ok()?;
            match coverage.format {
                CoverageFormat::Lcov => Some((lcov_percent(&text)?, lcov_lines(&text))),
                CoverageFormat::Cobertura => {
                    Some((cobertura_percent(&text)?, cobertura_lines(&text)))
                }
                _ => None,
            }
        })
        .collect();
    if let [(percent, _)] = reports.as_slice() {
        return Some(*percent);
    }
    let lines = reports
        .iter()
        .map(|(_, lines)| *lines)
        .collect::<Option<Vec<_>>>()?;
    let (hit, found) = lines
        .iter()
        .fold((0, 0), |(h, f), (hit, found)| (h + hit, f + found));
    (found > 0).then(|| hit as f64 * 100.0 / found as f64)
}

pub(crate) struct Published {
    pub url: String,
    /// Something to tell though the PR is up to date.
    pub note: Option<String>,
}

/// The fix [`publish`] opens a PR for: the issue, the agent's answer, the
/// gate that passed it on `tree`, and the input flags the PR tells (#444).
#[derive(Clone, Copy)]
pub(crate) struct PublishContext<'a> {
    pub profiles: &'a RepoProfiles,
    pub tree: &'a Path,
    pub issue: &'a IssueInput,
    pub answer: &'a FinalAnswer,
    pub gate: &'a GateReport,
    pub flags: &'a [InputFlag],
}

pub(crate) async fn publish<P, S>(
    platform: &Platform<P, S>,
    ledger: &mut IssueLedger<'_, S>,
    fix: PublishContext<'_>,
) -> Result<Published, LifecycleError>
where
    P: ModelProvider + Sync,
    S: LedgerStore + Sync,
{
    let PublishContext {
        profiles,
        tree,
        issue,
        answer,
        gate,
        flags,
    } = fix;
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
    let commit_message = masked(ledger, "commit message", &message.text).await?;
    // Masked before rendering too: rendering makes links inert, which
    // could hide a URL's password from the scan of the whole PR.
    let cause = masked(ledger, "PR", &answer.cause).await?;
    let mut changes = Vec::with_capacity(answer.changes.len());
    for c in &answer.changes {
        changes.push(aegis_pr::ChangeNote {
            location: masked(ledger, "PR", &c.location).await?,
            note: masked(ledger, "PR", &c.note).await?,
        });
    }
    let forge = fork_forge(platform)?;
    let remote = fork_remote(platform).await?;
    let identity = Identity {
        name: cfg.identity.name.clone(),
        email: cfg.identity.email.clone(),
    };
    let pushed = push_gated(tree, &issue.id, gate, &commit_message, &identity, &remote)?;
    // An eval run's PR targets a branch at the issue's old base, so it shows
    // only the agent's patch (#98).
    let target = match &issue.hidden {
        Some(_) => push_eval_base(tree, &issue.id, &gate.base, &remote)?
            .name()
            .to_owned(),
        None => cfg.repo.base.clone(),
    };

    // The sources were checked before the gate (#508); each links to its
    // lines on the PR's commit.
    let mut evidence = Vec::with_capacity(answer.evidence.len());
    for e in &answer.evidence {
        evidence.push(aegis_pr::Evidence {
            url: crate::evidence::line_url(&cfg.repo.fork, &pushed.commit, &e.location),
            location: masked(ledger, "PR", &e.location).await?,
            excerpt: match &e.excerpt {
                Some(x) => Some(masked(ledger, "PR", x).await?),
                None => None,
            },
        });
    }
    let record = ledger.record();
    let body = render(&PrContent {
        issue_url: issue.url.clone(),
        cause,
        evidence,
        changes,
        repro_test: answer.repro.as_ref().map(|r| r.test.clone()),
        allowed_test_changes: answer.allowed_test_changes.clone(),
        allowed_reference_changes: answer.allowed_reference_changes.clone(),
        gate,
        coverage_percent: coverage_percent(profiles, tree, &gate.parts),
        cost: Cost {
            models: record.tokens.keys().cloned().collect(),
            model_calls: record.model_calls,
            usd: record.cost_usd,
        },
        input_flags: flags
            .iter()
            .map(|f| InputFlagNote {
                source: f.source.clone(),
                location: f.location.clone(),
                excerpt: f.excerpt.clone(),
            })
            .collect(),
        locale: cfg.repo.locale,
    });
    let body = masked(ledger, "PR", &body).await?;
    let title = commit_message.lines().next().unwrap_or("fix").to_owned();
    match forge
        .open_request(&pushed.branch, &target, &title, &body, cfg.run.draft_prs)
        .await
    {
        Ok(url) => Ok(Published { url, note: None }),
        Err(e) => {
            // No PR, no branch: the fork is left as it was.
            let _ = abandon(tree, &pushed, &remote);
            Err(e.into())
        }
    }
}

/// The fork's forge: a GitHub repository, a GitLab project (#252) or a
/// Bitbucket Cloud (#634) or Data Center (#635) repository, reached with
/// tokens from `source`:
/// a GitHub repo with either, the others only with a fixed token.
pub fn fork_forge_of(fork: &ForkConfig, source: &TokenSource) -> Result<Forge, LifecycleError> {
    Ok(match (fork.platform, source) {
        (ForkPlatform::GitHub, _) => Forge::GitHub(GitHub::with_source(
            &fork.api,
            &fork.owner,
            &fork.name,
            source.clone(),
        )?),
        (ForkPlatform::GitLab, TokenSource::Static(token)) => {
            Forge::GitLab(GitLab::new(&fork.api, &fork.path(), token)?)
        }
        (ForkPlatform::Bitbucket, TokenSource::Static(token)) => {
            Forge::Bitbucket(Bitbucket::new(&fork.api, &fork.owner, &fork.name, token)?)
        }
        (ForkPlatform::BitbucketDataCenter, TokenSource::Static(token)) => {
            Forge::BitbucketDataCenter(aegis_pr::BitbucketDataCenter::new(
                &fork.api,
                &fork.owner,
                &fork.name,
                token,
            )?)
        }
        (
            ForkPlatform::GitLab | ForkPlatform::Bitbucket | ForkPlatform::BitbucketDataCenter,
            TokenSource::App { .. },
        ) => {
            return Err(LifecycleError::Config(
                "the GitHub App reaches GitHub repos only".into(),
            ));
        }
    })
}

fn fork_forge<P, S>(platform: &Platform<P, S>) -> Result<Forge, LifecycleError> {
    fork_forge_of(&platform.config.repo.fork, &platform.github)
}

/// Where the agent's branches are: the fork, or a local bare repo. Its
/// token is asked for now, right before the push or fetch it is for: an
/// app token minted at the start of a long job would have expired.
pub(crate) async fn fork_remote<P, S>(platform: &Platform<P, S>) -> Result<Remote, LifecycleError> {
    Ok(match &platform.config.repo.fork.push_to_local {
        Some(path) => Remote::local(path)?,
        None => fork_forge(platform)?.remote_now().await?,
    })
}

/// A review round's end (#243): the tree the gate passed committed on the
/// PR's head and pushed without force, then the reply on the PR. The PR
/// stays as it is; a push refused because the branch moved fails the
/// round, with the reviewers' commits kept.
pub(crate) async fn publish_round<P, S>(
    platform: &Platform<P, S>,
    ledger: &mut IssueLedger<'_, S>,
    tree: &Path,
    issue: &IssueInput,
    round: &ReviewRound,
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
    let commit_message = masked(ledger, "commit message", &message.text).await?;
    let remote = fork_remote(platform).await?;
    let identity = Identity {
        name: cfg.identity.name.clone(),
        email: cfg.identity.email.clone(),
    };
    let pushed = push_gated_onto(
        tree,
        round.branch_id(),
        gate,
        &round.pull.head_sha,
        &commit_message,
        &identity,
        &remote,
    )
    .map_err(|e| {
        LifecycleError::Config(format!(
            "pushing to {}: {e} (someone may have pushed to it since the round began)",
            round.pull.head_ref
        ))
    })?;
    // The commit is on the PR: a reply that fails to post is told, not
    // an error.
    let reply = masked(
        ledger,
        "review reply",
        &round.reply(&pushed.commit, answer, cfg.repo.locale),
    )
    .await?;
    let note = fork_forge(platform)?
        .comment_on_request(round.pull.number, &reply)
        .await
        .err()
        .map(|e| format!("pushed, but the reply was not posted: {e}"));
    Ok(Published {
        url: round.pull.url.clone(),
        note,
    })
}

#[cfg(test)]
mod tests {
    use aegis_lang::{Language, ProfileOverrides, Profiles, ResolvedProfile};

    use super::*;

    fn profile(language: Language, coverage: &str) -> ResolvedProfile {
        let overrides: ProfileOverrides = toml::from_str(coverage).unwrap();
        Profiles::builtin()
            .unwrap()
            .get(language)
            .unwrap()
            .resolve(&overrides)
            .unwrap()
    }

    fn rust_and_python() -> RepoProfiles {
        let rust = r#"
[gate]
coverage = ["true"]
[coverage]
format = "lcov"
path = "lcov.info"
"#;
        let python = r#"
[gate]
coverage = ["true"]
[coverage]
format = "cobertura"
path = "coverage.xml"
"#;
        RepoProfiles::new(
            profile(Language::Rust, rust),
            vec![("py".into(), profile(Language::Python, python))],
        )
        .unwrap()
    }

    #[test]
    fn coverage_comes_from_the_parts_the_gate_checked() {
        let tree = std::env::temp_dir().join(format!("aegis-coverage-{}", std::process::id()));
        std::fs::create_dir_all(tree.join("py")).unwrap();
        // The root's report is left from an earlier attempt.
        std::fs::write(tree.join("lcov.info"), "LF:10\nLH:1\n").unwrap();
        std::fs::write(
            tree.join("py/coverage.xml"),
            r#"<coverage line-rate="0.9" lines-covered="27" lines-valid="30">"#,
        )
        .unwrap();
        let profiles = rust_and_python();
        let py = vec!["py".to_owned()];
        assert_eq!(coverage_percent(&profiles, &tree, &py), Some(90.0));
        // Both checked: their lines together, 28 of 40.
        let both = vec![String::new(), "py".to_owned()];
        assert_eq!(coverage_percent(&profiles, &tree, &both), Some(70.0));
        // Only rates to go by: no single number.
        std::fs::write(
            tree.join("py/coverage.xml"),
            r#"<coverage line-rate="0.9">"#,
        )
        .unwrap();
        assert_eq!(coverage_percent(&profiles, &tree, &both), None);
        assert_eq!(coverage_percent(&profiles, &tree, &py), Some(90.0));
        let _ = std::fs::remove_dir_all(&tree);
    }
}
