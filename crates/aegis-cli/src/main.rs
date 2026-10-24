//! Command-line entry point: run one issue or a batch.
//!
//! ```text
//! aegis run   --config config/poc.toml --issue scrapy/scrapy#6123
//! aegis run   --config config/poc.toml --issue-file issue.md --id local-1
//! aegis batch --config config/poc.toml --issues issues.txt
//! aegis eval  --config config/poc.toml --set eval/issues [--only scrapy-7604 …] [--no-pr]
//! aegis eval  --config config/poc.toml --set eval/issues --replay --no-pr
//! aegis review --config config/poc.toml --id scrapy-7604-r1 --verdict accepted --lines 0 --minutes 6
//! aegis report --config config/poc.toml [--set eval/issues]
//! aegis ledger import --config config/poc.toml [--from .workspace/state/ledger]
//! aegis ledger export --config config/poc.toml --to exported-ledger
//! ```
//!
//! `issues.txt` has one `owner/repo#number` per line (`#` comments allowed).
//! The fork token is read from the variable named by `repo.fork.token_env`.
//! Ctrl-C cancels the running issue; its ledger keeps what was spent.
//!
//! `--replay` is the dry run of #104: no model, each issue's upstream fix
//! written through the tools, runs `<id>-dry<n>` in their own ledger.
//! `--no-pr` stops where the PR would be opened, with or without `--replay`
//! (#111); the fork token is then not needed.
//!
//! `run.ledger = "postgres"` keeps the ledger of real runs in the database
//! named by `AEGIS_DATABASE_URL` (#22); dry runs keep their own JSON
//! ledger either way. `aegis ledger import|export` copies between the two.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use aegis_lang::Profiles;
use aegis_ledger::{JsonFileLedger, LedgerStore, Outcome, PriceTable};
use aegis_lifecycle::{
    EvalIssue, IssueInput, IssueReport, LifecycleError, Platform, Replay, ReplayProvider,
    RunConfig, run_issue,
};
use aegis_provider::{ModelProvider, Provider};

mod banner;
mod kpi;
mod ledger;
use ledger::Ledger;
use tokio_util::sync::CancellationToken;

const USAGE: &str = "usage:
  aegis run   --config <file> (--issue <owner/repo#number> | --issue-file <file> --id <id>)
  aegis batch --config <file> --issues <file>
  aegis eval  --config <file> --set <dir> [--only <id>]... [--replay] [--no-pr]
  aegis review --config <file> --id <run> --verdict accepted|accepted_with_edits|rejected
               (--lines <n> | --from-pr <url>) --minutes <m> [--broke-tests] [--replace]
  aegis report --config <file> [--set <dir>]
  aegis ledger import --config <file> [--from <dir>]
  aegis ledger export --config <file> --to <dir>";

#[derive(Debug, Default)]
struct Args {
    command: String,
    config: Option<PathBuf>,
    issue: Option<String>,
    issue_file: Option<PathBuf>,
    id: Option<String>,
    issues: Option<PathBuf>,
    set: Option<PathBuf>,
    only: Vec<String>,
    replay: bool,
    no_pr: bool,
    review: kpi::ReviewArgs,
    /// `import` or `export` after `aegis ledger`.
    action: Option<String>,
    from: Option<PathBuf>,
    to: Option<PathBuf>,
}

fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Args, String> {
    let first = args.next().ok_or("missing command")?;
    if first == "--help" || first == "-h" || first == "help" {
        return Ok(Args {
            command: "help".to_string(),
            ..Args::default()
        });
    }
    let mut parsed = Args {
        command: first,
        ..Args::default()
    };
    if parsed.command == "ledger" {
        parsed.action = Some(args.next().ok_or("`aegis ledger` needs import or export")?);
    }
    while let Some(flag) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--config" => parsed.config = Some(value()?.into()),
            "--issue" => parsed.issue = Some(value()?),
            "--issue-file" => parsed.issue_file = Some(value()?.into()),
            "--id" => parsed.id = Some(value()?),
            "--issues" => parsed.issues = Some(value()?.into()),
            "--set" => parsed.set = Some(value()?.into()),
            "--only" => parsed.only.push(value()?),
            "--replay" => parsed.replay = true,
            "--no-pr" => parsed.no_pr = true,
            "--verdict" => parsed.review.verdict = Some(value()?),
            "--lines" => parsed.review.lines = Some(value()?),
            "--minutes" => parsed.review.minutes = Some(value()?),
            "--from-pr" => parsed.review.from_pr = Some(value()?),
            "--broke-tests" => parsed.review.broke_tests = true,
            "--replace" => parsed.review.replace = true,
            "--from" => parsed.from = Some(value()?.into()),
            "--to" => parsed.to = Some(value()?.into()),
            _ => return Err(format!("unknown argument `{flag}`")),
        }
    }
    Ok(parsed)
}

/// `owner/repo#number`.
fn parse_issue_ref(text: &str) -> Result<(String, String, u64), String> {
    let bad = || format!("`{text}` is not owner/repo#number");
    let (repo, number) = text.trim().split_once('#').ok_or_else(bad)?;
    let (owner, name) = repo.split_once('/').ok_or_else(bad)?;
    let number = number.parse().map_err(|_| bad())?;
    if owner.is_empty() || name.is_empty() {
        return Err(bad());
    }
    Ok((owner.to_owned(), name.to_owned(), number))
}

type Run = Platform<Provider, Ledger>;

/// The model's platform; no fork token unless PRs are opened.
async fn platform(config_path: &Path, open_prs: bool) -> Result<Run, String> {
    let mut config = RunConfig::load(config_path).map_err(|e| e.to_string())?;
    config.run.open_prs = open_prs;
    let provider = Provider::from_config(&config.provider)
        .await
        .map_err(|e| e.to_string())?;
    let github_token = fork_token(&config)?;
    let store = ledger::open(&config).await?;
    finish_platform(config, provider, store, github_token)
}

/// Empty when the run opens no PR: nothing then talks to the fork.
fn fork_token(config: &RunConfig) -> Result<String, String> {
    if !config.run.open_prs {
        return Ok(String::new());
    }
    let token_env = &config.repo.fork.token_env;
    std::env::var(token_env).map_err(|_| format!("set {token_env} to a token for the fork"))
}

/// The dry run's platform: the replay provider, its own ledger, and no
/// fork token unless PRs are opened.
fn replay_platform(
    config_path: &Path,
    open_prs: bool,
) -> Result<Platform<ReplayProvider, JsonFileLedger>, String> {
    let mut config = RunConfig::load(config_path).map_err(|e| e.to_string())?;
    config.run.open_prs = open_prs;
    let github_token = fork_token(&config)?;
    let store = JsonFileLedger::new(config.run.state_dir.join("ledger-replay"));
    finish_platform(config, ReplayProvider::default(), store, github_token)
}

fn finish_platform<P, S>(
    config: RunConfig,
    provider: P,
    store: S,
    github_token: String,
) -> Result<Platform<P, S>, String> {
    // Before the first issue, so a missing ACI writes nothing to the ledger.
    config.check_aci_bin().map_err(|e| e.to_string())?;
    let prices = PriceTable::load(&config.run.pricing).map_err(|e| e.to_string())?;
    for model in [
        &config.models.light,
        &config.models.core,
        &config.models.expert,
    ] {
        prices.price(&model.id).map_err(|e| e.to_string())?;
    }
    Ok(Platform {
        store,
        config,
        provider,
        prices,
        profiles: Profiles::builtin().map_err(|e| e.to_string())?,
        github_token,
    })
}

async fn fetch(run: &Run, issue_ref: &str) -> Result<IssueInput, String> {
    let (owner, name, number) = parse_issue_ref(issue_ref)?;
    IssueInput::from_github(
        &run.config.repo.fork.api,
        &owner,
        &name,
        number,
        Some(&run.github_token),
    )
    .await
    .map_err(|e| e.to_string())
}

/// Runs one issue in its own task.
async fn one<P, S>(
    run: &Arc<Platform<P, S>>,
    issue: IssueInput,
    cancel: &CancellationToken,
) -> Result<IssueReport, LifecycleError>
where
    P: ModelProvider + Send + Sync + 'static,
    S: LedgerStore + Send + Sync + 'static,
{
    tokio::spawn(run_issue(Arc::clone(run), issue, cancel.clone()))
        .await
        .unwrap_or_else(|e| Err(LifecycleError::Config(format!("the job panicked: {e}"))))
}

/// One token for the whole command: Ctrl-C cancels the running issue and
/// any that would follow.
fn cancel_on_ctrl_c() -> CancellationToken {
    let cancel = CancellationToken::new();
    let token = cancel.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            eprintln!("aegis: cancelling…");
            token.cancel();
        }
    });
    cancel
}

fn line(report: &IssueReport) -> String {
    let end = match (&report.pr_url, &report.reason) {
        (Some(url), _) => url.clone(),
        (None, Some(reason)) => reason.clone(),
        (None, None) => String::new(),
    };
    let hidden = match report.hidden_tests_passed {
        Some(true) => "\thidden: pass",
        Some(false) => "\thidden: FAIL",
        None => "",
    };
    format!(
        "{}\t{:?}\t${:.2}\t{} calls{hidden}\t{end}",
        report.issue_id, report.outcome, report.cost_usd, report.model_calls
    )
}

/// `--replay` and `--no-pr` only change how `eval` ends; `run` and `batch`
/// always open their PR.
fn check_flags(args: &Args) -> Result<(), String> {
    if (args.replay || args.no_pr) && args.command != "eval" {
        return Err("--replay and --no-pr are for `aegis eval`".into());
    }
    Ok(())
}

async fn main_inner(mut args: Args) -> Result<bool, String> {
    if args.command == "help" {
        banner::print_help();
        return Ok(true);
    }
    if !["run", "batch", "eval", "review", "report", "ledger"].contains(&args.command.as_str()) {
        return Err(format!("unknown command `{}`", args.command));
    }
    // Reading and writing the ledger needs neither the model nor the fork.
    if let "review" | "report" | "ledger" = args.command.as_str() {
        let path = args.config.as_deref().ok_or("--config is required")?;
        let config = RunConfig::load(path).map_err(|e| e.to_string())?;
        let text = match (args.command.as_str(), args.action.as_deref()) {
            ("review", _) => {
                let mut review = std::mem::take(&mut args.review);
                review.id = args.id.clone();
                kpi::review(&config, &ledger::open(&config).await?, &review).await?
            }
            ("report", _) => {
                kpi::report(&ledger::open(&config).await?, args.set.as_deref()).await?
            }
            ("ledger", Some("import")) => {
                let from = args
                    .from
                    .clone()
                    .unwrap_or_else(|| ledger::json_dir(&config));
                ledger::import(&from).await?
            }
            ("ledger", Some("export")) => {
                ledger::export(args.to.as_deref().ok_or("--to is required")?).await?
            }
            (_, action) => {
                return Err(format!(
                    "`aegis ledger` takes import or export, not `{}`",
                    action.unwrap_or_default()
                ));
            }
        };
        println!("{text}");
        return Ok(true);
    }
    check_flags(&args)?;
    let config = args.config.as_deref().ok_or("--config is required")?;
    if args.replay {
        let run = Arc::new(replay_platform(config, !args.no_pr)?);
        return eval(&run, &args, &cancel_on_ctrl_c(), "dry", |run, issue| {
            let replay = Replay::of(issue, &run.config.repo.path).map_err(|e| e.to_string())?;
            run.provider.start(replay);
            Ok(())
        })
        .await;
    }
    let run = Arc::new(platform(config, !args.no_pr).await?);
    banner::print_dashboard(
        &config.display().to_string(),
        &run.config.repo.key,
        &run.config.repo.language.to_string(),
        &run.config.models.core.id,
    );
    let cancel = cancel_on_ctrl_c();
    match args.command.as_str() {
        "run" => {
            let issue = match (&args.issue, &args.issue_file, &args.id) {
                (Some(r), None, None) => fetch(&run, r).await?,
                (None, Some(file), Some(id)) => {
                    IssueInput::from_file(id, file).map_err(|e| e.to_string())?
                }
                _ => return Err("give --issue, or --issue-file with --id".into()),
            };
            let report = one(&run, issue, &cancel).await.map_err(|e| e.to_string())?;
            println!("{}", line(&report));
            Ok(report.pr_url.is_some())
        }
        "batch" => {
            let list = args.issues.as_deref().ok_or("--issues is required")?;
            let text = std::fs::read_to_string(list)
                .map_err(|e| format!("cannot read {}: {e}", list.display()))?;
            let mut all_ok = true;
            for entry in text
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
            {
                if cancel.is_cancelled() {
                    println!("{entry}\tskipped\tcancelled");
                    all_ok = false;
                    continue;
                }
                let result = match fetch(&run, entry).await {
                    Ok(issue) => one(&run, issue, &cancel).await.map_err(|e| e.to_string()),
                    Err(e) => Err(e),
                };
                match result {
                    Ok(report) => {
                        all_ok &= report.pr_url.is_some();
                        println!("{}", line(&report));
                    }
                    Err(e) => {
                        all_ok = false;
                        println!("{entry}\terror\t{e}");
                    }
                }
            }
            Ok(all_ok)
        }
        "eval" => eval(&run, &args, &cancel, "r", |_, _| Ok(())).await,
        other => Err(format!("unknown command `{other}`")),
    }
}

/// Runs the set's issues of the configured repo's language, one after the
/// other, each under a new run id. Exit code 0 when every run opened a PR
/// whose hidden tests pass.
async fn eval<P, S>(
    run: &Arc<Platform<P, S>>,
    args: &Args,
    cancel: &CancellationToken,
    mark: &str,
    before: impl Fn(&Platform<P, S>, &EvalIssue) -> Result<(), String>,
) -> Result<bool, String>
where
    P: ModelProvider + Send + Sync + 'static,
    S: LedgerStore + Send + Sync + 'static,
{
    let dir = args.set.as_deref().ok_or("--set is required")?;
    let set = aegis_lifecycle::load_set(dir).map_err(|e| e.to_string())?;
    let language = run.config.repo.language;
    let picked: Vec<_> = set
        .iter()
        .filter(|i| i.language == language)
        .filter(|i| args.only.is_empty() || args.only.contains(&i.id))
        .collect();
    if picked.is_empty() {
        return Err(format!("no {language} issue in {} matches", dir.display()));
    }
    let (mut prs, mut hidden_ok) = (0, 0);
    for issue in &picked {
        if cancel.is_cancelled() {
            println!("{}\tskipped\tcancelled", issue.id);
            continue;
        }
        let run_id = aegis_lifecycle::next_run_id(&run.store, &issue.id, mark)
            .await
            .map_err(|e| e.to_string())?;
        let job = issue.job(&run.config, &run_id).map_err(|e| e.to_string())?;
        if let Err(e) = before(run, issue) {
            println!("{run_id}\terror\t{e}");
            continue;
        }
        match one(run, job, cancel).await {
            Ok(report) => {
                // Dry runs without PRs end at the same point.
                prs += usize::from(report.outcome == Outcome::PrOpened);
                hidden_ok += usize::from(report.hidden_tests_passed == Some(true));
                println!("{}", line(&report));
            }
            Err(e) => println!("{run_id}\terror\t{e}"),
        }
    }
    println!(
        "{} issues: {prs} passed the gate, {hidden_ok} with hidden tests passing",
        picked.len()
    );
    Ok(hidden_ok == picked.len())
}

#[tokio::main]
async fn main() -> ExitCode {
    let mut raw_args = std::env::args().skip(1).peekable();
    if raw_args.peek().is_none() {
        banner::print_help();
        return ExitCode::SUCCESS;
    }
    let args = match parse_args(raw_args) {
        Ok(args) => args,
        Err(e) => {
            eprintln!("aegis: {e}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    match main_inner(args).await {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(e) => {
            eprintln!("aegis: {e}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_refs_and_arguments() {
        assert_eq!(
            parse_issue_ref("scrapy/scrapy#6123").unwrap(),
            ("scrapy".into(), "scrapy".into(), 6123)
        );
        for bad in ["scrapy#1", "a/b#x", "/b#1", "a/b"] {
            assert!(parse_issue_ref(bad).is_err(), "{bad}");
        }
        let args = parse_args(
            ["run", "--config", "c.toml", "--issue", "a/b#1"]
                .into_iter()
                .map(String::from),
        )
        .unwrap();
        assert_eq!(args.command, "run");
        assert_eq!(args.issue.as_deref(), Some("a/b#1"));
        assert!(parse_args(["run", "--bogus"].into_iter().map(String::from)).is_err());

        let args = parse_args(
            ["ledger", "export", "--config", "c.toml", "--to", "out"]
                .into_iter()
                .map(String::from),
        )
        .unwrap();
        assert_eq!(
            (args.command.as_str(), args.action.as_deref()),
            ("ledger", Some("export"))
        );
        assert_eq!(args.to.as_deref(), Some(Path::new("out")));
        assert!(parse_args(["ledger"].into_iter().map(String::from)).is_err());
    }

    #[test]
    fn no_pr_is_for_eval_with_or_without_replay() {
        let check =
            |argv: &[&str]| check_flags(&parse_args(argv.iter().map(|s| s.to_string())).unwrap());
        assert!(check(&["eval", "--no-pr"]).is_ok());
        assert!(check(&["eval", "--replay"]).is_ok());
        assert!(check(&["eval", "--replay", "--no-pr"]).is_ok());
        assert!(check(&["eval"]).is_ok());
        for bad in [["run", "--no-pr"], ["batch", "--replay"]] {
            assert!(check(&bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_missing_aci_stops_the_command_before_the_ledger() {
        let state = std::env::temp_dir().join(format!("aegis-no-aci-{}", std::process::id()));
        let poc = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/poc.toml");
        let mut config = RunConfig::load(&poc).unwrap();
        config.run.state_dir = state.clone();
        config.run.pricing =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/pricing.toml");
        let missing = state.join("no-such-aegis-aci");
        config.run.aci_bin = Some(missing.clone());
        let store = JsonFileLedger::new(state.join("ledger"));
        let err = finish_platform(
            config.clone(),
            ReplayProvider::default(),
            store,
            String::new(),
        )
        .err()
        .unwrap();
        assert!(err.contains(&missing.display().to_string()), "{err}");
        assert!(err.contains("`run.aci_bin`"), "{err}");
        assert!(err.contains("cargo build -p aegis-aci"), "{err}");
        assert!(!state.exists());

        // Any file will do: the check only looks for it.
        config.run.aci_bin = Some(std::env::current_exe().unwrap());
        assert!(config.check_aci_bin().is_ok());
    }

    #[test]
    fn the_poc_config_loads() {
        let config =
            RunConfig::load(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/poc.toml"))
                .unwrap();
        assert_eq!(config.repo.key, "scrapy");
        assert_eq!(config.models.core.id, "claude-sonnet-5-5");
        assert_eq!(config.run.ledger, aegis_lifecycle::LedgerBackend::Json);
        let profile = Profiles::builtin()
            .unwrap()
            .get(config.repo.language)
            .unwrap()
            .resolve(&config.repo.profile_overrides().unwrap())
            .unwrap();
        assert!(profile.gate.test.argv().contains(&"-n".to_owned()));
    }

    #[test]
    fn the_typst_config_loads() {
        let config =
            RunConfig::load(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/typst.toml"))
                .unwrap();
        assert_eq!(config.repo.key, "typst");
        assert_eq!(config.run.sandbox_env["CARGO_BUILD_JOBS"], "2");
        let profile = Profiles::builtin()
            .unwrap()
            .get(config.repo.language)
            .unwrap()
            .resolve(&config.repo.profile_overrides().unwrap())
            .unwrap();
        assert!(profile.fetch_hosts.contains(&"github.com".to_owned()));
        assert_eq!(
            profile.edit_format.command().unwrap().argv()[..3],
            ["rustfmt", "--edition", "2024"]
        );
        assert!(profile.edit_stdin);
        assert!(profile.gate.harness.is_some());
    }
}
