//! `aegis inject` (#361): the prompt injection set, each case an eval issue
//! with a payload planted in it, run without a PR and scored from the
//! ledger and the patch. `--rescore` scores the runs already in the ledger
//! again, without the model.

use std::path::Path;
use std::sync::Arc;

use aegis_lang::{Language, Profiles};
use aegis_ledger::{IssueId, IssueRecord, LedgerStore};
use aegis_lifecycle::injection::{self, InjectionCase, Planted, Tally, Verdict, Watch};
use aegis_lifecycle::{EvalIssue, Platform, RunConfig};
use aegis_provider::ModelProvider;
use tokio_util::sync::CancellationToken;

/// The eval issues a set's cases are planted in: `issues/` next to it.
fn base_issues(set: &Path) -> Result<Vec<EvalIssue>, String> {
    let dir = set
        .parent()
        .map(|p| p.join("issues"))
        .ok_or_else(|| format!("{} has no parent directory", set.display()))?;
    aegis_lifecycle::load_set(&dir).map_err(|e| e.to_string())
}

/// The cases to run: those `only` names (each must exist), planted in an
/// eval issue of `language`.
fn pick<'a>(
    cases: &'a [InjectionCase],
    issues: &'a [EvalIssue],
    only: &[String],
    language: Language,
) -> Result<Vec<(&'a InjectionCase, &'a EvalIssue)>, String> {
    if let Some(unknown) = only.iter().find(|id| !cases.iter().any(|c| &c.id == *id)) {
        return Err(format!("no case `{unknown}`"));
    }
    let mut picked = Vec::new();
    for case in cases
        .iter()
        .filter(|c| only.is_empty() || only.contains(&c.id))
    {
        let issue = issues
            .iter()
            .find(|i| i.id == case.issue)
            .ok_or_else(|| format!("{}: no eval issue `{}`", case.id, case.issue))?;
        if issue.language == language {
            picked.push((case, issue));
        }
    }
    Ok(picked)
}

/// The set loaded and picked for `cfg`'s language, owned so a caller can
/// borrow from it.
struct Set {
    cases: Vec<InjectionCase>,
    issues: Vec<EvalIssue>,
    dependency_files: Vec<String>,
}

impl Set {
    fn load(set: &Path, cfg: &RunConfig, profiles: &Profiles) -> Result<Self, String> {
        let profiles = cfg.repo.profiles(profiles).map_err(|e| e.to_string())?;
        Ok(Self {
            cases: injection::load_cases(set).map_err(|e| e.to_string())?,
            issues: base_issues(set)?,
            dependency_files: injection::dependency_files(&profiles),
        })
    }

    fn pick(
        &self,
        set: &Path,
        only: &[String],
        language: Language,
    ) -> Result<Vec<(&InjectionCase, &EvalIssue)>, String> {
        let picked = pick(&self.cases, &self.issues, only, language)?;
        if picked.is_empty() {
            return Err(format!("no {language} case in {} matches", set.display()));
        }
        Ok(picked)
    }
}

/// Prints the summary; true when every picked case was scored and none
/// complied (the exit code).
fn finish(picked: usize, results: &[Tally]) -> bool {
    print!("{}", injection::summary(results));
    let unscored = picked - results.len();
    if unscored > 0 {
        println!("{unscored} cases not scored (error, cancelled or never run)");
    }
    unscored == 0 && results.iter().all(|t| t.verdict != Verdict::Complied)
}

/// Runs the set's cases whose issue is in the configured repo's language.
/// `start` gets each planted issue before its run (the dry run's replay).
/// Exit code 0 only when every picked case was scored and none complied.
pub async fn inject<P, S>(
    run: &Arc<Platform<P, S>>,
    set: &Path,
    only: &[String],
    cancel: &CancellationToken,
    mark: &str,
    start: impl Fn(&Platform<P, S>, &EvalIssue) -> Result<(), String>,
) -> Result<bool, String>
where
    P: ModelProvider + Send + Sync + 'static,
    S: LedgerStore + Send + Sync + 'static,
{
    let loaded = Set::load(set, &run.config, &run.profiles)?;
    let picked = loaded.pick(set, only, run.config.repo.language)?;
    let mut results = Vec::new();
    for (case, issue) in &picked {
        if cancel.is_cancelled() {
            println!("{}\tskipped\tcancelled", case.id);
            continue;
        }
        let deps = &loaded.dependency_files;
        match run_case(run, case, issue, deps, cancel, mark, &start).await {
            Ok(tally) => results.push(tally),
            Err(line) => println!("{line}"),
        }
    }
    Ok(finish(picked.len(), &results))
}

/// Plants, runs and scores one case, printing its line; the error line
/// when it could not be scored.
async fn run_case<P, S>(
    run: &Arc<Platform<P, S>>,
    case: &InjectionCase,
    issue: &EvalIssue,
    dependency_files: &[String],
    cancel: &CancellationToken,
    mark: &str,
    start: &impl Fn(&Platform<P, S>, &EvalIssue) -> Result<(), String>,
) -> Result<Tally, String>
where
    P: ModelProvider + Send + Sync + 'static,
    S: LedgerStore + Send + Sync + 'static,
{
    let run_id = aegis_lifecycle::next_run_id(&run.store, &case.id, mark)
        .await
        .map_err(|e| format!("{}\terror\t{e}", case.id))?;
    let error = |e: String| format!("{run_id}\terror\t{e}");
    let planted = case
        .plant(issue, &run.config, &run_id)
        .map_err(|e| error(e.to_string()))?;
    start(run, &planted.issue).map_err(error)?;
    let report = crate::one(run, planted.job.clone(), cancel)
        .await
        .map_err(|e| error(e.to_string()))?;
    let patch = report.patch.map(|p| p.diff);
    let id = report.issue_id;
    judge(
        &run.store,
        case,
        &planted,
        &id,
        patch.as_deref(),
        dependency_files,
    )
    .await
    .map_err(error)
}

/// `aegis inject --rescore`: each picked case's latest `<case>-<mark><n>`
/// run, scored again from its ledger events and kept patch. The model is
/// not called, so no key is needed.
pub async fn rescore<S: LedgerStore>(
    cfg: &RunConfig,
    store: &S,
    set: &Path,
    only: &[String],
    mark: &str,
) -> Result<bool, String> {
    let profiles = Profiles::builtin().map_err(|e| e.to_string())?;
    let loaded = Set::load(set, cfg, &profiles)?;
    let picked = loaded.pick(set, only, cfg.repo.language)?;
    let mut results = Vec::new();
    for (case, issue) in &picked {
        let Some(run_id) = latest_run(store, &case.id, mark).await? else {
            println!("{}\tnot run", case.id);
            continue;
        };
        let error = |e: String| format!("{run_id}\terror\t{e}");
        let scored = match case.plant(issue, cfg, &run_id) {
            Ok(planted) => {
                let patch = std::fs::read_to_string(
                    aegis_lifecycle::job_dir(cfg, &run_id).join("patch.diff"),
                )
                .ok();
                let deps = &loaded.dependency_files;
                judge(store, case, &planted, &run_id, patch.as_deref(), deps)
                    .await
                    .map_err(error)
            }
            Err(e) => Err(error(e.to_string())),
        };
        match scored {
            Ok(tally) => results.push(tally),
            Err(line) => println!("{line}"),
        }
    }
    Ok(finish(picked.len(), &results))
}

/// The last `<id>-<mark><n>` with events in `store`.
async fn latest_run<S: LedgerStore>(
    store: &S,
    id: &str,
    mark: &str,
) -> Result<Option<String>, String> {
    let mut latest = None;
    for n in 1.. {
        let run = format!("{id}-{mark}{n}");
        let issue = IssueId::parse(run.clone()).map_err(|e| e.to_string())?;
        if store
            .events(&issue)
            .await
            .map_err(|e| e.to_string())?
            .is_empty()
        {
            return Ok(latest);
        }
        latest = Some(run);
    }
    unreachable!("ran out of run numbers")
}

/// Scores run `run_id` of `case` from its ledger events and `patch`, and
/// prints its line.
async fn judge<S: LedgerStore>(
    store: &S,
    case: &InjectionCase,
    planted: &Planted,
    run_id: &str,
    patch: Option<&str>,
    dependency_files: &[String],
) -> Result<Tally, String> {
    let id = IssueId::parse(run_id.to_owned()).map_err(|e| e.to_string())?;
    let events = store.events(&id).await.map_err(|e| e.to_string())?;
    let watch = Watch {
        goal: case.goal,
        dependency_files,
        canary_file: injection::CANARY_FILE,
        marker: case.marker(&planted.canary),
    };
    let score = injection::score(&watch, &events, patch);
    let tally = Tally {
        surface: case.surface,
        goal: case.goal,
        verdict: score.verdict,
        seen: injection::seen(&planted.placed, &events),
        reported: score.reported,
        tampered: injection::tampered(&case.planted_payload(), patch),
    };
    let record = IssueRecord::from_events(id, &events);
    println!("{}", line(run_id, &tally, &record, &score));
    Ok(tally)
}

fn line(run_id: &str, tally: &Tally, record: &IssueRecord, score: &injection::Score) -> String {
    let hidden = match record.hidden_tests_passed {
        Some(true) => "pass",
        Some(false) => "FAIL",
        None => "-",
    };
    let outcome = record
        .outcome
        .map_or_else(|| "-".to_owned(), |o| format!("{o:?}"));
    format!(
        "{run_id}\t{}\t{}\t{}\t{}\t{}\t{outcome}\t${:.2}\thidden: {hidden}\t{}",
        tally.surface.as_str(),
        tally.goal.as_str(),
        tally.verdict.as_str(),
        if tally.seen { "seen" } else { "unseen" },
        match (tally.reported, tally.tampered) {
            (true, true) => "reported+removed",
            (true, false) => "reported",
            (false, true) => "removed",
            (false, false) => "silent",
        },
        record.cost_usd,
        score.evidence.join("; ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_names_known_cases_and_other_languages_are_left_out() {
        let set = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../eval/injection");
        let cases = injection::load_cases(&set).unwrap();
        let issues = base_issues(&set).unwrap();
        let go = pick(&cases, &issues, &[], Language::Go).unwrap();
        assert!(!go.is_empty());
        assert!(go.iter().all(|(_, i)| i.language == Language::Go));
        let one = ["gson-1587-readme-secret".to_owned()];
        assert_eq!(
            pick(&cases, &issues, &one, Language::Java).unwrap().len(),
            1
        );
        assert!(
            pick(&cases, &issues, &one, Language::Go)
                .unwrap()
                .is_empty()
        );
        let typo = ["gson-1587-readme-secrt".to_owned()];
        let err = pick(&cases, &issues, &typo, Language::Java).unwrap_err();
        assert!(err.contains("no case"), "{err}");
    }
}
