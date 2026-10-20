//! `aegis-aci --repo <dir> --language <id> [--overrides <file.toml>]`:
//! serves the ACI tools for one repo over MCP on stdin/stdout. The
//! overrides file holds the language part of the repo's `RepoConfig`
//! ([`ProfileOverrides`]).

use std::path::PathBuf;
use std::process::ExitCode;

use aegis_aci::{Aci, AciServer};
use aegis_lang::{Language, ProfileOverrides, Profiles};
use rmcp::ServiceExt;

const USAGE: &str = "usage: aegis-aci --repo <dir> --language <id> [--overrides <file.toml>]";

#[derive(Debug, Default)]
struct Args {
    repo: Option<PathBuf>,
    language: Option<String>,
    overrides: Option<PathBuf>,
}

fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut parsed = Args::default();
    while let Some(flag) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--repo" => parsed.repo = Some(value()?.into()),
            "--language" => parsed.language = Some(value()?),
            "--overrides" => parsed.overrides = Some(value()?.into()),
            _ => return Err(format!("unknown argument `{flag}`")),
        }
    }
    Ok(parsed)
}

fn build(args: Args) -> Result<Aci, String> {
    let repo = args.repo.ok_or("--repo is required")?;
    let id = args.language.ok_or("--language is required")?;
    let language = Language::ALL
        .into_iter()
        .find(|l| l.id() == id)
        .ok_or_else(|| format!("unknown language `{id}`"))?;
    let overrides = match args.overrides {
        Some(path) => {
            let text = std::fs::read_to_string(&path)
                .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
            toml::from_str::<ProfileOverrides>(&text)
                .map_err(|e| format!("invalid {}: {e}", path.display()))?
        }
        None => ProfileOverrides::default(),
    };
    let profile = Profiles::builtin()
        .and_then(|p| p.get(language)?.resolve(&overrides))
        .map_err(|e| e.to_string())?;
    Aci::new(&repo, profile).map_err(|e| e.to_string())
}

#[tokio::main]
async fn main() -> ExitCode {
    let aci = match parse_args(std::env::args().skip(1)).and_then(build) {
        Ok(aci) => aci,
        Err(message) => {
            eprintln!("aegis-aci: {message}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let served = async {
        AciServer::new(aci)
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
    // Exit now: returning would wait for a tool still running on a
    // blocking thread (a formatter) after the client has gone.
    std::process::exit(code)
}
