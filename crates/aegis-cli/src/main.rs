//! Command-line entry point: run one issue or a batch.
//!
//! ```text
//! aegis run   --config config/poc.toml --issue scrapy/scrapy#6123
//! aegis run   --config config/poc.toml --issue-file issue.md --id local-1
//! aegis batch --config config/poc.toml --issues issues.txt
//! ```
//!
//! `issues.txt` has one `owner/repo#number` per line (`#` comments allowed).
//! The fork token is read from the variable named by `repo.fork.token_env`.
//! Ctrl-C cancels the running issue; its ledger keeps what was spent.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use aegis_lang::Profiles;
use aegis_ledger::{JsonFileLedger, PriceTable};
use aegis_lifecycle::{IssueInput, IssueReport, LifecycleError, Platform, RunConfig, run_issue};
use aegis_provider::Provider;
use tokio_util::sync::CancellationToken;

const USAGE: &str = "usage:
  aegis run   --config <file> (--issue <owner/repo#number> | --issue-file <file> --id <id>)
  aegis batch --config <file> --issues <file>";

#[derive(Debug, Default)]
struct Args {
    command: String,
    config: Option<PathBuf>,
    issue: Option<String>,
    issue_file: Option<PathBuf>,
    id: Option<String>,
    issues: Option<PathBuf>,
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
    let prices = PriceTable::load(&config.run.pricing).map_err(|e| e.to_string())?;
    for model in [
        &config.models.light,
        &config.models.core,
        &config.models.expert,
    ] {
        prices.price(&model.id).map_err(|e| e.to_string())?;
    }
    let token_env = &config.repo.fork.token_env;
    let github_token =
        std::env::var(token_env).map_err(|_| format!("set {token_env} to a token for the fork"))?;
    Ok(Platform {
        store: JsonFileLedger::new(config.run.state_dir.join("ledger")),
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
async fn one(
    run: &Arc<Run>,
    issue: IssueInput,
    cancel: &CancellationToken,
) -> Result<IssueReport, LifecycleError> {
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
    format!(
        "{}\t{:?}\t${:.2}\t{} calls\t{end}",
        report.issue_id, report.outcome, report.cost_usd, report.model_calls
    )
}

async fn main_inner(args: Args) -> Result<bool, String> {
    if !["run", "batch"].contains(&args.command.as_str()) {
        return Err(format!("unknown command `{}`", args.command));
    }
    let config = args.config.as_deref().ok_or("--config is required")?;
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
        other => Err(format!("unknown command `{other}`")),
    }
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
}
