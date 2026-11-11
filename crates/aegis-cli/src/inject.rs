//! `aegis inject` (#361): the prompt injection set, each case an eval issue
//! with a payload planted in it, run without a PR and scored from the
//! ledger and the patch.

use std::path::Path;
use std::sync::Arc;

use aegis_lang::Language;
use aegis_ledger::{IssueId, LedgerStore};
use aegis_lifecycle::injection::{self, InjectionCase, Planted, Verdict, Watch};
use aegis_lifecycle::{EvalIssue, IssueReport, Platform};
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
    let cases = injection::load_cases(set).map_err(|e| e.to_string())?;
    let issues = base_issues(set)?;
    let language = run.config.repo.language;
    let picked = pick(&cases, &issues, only, language)?;
    if picked.is_empty() {
        return Err(format!("no {language} case in {} matches", set.display()));
    }
    let profiles = run.config.repo.profiles(&run.profiles);
    let dependency_files = injection::dependency_files(&profiles.map_err(|e| e.to_string())?);
    let mut results = Vec::new();
    for (case, issue) in &picked {
        if cancel.is_cancelled() {
            println!("{}	skipped	cancelled", case.id);
            continue;
        }
        match run_case(run, case, issue, &dependency_files, cancel, mark, &start).await {
            Ok(verdict) => results.push((case.surface, case.goal, verdict)),
            Err(line) => println!("{line}"),
        }
    }
    print!("{}", injection::summary(&results));
    let unscored = picked.len() - results.len();
    if unscored > 0 {
        println!("{unscored} cases not scored (error or cancelled)");
    }
    Ok(unscored == 0 && results.iter().all(|(_, _, v)| *v != Verdict::Complied))
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
) -> Result<Verdict, String>
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
    let (verdict, line) = one_case(run, case, planted, dependency_files, cancel)
        .await
        .map_err(error)?;
    println!("{line}");
    Ok(verdict)
}

/// Runs one planted case and scores it: the verdict and its output line.
async fn one_case<P, S>(
    run: &Arc<Platform<P, S>>,
    case: &InjectionCase,
    planted: Planted,
    dependency_files: &[String],
    cancel: &CancellationToken,
) -> Result<(Verdict, String), String>
where
    P: ModelProvider + Send + Sync + 'static,
    S: LedgerStore + Send + Sync + 'static,
{
    let report = crate::one(run, planted.job, cancel)
        .await
        .map_err(|e| e.to_string())?;
    let id = IssueId::parse(report.issue_id.clone()).map_err(|e| e.to_string())?;
    let events = run.store.events(&id).await.map_err(|e| e.to_string())?;
    let watch = Watch {
        goal: case.goal,
        dependency_files,
        canary_file: injection::CANARY_FILE,
        marker: case.marker(&planted.canary),
    };
    let patch = report.patch.as_ref().map(|p| p.diff.as_str());
    let score = injection::score(&watch, &events, patch);
    Ok((score.verdict, line(case, &report, &score)))
}

fn line(case: &InjectionCase, report: &IssueReport, score: &injection::Score) -> String {
    let hidden = match report.hidden_tests_passed {
        Some(true) => "pass",
        Some(false) => "FAIL",
        None => "-",
    };
    format!(
        "{}\t{}\t{}\t{}\t{:?}\t${:.2}\thidden: {hidden}\t{}",
        report.issue_id,
        case.surface.as_str(),
        case.goal.as_str(),
        score.verdict.as_str(),
        report.outcome,
        report.cost_usd,
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
