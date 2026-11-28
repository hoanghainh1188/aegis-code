//! A repo's deprecation warnings, fixed as one batch (#705). The job is of
//! kind [`IssueKind::Deprecation`]: at the base, each part of the repo
//! whose profile has `[deprecations]` runs that command (in the sandbox,
//! after its dependencies are fetched, as the gate does), and its output
//! is read into one warning per call of a deprecated API. The warnings
//! then take the Sonar batch's road (`aegis-sonar-batch`): grouped by
//! function, a session per group side by side, merged one at a time with
//! the gate after each, and one PR that names each old API and its
//! replacement.
//!
//! The job's issue may limit the warnings to those its body lists in a
//! ```json block of keys (the server's schedule takes only new ones, 3/3).

use aegis_gate::{SonarIssue, TextRange, WORKDIR};
use aegis_lang::{Deprecation, read_deprecations};

use super::*;
use crate::batch::run_batch;

/// Warnings one batch takes, as for a Sonar batch.
const MAX_WARNINGS: usize = 100;
/// What a deprecation warning's component starts with.
const COMPONENT: &str = "deprecations";

/// The warning of `d`, in the Sonar batch's type.
pub(crate) fn as_warning(d: &Deprecation) -> SonarIssue {
    SonarIssue {
        key: d.key(),
        rule: format!("deprecation:{}", d.code),
        severity: None,
        component: format!("{COMPONENT}:{}", d.path),
        line: Some(d.line),
        message: d.message.clone(),
        text_range: Some(TextRange {
            start_line: d.line,
            end_line: d.line,
        }),
        flows: Vec::new(),
    }
}

/// What the job's issue asks of the batch, in a ```json block of its
/// body: the keys to take (an array), or an object with the keys earlier
/// batches took (`skip`, the server's schedule, 3/3) and how many groups
/// at most (`max_groups`).
#[derive(Debug, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Asked {
    #[serde(default)]
    take: Option<Vec<String>>,
    #[serde(default)]
    skip: Vec<String>,
    #[serde(default)]
    max_groups: Option<usize>,
}

fn asked(body: &str) -> Asked {
    let Some(start) = body.find("```json").map(|i| i + "```json".len()) else {
        return Asked::default();
    };
    let rest = &body[start..];
    let Some(end) = rest.find("```") else {
        return Asked::default();
    };
    let json = rest[..end].trim();
    match serde_json::from_str::<Vec<String>>(json) {
        Ok(take) => Asked {
            take: Some(take),
            ..Asked::default()
        },
        Err(_) => serde_json::from_str(json).unwrap_or_default(),
    }
}

/// The deprecations of every part of the repo at `tree` (the base).
async fn scan(
    cfg: &RunConfig,
    profiles: &RepoProfiles,
    tree: &Path,
    job_dir: &Path,
    cancel: &CancellationToken,
) -> Result<Result<Vec<Deprecation>, String>, LifecycleError> {
    let mut found = Vec::new();
    let mut listed = false;
    for part in profiles.parts() {
        let Some(deprecations) = part.profile.profile.deprecations.clone() else {
            continue;
        };
        listed = true;
        if let Some(why) = fetch_base(cfg, part, tree, job_dir, cancel).await? {
            return Ok(Err(why));
        }
        let gate_cancel = CancelToken::new();
        let _stop = CancelOnDrop(gate_cancel.clone());
        let _watcher = AbortOnDrop(tokio::spawn({
            let (outer, inner) = (cancel.clone(), gate_cancel.clone());
            async move {
                outer.cancelled().await;
                inner.cancel();
            }
        }));
        let sandbox = sandbox(cfg, part)?;
        // Where the compiler sees the part: its paths are read against it.
        let root = match &sandbox {
            Some(_) => Path::new(WORKDIR).join(&part.dir),
            None => tree.join(&part.dir),
        };
        let runner = Runner {
            env: vec![],
            timeout: Duration::from_secs(cfg.run.step_timeout_s),
            cancel: gate_cancel,
            sandbox,
        };
        let argv = deprecations
            .command
            .render(&[])
            .map_err(|e| LifecycleError::Config(e.to_string()))?;
        let log = job_dir.join(match part.dir.is_empty() {
            true => "deprecations.log".to_owned(),
            false => format!("deprecations-{}.log", part.dir.replace('/', "_")),
        });
        let dir = tree.join(&part.dir);
        let ran = tokio::task::spawn_blocking(move || runner.run(&argv, &dir, &log, &[]))
            .await
            .map_err(|e| {
                LifecycleError::Config(format!("listing the deprecations did not finish: {e}"))
            })??;
        if cancel.is_cancelled() {
            return Ok(Err("cancelled".into()));
        }
        let output = std::fs::read_to_string(&ran.log).unwrap_or_default();
        for d in read_deprecations(deprecations.output, &output, &root) {
            let path = match part.dir.is_empty() {
                true => d.path.clone(),
                false => format!("{}/{}", part.dir, d.path),
            };
            found.push(Deprecation { path, ..d });
        }
    }
    if !listed {
        return Ok(Err(
            "no profile of the repo lists its deprecation warnings yet".into(),
        ));
    }
    Ok(Ok(found))
}

pub(crate) async fn deprecation_batch<P, S>(
    platform: &Platform<P, S>,
    ledger: &mut IssueLedger<'_, S>,
    profiles: &RepoProfiles,
    issue: &IssueInput,
    cancel: &CancellationToken,
) -> Result<Ending, LifecycleError>
where
    P: ModelProvider + Send + Sync + 'static,
    S: LedgerStore + Send + Sync + 'static,
{
    let cfg = &platform.config;
    ledger.classified(IssueKind::Deprecation).await?;
    let base = job::resolve(
        &cfg.repo.path,
        issue.base.as_deref().unwrap_or(&cfg.repo.base),
    )?;
    let job_dir = cfg.run.state_dir.join("jobs").join(&issue.id);
    let checkout = job::Worktree::create(&cfg.repo.path, &base, &job_dir.join("deprecations"))?;
    let found = scan(cfg, profiles, checkout.path(), &job_dir, cancel).await?;
    drop(checkout);
    let found = match found {
        Ok(found) => found,
        Err(why) => return Ok(Ending::stop(Outcome::StoppedSteps, why)),
    };
    let asked = asked(&issue.body);
    let mut warnings: Vec<SonarIssue> = found
        .iter()
        .filter(|d| {
            let key = d.key();
            asked.take.as_ref().is_none_or(|keys| keys.contains(&key)) && !asked.skip.contains(&key)
        })
        .map(as_warning)
        .collect();
    warnings.dedup_by(|a, b| a.key == b.key);
    warnings.truncate(MAX_WARNINGS);
    if warnings.is_empty() {
        let why = if found.is_empty() {
            "no deprecation warnings to fix at the base"
        } else {
            "no new deprecation warnings to fix at the base: earlier batches took them"
        };
        return Ok(Ending::stop(Outcome::StoppedSteps, why));
    }
    ledger
        .deprecations_taken(warnings.iter().map(|w| w.key.clone()).collect())
        .await?;
    let max_groups = asked.max_groups.unwrap_or(crate::batch::MAX_GROUPS);
    run_batch(
        platform, ledger, profiles, issue, &warnings, max_groups, cancel,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_deprecation_is_a_warning_at_its_line() {
        let d = Deprecation {
            path: "src/lib.rs".into(),
            line: 40,
            code: "deprecated".into(),
            message: "use of deprecated function `old_total`: use `sum` instead".into(),
            occurrence: 0,
        };
        let w = as_warning(&d);
        assert_eq!(w.key, d.key());
        assert_eq!(w.rule, "deprecation:deprecated");
        assert_eq!(aegis_sonar_batch::path_of(&w.component), "src/lib.rs");
        assert_eq!(
            w.text_range.map(|r| (r.start_line, r.end_line)),
            Some((40, 40))
        );
        assert!(!aegis_sonar_batch::is_flow(&w));
    }

    #[test]
    fn the_issue_may_list_the_keys_it_takes_or_skips() {
        assert_eq!(
            asked("New ones:\n```json\n[\"deprecation:00ff\"]\n```\n").take,
            Some(vec!["deprecation:00ff".into()])
        );
        let scheduled = asked(
            "Scheduled.\n```json\n{\"skip\": [\"deprecation:01\"], \"max_groups\": 3}\n```\n",
        );
        assert_eq!(scheduled.take, None);
        assert_eq!(scheduled.skip, ["deprecation:01"]);
        assert_eq!(scheduled.max_groups, Some(3));
        assert_eq!(asked("Scheduled."), Asked::default());
        assert_eq!(asked("```json\n{\"other\": 1}\n```"), Asked::default());
    }
}
