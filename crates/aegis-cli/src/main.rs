//! Command-line entry point: run one issue or a batch.
//!
//! ```text
//! aegis run   --config config/poc.toml --issue scrapy/scrapy#6123
//! aegis run   --config config/poc.toml --issue-file issue.md --id local-1
//! aegis batch --config config/poc.toml --issues issues.txt
//! aegis eval  --config config/poc.toml --set eval/issues [--only scrapy-7604 …]
//! aegis eval  --config config/poc.toml --set eval/issues --replay --no-pr
//! aegis review --config config/poc.toml --id scrapy-7604-r1 --verdict accepted --lines 0 --minutes 6
//! aegis report --config config/poc.toml [--set eval/issues]
//! ```
//!
//! `issues.txt` has one `owner/repo#number` per line (`#` comments allowed).
//! The fork token is read from the variable named by `repo.fork.token_env`.
//! Ctrl-C cancels the running issue; its ledger keeps what was spent.
//!
//! `--replay` is the dry run of #104: no model, each issue's upstream fix
//! written through the tools, runs `<id>-dry<n>` in their own ledger.
//! `--no-pr` stops where the PR would be opened.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use aegis_lang::Profiles;
use aegis_ledger::{JsonFileLedger, Outcome, PriceTable};
use aegis_lifecycle::{
    EvalIssue, IssueInput, IssueReport, LifecycleError, Platform, Replay, ReplayProvider,
    RunConfig, run_issue,
};
use aegis_provider::{ModelProvider, Provider};

mod kpi;
use tokio_util::sync::CancellationToken;

const USAGE: &str = "usage:
  aegis run   --config <file> (--issue <owner/repo#number> | --issue-file <file> --id <id>)
  aegis batch --config <file> --issues <file>
  aegis eval  --config <file> --set <dir> [--only <id>]... [--replay] [--no-pr]
  aegis review --config <file> --id <run> --verdict accepted|accepted_with_edits|rejected
               (--lines <n> | --from-pr <url>) --minutes <m> [--broke-tests] [--replace]
  aegis report --config <file> [--set <dir>]";

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
}

fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut parsed = Args {
        command: args.next().ok_or("missing command")?,
        ..Args::default()
    };
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

type Run = Platform<Provider, JsonFileLedger>;

async fn platform(config_path: &Path) -> Result<Run, String> {
    let config = RunConfig::load(config_path).map_err(|e| e.to_string())?;
    let provider = Provider::from_config(&config.provider)
        .await
        .map_err(|e| e.to_string())?;
    let github_token = fork_token(&config)?;
    let store = JsonFileLedger::new(config.run.state_dir.join("ledger"));
    finish_platform(config, provider, store, github_token)
}

fn fork_token(config: &RunConfig) -> Result<String, String> {
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
    let github_token = if open_prs {
        fork_token(&config)?
    } else {
        String::new()
    };
    let store = JsonFileLedger::new(config.run.state_dir.join("ledger-replay"));
    finish_platform(config, ReplayProvider::default(), store, github_token)
}

fn finish_platform<P>(
    config: RunConfig,
    provider: P,
    store: JsonFileLedger,
    github_token: String,
) -> Result<Platform<P, JsonFileLedger>, String> {
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
async fn one<P>(
    run: &Arc<Platform<P, JsonFileLedger>>,
    issue: IssueInput,
    cancel: &CancellationToken,
) -> Result<IssueReport, LifecycleError>
where
    P: ModelProvider + Send + Sync + 'static,
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

async fn main_inner(mut args: Args) -> Result<bool, String> {
    if !["run", "batch", "eval", "review", "report"].contains(&args.command.as_str()) {
        return Err(format!("unknown command `{}`", args.command));
    }
    // Reading and writing the ledger needs neither the model nor the fork.
    if let "review" | "report" = args.command.as_str() {
        let path = args.config.as_deref().ok_or("--config is required")?;
        let config = RunConfig::load(path).map_err(|e| e.to_string())?;
        let text = if args.command == "review" {
            let mut review = std::mem::take(&mut args.review);
            review.id = args.id.clone();
            kpi::review(&config, &review).await?
        } else {
            kpi::report(&kpi::store(&config), args.set.as_deref()).await?
        };
        println!("{text}");
        return Ok(true);
    }
    let config = args.config.as_deref().ok_or("--config is required")?;
    if args.replay || args.no_pr {
        if args.command != "eval" || !args.replay {
            return Err("--replay and --no-pr are for `aegis eval --replay`".into());
        }
        let run = Arc::new(replay_platform(config, !args.no_pr)?);
        return eval(&run, &args, &cancel_on_ctrl_c(), "dry", |run, issue| {
            let replay = Replay::of(issue, &run.config.repo.path).map_err(|e| e.to_string())?;
            run.provider.start(replay);
            Ok(())
        })
        .await;
    }
    let run = Arc::new(platform(config).await?);
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
async fn eval<P>(
    run: &Arc<Platform<P, JsonFileLedger>>,
    args: &Args,
    cancel: &CancellationToken,
    mark: &str,
    before: impl Fn(&Platform<P, JsonFileLedger>, &EvalIssue) -> Result<(), String>,
) -> Result<bool, String>
where
    P: ModelProvider + Send + Sync + 'static,
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
    let args = match parse_args(std::env::args().skip(1)) {
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
    }

    #[test]
    fn the_poc_config_loads() {
        let config =
            RunConfig::load(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/poc.toml"))
                .unwrap();
        assert_eq!(config.repo.key, "scrapy");
        assert_eq!(config.models.core.id, "claude-sonnet-5-5");
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
        assert!(profile.gate.harness.is_some());
    }
}
