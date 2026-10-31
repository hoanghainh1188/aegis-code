//! The commands of `aegis`. Each prints to `out` and says how the
//! process ends; `--json` prints the server's data as it came.

use std::io::{BufRead, Write};
use std::time::Duration;

use crate::api::{Client, Job};
use crate::config::{self, Login};

pub const USAGE: &str = "\
aegis: give aegis-code jobs and follow them, from your terminal.

  aegis login <server>          log in with a token from <server>/me/tokens
  aegis logout
  aegis whoami
  aegis repos                   the repos you have a role in
  aegis submit <repo> <issue>   a job for an issue: owner/repo#12 (GitHub),
                                group/project#12 (--source gitlab), PROJ-7
                                (--source jira), …
  aegis list [--repo <repo>] [--status <status>] [--limit <n>]
  aegis show <job>              the job and its runs
  aegis watch <job> [--every <seconds>]
                                until it ends; exit 0 when it opened a PR
  aegis cancel <job>
  aegis retry <job>
  aegis review <job>            a review round of the PR the job opened
  aegis open <job>              its PR, or its page on the dashboard

  --json                        print the server's answer as JSON
";

/// How the process ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    Ok = 0,
    /// The job ended without a PR (`watch`).
    NoPr = 1,
    Error = 2,
}

/// The value after `name` in `args`, taken out.
fn take_opt(args: &mut Vec<String>, name: &str) -> Result<Option<String>, String> {
    let Some(i) = args.iter().position(|a| a == name) else {
        return Ok(None);
    };
    if i + 1 >= args.len() {
        return Err(format!("{name} needs a value"));
    }
    let value = args.remove(i + 1);
    args.remove(i);
    Ok(Some(value))
}

fn take_flag(args: &mut Vec<String>, name: &str) -> bool {
    match args.iter().position(|a| a == name) {
        Some(i) => {
            args.remove(i);
            true
        }
        None => false,
    }
}

fn job_id(text: &str) -> Result<i64, String> {
    text.trim_start_matches('#')
        .parse()
        .map_err(|_| format!("`{text}` is not a job number"))
}

/// The source an issue ref is from when `--source` is not given: GitHub
/// for `owner/repo#n`, Jira for `KEY-n`.
fn guess_source(issue_ref: &str) -> Result<&'static str, String> {
    let github = issue_ref.split_once('#').is_some_and(|(repo, n)| {
        repo.split('/').count() == 2 && n.bytes().all(|b| b.is_ascii_digit()) && !n.is_empty()
    });
    let jira = issue_ref.split_once('-').is_some_and(|(key, n)| {
        !key.is_empty()
            && key
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
            && !n.is_empty()
            && n.bytes().all(|b| b.is_ascii_digit())
    });
    match (github, jira) {
        (true, _) => Ok("github"),
        (_, true) => Ok("jira"),
        _ => Err(format!(
            "which tracker is `{issue_ref}` from? Say it with --source github|gitlab|jira|backlog"
        )),
    }
}

fn line(job: &Job) -> String {
    let what = match (job.outcome.as_deref(), &job.pr_url) {
        (_, Some(url)) => format!("PR {url}"),
        (Some(outcome), None) => outcome.to_owned(),
        (None, None) => String::new(),
    };
    let round = if job.review_round > 0 {
        format!(
            " (review round {} of #{})",
            job.review_round,
            job.review_of.unwrap_or(0)
        )
    } else {
        String::new()
    };
    format!(
        "#{:<6} {:<9} {}{round}  {what}",
        job.id, job.status, job.issue_ref
    )
    .trim_end()
    .to_owned()
}

fn print_json(out: &mut dyn Write, value: &impl serde::Serialize) -> Result<(), String> {
    let text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    writeln!(out, "{text}").map_err(|e| e.to_string())
}

/// Reads the token without showing it when stdin is a terminal.
fn read_token(
    input: &mut dyn BufRead,
    out: &mut dyn Write,
    interactive: bool,
) -> Result<String, String> {
    if interactive {
        let _ = write!(out, "Token (from /me/tokens on the dashboard): ");
        let _ = out.flush();
    }
    let hide = interactive && cfg!(unix);
    if hide {
        let _ = std::process::Command::new("stty").arg("-echo").status();
    }
    let mut token = String::new();
    let read = input.read_line(&mut token);
    if hide {
        let _ = std::process::Command::new("stty").arg("echo").status();
        let _ = writeln!(out);
    }
    read.map_err(|e| e.to_string())?;
    let token = token.trim().to_owned();
    if token.is_empty() {
        return Err("no token given".into());
    }
    Ok(token)
}

/// Opens `url` with the desktop's handler.
fn open_url(url: &str) -> Result<(), String> {
    let program = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(windows) {
        "explorer"
    } else {
        "xdg-open"
    };
    std::process::Command::new(program)
        .arg(url)
        .status()
        .map_err(|e| format!("{program}: {e}"))
        .map(drop)
}

/// Runs `args` (without the program's name).
pub fn run(
    mut args: Vec<String>,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
    interactive: bool,
) -> Result<Exit, String> {
    let json = take_flag(&mut args, "--json");
    let Some(command) = args.first().cloned() else {
        write!(out, "{USAGE}").map_err(|e| e.to_string())?;
        return Ok(Exit::Error);
    };
    let rest: Vec<String> = args[1..].to_vec();
    let w = |out: &mut dyn Write, text: String| writeln!(out, "{text}").map_err(|e| e.to_string());
    match command.as_str() {
        "help" | "--help" | "-h" => {
            write!(out, "{USAGE}").map_err(|e| e.to_string())?;
            Ok(Exit::Ok)
        }
        "login" => {
            let [server] = rest.as_slice() else {
                return Err("usage: aegis login <server>".into());
            };
            let server = config::checked_server(server)?;
            let token = read_token(input, out, interactive)?;
            let login = Login { server, token };
            let me = Client::new(login.clone())?.me()?;
            let path = config::path()?;
            config::save(&path, &login)?;
            w(
                out,
                format!(
                    "logged in to {} as {} ({})",
                    login.server,
                    me.name,
                    path.display()
                ),
            )?;
            Ok(Exit::Ok)
        }
        "logout" => {
            let path = config::path()?;
            match std::fs::remove_file(&path) {
                Ok(()) => w(out, format!("logged out ({} removed)", path.display()))?,
                Err(_) => w(out, "not logged in".into())?,
            }
            Ok(Exit::Ok)
        }
        _ => {
            let client = Client::new(config::load()?)?;
            run_with(&client, &command, rest, json, out)
        }
    }
}

/// A command that needs the login, with `client` (tests give their own).
pub fn run_with(
    client: &Client,
    command: &str,
    mut rest: Vec<String>,
    json: bool,
    out: &mut dyn Write,
) -> Result<Exit, String> {
    let w = |out: &mut dyn Write, text: String| writeln!(out, "{text}").map_err(|e| e.to_string());
    let one_job = |rest: &[String]| -> Result<i64, String> {
        match rest {
            [id] => job_id(id),
            _ => Err(format!("usage: aegis {command} <job>")),
        }
    };
    match command {
        "whoami" => {
            let me = client.me()?;
            let admin = if me.admin { " (admin)" } else { "" };
            w(out, format!("{}{admin} on {}", me.name, client.server()))?;
        }
        "repos" => {
            let repos = client.repos()?;
            if json {
                print_json(out, &serde_json::to_value(repos.iter().map(|r| {
                    serde_json::json!({ "id": r.id, "slug": r.slug, "url": r.url,
                        "default_branch": r.default_branch, "language": r.language, "role": r.role })
                }).collect::<Vec<_>>()).map_err(|e| e.to_string())?)?;
            } else if repos.is_empty() {
                w(
                    out,
                    "no repo: ask an admin for a role (`aegis-server grant`)".into(),
                )?;
            } else {
                for r in repos {
                    w(
                        out,
                        format!("{:<20} {:<11} {:<10} {}", r.slug, r.role, r.language, r.url),
                    )?;
                }
            }
        }
        "submit" => {
            let source = take_opt(&mut rest, "--source")?;
            let [repo, issue_ref] = rest.as_slice() else {
                return Err("usage: aegis submit <repo> <issue> [--source <tracker>]".into());
            };
            let source = match source {
                Some(s) => s,
                None => guess_source(issue_ref)?.to_owned(),
            };
            let repo = client.repo(repo)?;
            let job = client.submit(repo.id, issue_ref, &source)?;
            if json {
                return print_json(out, &job_json(&job)).map(|()| Exit::Ok);
            }
            w(
                out,
                format!(
                    "job #{} queued for {} in {}",
                    job.id, job.issue_ref, repo.slug
                ),
            )?;
            w(out, format!("follow it: aegis watch {}", job.id))?;
        }
        "list" => {
            let repo = take_opt(&mut rest, "--repo")?;
            let status = take_opt(&mut rest, "--status")?;
            let limit = take_opt(&mut rest, "--limit")?
                .map(|l| {
                    l.parse::<u32>()
                        .map_err(|_| format!("--limit `{l}` is not a number"))
                })
                .transpose()?
                .unwrap_or(20);
            let repo_id = repo
                .map(|slug| client.repo(&slug).map(|r| r.id))
                .transpose()?;
            let jobs = client.jobs(repo_id, status.as_deref(), limit)?;
            if json {
                return print_json(out, &jobs.iter().map(job_json).collect::<Vec<_>>())
                    .map(|()| Exit::Ok);
            }
            if jobs.is_empty() {
                w(out, "no job".into())?;
            }
            for job in &jobs {
                w(out, line(job))?;
            }
        }
        "show" => {
            let id = one_job(&rest)?;
            let job = client.job(id)?;
            let runs = client.runs(id)?;
            if json {
                let runs: Vec<_> = runs
                    .iter()
                    .map(|r| {
                        serde_json::json!({
                    "attempt": r.attempt, "status": r.status, "error": r.error,
                    "started_at": r.started_at, "ended_at": r.ended_at })
                    })
                    .collect();
                return print_json(
                    out,
                    &serde_json::json!({ "job": job_json(&job), "runs": runs }),
                )
                .map(|()| Exit::Ok);
            }
            w(out, line(&job))?;
            w(
                out,
                format!(
                    "source {} · attempts {} · created {} · updated {}",
                    job.source, job.attempts, job.created_at, job.updated_at
                ),
            )?;
            if let Some(reason) = &job.reason {
                w(out, format!("reason: {reason}"))?;
            }
            for r in &runs {
                let error = r
                    .error
                    .as_deref()
                    .map_or(String::new(), |e| format!("  {e}"));
                w(
                    out,
                    format!(
                        "  run {}: {} from {} to {}{error}",
                        r.attempt,
                        r.status,
                        r.started_at,
                        r.ended_at.as_deref().unwrap_or("…")
                    ),
                )?;
            }
            w(
                out,
                format!("dashboard: {}/jobs/{}", client.server(), job.id),
            )?;
        }
        "watch" => {
            let every = take_opt(&mut rest, "--every")?
                .map(|s| {
                    s.parse::<u64>()
                        .map_err(|_| format!("--every `{s}` is not a number of seconds"))
                })
                .transpose()?
                .unwrap_or(10)
                .max(1);
            let id = one_job(&rest)?;
            let mut last = String::new();
            loop {
                let job = client.job(id)?;
                let now = line(&job);
                if now != last {
                    w(out, now.clone())?;
                    last = now;
                }
                if job.ended() {
                    if let Some(reason) = &job.reason {
                        w(out, format!("reason: {reason}"))?;
                    }
                    return Ok(if job.pr_url.is_some() {
                        Exit::Ok
                    } else {
                        Exit::NoPr
                    });
                }
                std::thread::sleep(Duration::from_secs(every));
            }
        }
        "cancel" | "retry" | "review" => {
            let id = one_job(&rest)?;
            let job = match command {
                "cancel" => client.cancel(id)?,
                "retry" => client.retry(id)?,
                _ => client.review(id)?,
            };
            if json {
                return print_json(out, &job_json(&job)).map(|()| Exit::Ok);
            }
            let done = match command {
                "cancel" if job.status == "cancelled" => format!("job #{} cancelled", job.id),
                "cancel" => format!("job #{}: cancel asked; it stops at its next check", job.id),
                "retry" => format!("job #{} queued to run #{id} again", job.id),
                _ => format!(
                    "job #{} queued: review round {} of #{id}'s PR",
                    job.id, job.review_round
                ),
            };
            w(out, done)?;
        }
        "open" => {
            let id = one_job(&rest)?;
            let job = client.job(id)?;
            let url = job
                .pr_url
                .clone()
                .unwrap_or_else(|| format!("{}/jobs/{}", client.server(), job.id));
            w(out, url.clone())?;
            open_url(&url)?;
        }
        other => return Err(format!("unknown command `{other}`\n\n{USAGE}")),
    }
    Ok(Exit::Ok)
}

fn job_json(job: &Job) -> serde_json::Value {
    serde_json::json!({
        "id": job.id, "repo_id": job.repo_id, "issue_ref": job.issue_ref,
        "source": job.source, "status": job.status, "outcome": job.outcome,
        "reason": job.reason, "pr_url": job.pr_url, "attempts": job.attempts,
        "retry_of": job.retry_of, "review_of": job.review_of,
        "review_round": job.review_round, "created_at": job.created_at,
        "updated_at": job.updated_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tracker_is_guessed_from_the_issue_ref_or_asked_for() {
        assert_eq!(guess_source("typst/typst#12"), Ok("github"));
        assert_eq!(guess_source("APP-7"), Ok("jira"));
        for unclear in ["group/sub/app#3", "typst#12", "app-7", "12"] {
            assert!(guess_source(unclear).is_err(), "{unclear}");
        }
    }

    #[test]
    fn options_are_taken_out_of_the_arguments() {
        let mut args: Vec<String> = ["a", "--repo", "demo", "b"].map(String::from).to_vec();
        assert_eq!(take_opt(&mut args, "--repo"), Ok(Some("demo".into())));
        assert_eq!(args, ["a", "b"]);
        assert!(take_opt(&mut vec!["--repo".into()], "--repo").is_err());
        assert_eq!(job_id("#42"), Ok(42));
        assert!(job_id("x").is_err());
    }
}
