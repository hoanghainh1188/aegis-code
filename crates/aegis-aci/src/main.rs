//! `aegis-aci --repo <dir> --language <id> [--overrides <file.toml>] [--parts <file.toml>]
//! [--format-image <image> [--docker <program>] [--format-mount <host>:<path>]...]
//! [--test-env <key>=<value>]... [--test-timeout <secs>] [--test-dir <dir>]`:
//! serves the ACI tools for
//! one repo over MCP on stdin/stdout. The overrides file holds the language
//! part of the repo's `RepoConfig` ([`ProfileOverrides`]); the parts file
//! the directories with a profile of their own (`[[part]]` with `dir`,
//! `language` and `overrides`, #71). With
//! `--format-image` the formatter of `edit` and the test of `run_tests` run
//! in that image, as the gate's steps do: no network, the mounts read-only,
//! the `--test-env` variables set. `--test-dir` holds `run_tests`' logs.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use aegis_aci::{Aci, AciServer, FormatContainer, RUN_TESTS_TIMEOUT, TestSettings};
use aegis_gate::Sandbox;
use aegis_lang::{Language, ProfileOverrides, Profiles, RepoProfiles};
use rmcp::ServiceExt;
use serde::Deserialize;

const USAGE: &str = "usage: aegis-aci --repo <dir> --language <id> [--overrides <file.toml>]
       [--parts <file.toml>]
       [--format-image <image> [--docker <program>] [--format-mount <host>:<path>]...]
       [--test-env <key>=<value>]... [--test-timeout <secs>] [--test-dir <dir>]";

/// How long `run_tests` waits for a test still running to stop when the
/// client goes.
const STOP_GRACE: Duration = Duration::from_secs(5);

#[derive(Debug, Default)]
struct Args {
    repo: Option<PathBuf>,
    language: Option<String>,
    overrides: Option<PathBuf>,
    parts: Option<PathBuf>,
    format_image: Option<String>,
    docker: Option<String>,
    format_mounts: Vec<(String, String)>,
    test_env: Vec<(String, String)>,
    test_timeout: Option<Duration>,
    test_dir: Option<PathBuf>,
}

fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut parsed = Args::default();
    while let Some(flag) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--repo" => parsed.repo = Some(value()?.into()),
            "--language" => parsed.language = Some(value()?),
            "--overrides" => parsed.overrides = Some(value()?.into()),
            "--parts" => parsed.parts = Some(value()?.into()),
            "--format-image" => parsed.format_image = Some(value()?),
            "--docker" => parsed.docker = Some(value()?),
            "--format-mount" => {
                let text = value()?;
                let (host, inside) = text
                    .rsplit_once(':')
                    .ok_or_else(|| format!("--format-mount `{text}` is not <host>:<path>"))?;
                parsed
                    .format_mounts
                    .push((host.to_owned(), inside.to_owned()));
            }
            "--test-env" => {
                let text = value()?;
                let (key, val) = text
                    .split_once('=')
                    .filter(|(k, _)| !k.is_empty())
                    .ok_or_else(|| format!("--test-env `{text}` is not <key>=<value>"))?;
                parsed.test_env.push((key.to_owned(), val.to_owned()));
            }
            "--test-timeout" => {
                let text = value()?;
                let secs: u64 = text
                    .parse()
                    .map_err(|_| format!("--test-timeout `{text}` is not a number of seconds"))?;
                parsed.test_timeout = Some(Duration::from_secs(secs));
            }
            "--test-dir" => parsed.test_dir = Some(value()?.into()),
            _ => return Err(format!("unknown argument `{flag}`")),
        }
    }
    Ok(parsed)
}

/// One `[[part]]` of the parts file.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Part {
    dir: String,
    language: Language,
    #[serde(default)]
    overrides: ProfileOverrides,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct PartsFile {
    #[serde(default)]
    part: Vec<Part>,
}

fn read_toml<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    toml::from_str(&text).map_err(|e| format!("invalid {}: {e}", path.display()))
}

fn build(args: Args) -> Result<Aci, String> {
    let repo = args.repo.ok_or("--repo is required")?;
    let id = args.language.ok_or("--language is required")?;
    let language = Language::ALL
        .into_iter()
        .find(|l| l.id() == id)
        .ok_or_else(|| format!("unknown language `{id}`"))?;
    let overrides: ProfileOverrides = match &args.overrides {
        Some(path) => read_toml(path)?,
        None => ProfileOverrides::default(),
    };
    let parts: PartsFile = match &args.parts {
        Some(path) => read_toml(path)?,
        None => PartsFile::default(),
    };
    let builtin = Profiles::builtin().map_err(|e| e.to_string())?;
    let resolve = |language: Language, overrides: &ProfileOverrides| {
        builtin
            .get(language)
            .and_then(|p| p.resolve(overrides))
            .map_err(|e| e.to_string())
    };
    let dirs = parts
        .part
        .iter()
        .map(|p| Ok((p.dir.clone(), resolve(p.language, &p.overrides)?)))
        .collect::<Result<Vec<_>, String>>()?;
    let profiles =
        RepoProfiles::new(resolve(language, &overrides)?, dirs).map_err(|e| e.to_string())?;
    let aci = Aci::new(&repo, profiles).map_err(|e| e.to_string())?;
    let defaults = TestSettings::default();
    let mut tests = TestSettings {
        sandbox: None,
        env: args.test_env,
        timeout: args.test_timeout.unwrap_or(RUN_TESTS_TIMEOUT),
        work_dir: args.test_dir.unwrap_or(defaults.work_dir),
    };
    let aci = match (args.format_image, args.docker) {
        (Some(image), docker) => {
            let container = FormatContainer {
                docker: docker.unwrap_or_else(|| "docker".into()),
                image,
                mounts: args.format_mounts,
            };
            tests.sandbox = Some(test_sandbox(&container));
            aci.with_format_container(container)
        }
        (None, Some(_)) => return Err("--docker needs --format-image".into()),
        (None, None) => aci,
    };
    Ok(aci.with_test_settings(tests))
}

/// The gate's step container (`aegis_gate::Sandbox`), from the same image
/// and mounts as the formatter's; the `--test-env` variables are added per
/// run.
fn test_sandbox(container: &FormatContainer) -> Sandbox {
    Sandbox {
        docker: container.docker.clone(),
        image: container.image.clone(),
        mounts: container
            .mounts
            .iter()
            .map(|(host, inside)| (PathBuf::from(host), inside.clone()))
            .collect(),
        env: Vec::new(),
        network: None,
        read_only_mounts: true,
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let mut aci = match parse_args(std::env::args().skip(1)).and_then(build) {
        Ok(aci) => aci,
        Err(message) => {
            eprintln!("aegis-aci: {message}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    // It indexes while the agent reads; a server that cannot start leaves
    // its two tools answering why.
    if let Err(e) = aci.start_language_server() {
        eprintln!("aegis-aci: {e}");
    }
    let server = AciServer::new(aci);
    let served = async {
        server
            .clone()
            .serve(rmcp::transport::stdio())
            .await?
            .waiting()
            .await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    };
    let code = match served.await {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("aegis-aci: {e}");
            1
        }
    };
    // A test still running would keep its container after the client
    // has gone.
    server.stop_tests(STOP_GRACE);
    // Exit now: returning would wait for a tool still running on a
    // blocking thread (a formatter) after the client has gone.
    std::process::exit(code)
}
