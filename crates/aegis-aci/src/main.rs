//! `aegis-aci --repo <dir> --language <id> [--overrides <file.toml>] [--parts <file.toml>]
//! [--format-image <image> [--docker <program>] [--format-mount <host>:<path>]...
//!  [--sandbox-memory-mb <n>] [--sandbox-cpus <n>] [--sandbox-pids <n>] [--sandbox-tmp-mb <n>]
//!  [--sandbox-runtime <name>]]
//! [--test-env <key>=<value>]... [--test-timeout <secs>] [--test-dir <dir>]
//! [--sidecars <file.toml>]`:
//! serves the ACI tools for
//! one repo over MCP on stdin/stdout. The overrides file holds the language
//! part of the repo's `RepoConfig` ([`ProfileOverrides`]); the parts file
//! the directories with a profile of their own (`[[part]]` with `dir`,
//! `language`, `overrides` and `mounts`, #71): a part's `mounts`
//! (`{ host, path }`, its dependency dirs) are mounted read-only in its
//! containers, as the root's `--format-mount`s are in the root's. With
//! `--format-image` the formatter of `edit` and the test of `run_tests` run
//! in that image, as the gate's steps do: no network, the mounts read-only,
//! the `--test-env` variables set, and within the `--sandbox-*` limits (the
//! run's `[run.sandbox_limits]`, #320; a profile's own memory and CPUs in
//! their place), on `--sandbox-runtime` when given (the run's `[run]
//! sandbox_runtime`, #519). `--test-dir` holds `run_tests`' logs. The sidecars file
//! holds the repo's services (`[[sidecar]]`, as `[[repo.sidecars]]`, #491):
//! with `--format-image`, `run_tests` reaches them as the gate's test steps
//! do, fresh for each run, and they are removed when the session ends.
//!
//! With `--listen <addr:port> --tls-cert <pem> --tls-key <pem>
//! --tls-client-ca <pem> [--allowed-host <name>]...` (#664) the same tools
//! are served to a remote worker over MCP Streamable HTTP at `/mcp`, only
//! to a client whose certificate chains to the client CA (mutual TLS);
//! `--allowed-host` names the host as the worker dials it.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use aegis_aci::{Aci, AciServer, FormatContainer, RUN_TESTS_TIMEOUT, TestSettings};
use aegis_gate::{Limits, Sandbox, SidecarSpec, Sidecars, check_sidecars};
use aegis_lang::{Language, ProfileOverrides, Profiles, RepoProfiles};
use rmcp::ServiceExt;
use serde::Deserialize;

const USAGE: &str = "usage: aegis-aci --repo <dir> --language <id> [--overrides <file.toml>]
       [--parts <file.toml>]
       [--format-image <image> [--docker <program>] [--format-mount <host>:<path>]...
        [--sandbox-memory-mb <n>] [--sandbox-cpus <n>] [--sandbox-pids <n>] [--sandbox-tmp-mb <n>]
        [--sandbox-runtime <name>]]
       [--test-env <key>=<value>]... [--test-timeout <secs>] [--test-dir <dir>]
       [--sidecars <file.toml>]
       [--listen <addr:port> --tls-cert <pem> --tls-key <pem> --tls-client-ca <pem>
        [--allowed-host <name>]...]";

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
    limits: Limits,
    /// The OCI runtime of the containers (#519); `None` is the daemon's.
    runtime: Option<String>,
    test_env: Vec<(String, String)>,
    test_timeout: Option<Duration>,
    test_dir: Option<PathBuf>,
    sidecars: Option<PathBuf>,
    /// Served over mutual TLS instead of stdio (#664).
    remote: Option<Remote>,
}

/// Where and how the tools are served to a remote worker (#664).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Remote {
    listen: String,
    tls: aegis_mtls::MtlsFiles,
    allowed_hosts: Vec<String>,
}

fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut parsed = Args::default();
    let (mut listen, mut cert, mut key, mut ca) = (None, None, None, None);
    let mut allowed_hosts = Vec::new();
    while let Some(flag) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--listen" => listen = Some(value()?),
            "--tls-cert" => cert = Some(PathBuf::from(value()?)),
            "--tls-key" => key = Some(PathBuf::from(value()?)),
            "--tls-client-ca" => ca = Some(PathBuf::from(value()?)),
            "--allowed-host" => allowed_hosts.push(value()?),
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
            "--sidecars" => parsed.sidecars = Some(value()?.into()),
            "--sandbox-memory-mb" => parsed.limits.memory_mb = number(&flag, &value()?)?,
            "--sandbox-cpus" => parsed.limits.cpus = number(&flag, &value()?)?,
            "--sandbox-pids" => parsed.limits.pids = number(&flag, &value()?)?,
            "--sandbox-tmp-mb" => parsed.limits.tmp_mb = number(&flag, &value()?)?,
            "--sandbox-runtime" => {
                let runtime = value()?;
                aegis_gate::check_runtime(&runtime)?;
                parsed.runtime = Some(runtime);
            }
            _ => return Err(format!("unknown argument `{flag}`")),
        }
    }
    let tls = aegis_mtls::MtlsFiles::from_flags(cert, key, ca)?;
    parsed.remote = match (listen, tls) {
        (None, None) if allowed_hosts.is_empty() => None,
        (Some(listen), Some(tls)) => Some(Remote {
            listen,
            tls,
            // The name it is dialed by; localhost for a local test.
            allowed_hosts: if allowed_hosts.is_empty() {
                vec!["localhost".into(), "127.0.0.1".into()]
            } else {
                allowed_hosts
            },
        }),
        // Never on the network without mutual TLS.
        _ => {
            return Err(
                "--listen needs --tls-cert, --tls-key and --tls-client-ca, and they need it".into(),
            );
        }
    };
    Ok(parsed)
}

/// A count above 0.
fn number<T: std::str::FromStr + Default + PartialEq>(flag: &str, text: &str) -> Result<T, String> {
    text.parse()
        .ok()
        .filter(|n| *n != T::default())
        .ok_or_else(|| format!("{flag} `{text}` is not a number above 0"))
}

/// One `[[part]]` of the parts file.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Part {
    dir: String,
    language: Language,
    #[serde(default)]
    overrides: ProfileOverrides,
    #[serde(default)]
    mounts: Vec<Mount>,
}

/// A host directory and where a part's containers see it (#198).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Mount {
    host: String,
    path: String,
}

/// The `--sidecars` file (#491).
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct SidecarsFile {
    #[serde(default)]
    sidecar: Vec<SidecarSpec>,
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
    let mounts = parts
        .part
        .into_iter()
        .map(|p| {
            let mounts = p.mounts.into_iter().map(|m| (m.host, m.path)).collect();
            (p.dir, mounts)
        })
        .collect();
    let aci = Aci::new(&repo, profiles)
        .map_err(|e| e.to_string())?
        .with_part_mounts(mounts);
    let defaults = TestSettings::default();
    let sidecars: Vec<SidecarSpec> = match &args.sidecars {
        Some(path) => read_toml::<SidecarsFile>(path)?.sidecar,
        None => vec![],
    };
    check_sidecars(&sidecars)?;
    let mut tests = TestSettings {
        sandbox: None,
        env: args.test_env,
        timeout: args.test_timeout.unwrap_or(RUN_TESTS_TIMEOUT),
        work_dir: args.test_dir.unwrap_or(defaults.work_dir),
        sidecars: None,
    };
    let aci = match (args.format_image, args.docker) {
        (Some(image), docker) => {
            let docker = docker.unwrap_or_else(|| "docker".into());
            if !sidecars.is_empty() {
                tests.sidecars = Some(Sidecars {
                    docker: docker.clone(),
                    specs: sidecars,
                });
            }
            let container = FormatContainer::new(docker, image, args.format_mounts, args.limits)
                .with_runtime(args.runtime);
            tests.sandbox = Some(test_sandbox(&container));
            aci.with_format_container(container)
        }
        (None, Some(_)) => return Err("--docker needs --format-image".into()),
        (None, None) if !sidecars.is_empty() => {
            return Err("--sidecars needs --format-image: they run in Docker".into());
        }
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
        // `run_tests` takes the part's (#320).
        hardening: container.hardening().clone(),
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let built = parse_args(std::env::args().skip(1))
        .and_then(|args| Ok((args.remote.clone(), build(args)?)));
    let (remote, aci) = match built {
        Ok(built) => built,
        Err(message) => {
            eprintln!("aegis-aci: {message}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    // The language server starts at the first find_definition /
    // find_references (#268), not here.
    let server = AciServer::new(aci);
    let code = match remote {
        None => serve_stdio(server.clone()).await,
        Some(remote) => serve_remote(server.clone(), remote).await,
    };
    // A test still running would keep its container after the client
    // has gone.
    server.stop_tests(STOP_GRACE);
    // Exit now: returning would wait for a tool still running on a
    // blocking thread (a formatter) after the client has gone.
    std::process::exit(code)
}

async fn serve_stdio(server: AciServer) -> i32 {
    let served = async {
        server
            .serve(rmcp::transport::stdio())
            .await?
            .waiting()
            .await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    };
    match served.await {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("aegis-aci: {e}");
            1
        }
    }
}

/// Serves a remote worker until SIGTERM or Ctrl-C (#664).
async fn serve_remote(server: AciServer, remote: Remote) -> i32 {
    let tls = match aegis_mtls::acceptor(&remote.tls) {
        Ok(tls) => tls,
        Err(e) => {
            eprintln!("aegis-aci: TLS: {e}");
            return 2;
        }
    };
    let listener = match tokio::net::TcpListener::bind(&remote.listen).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("aegis-aci: cannot listen on {}: {e}", remote.listen);
            return 1;
        }
    };
    eprintln!(
        "aegis-aci: serving https://{}{} over mutual TLS",
        remote.listen,
        aegis_aci::remote::MCP_PATH
    );
    let mut terminate =
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("aegis-aci: {e}");
                return 1;
            }
        };
    tokio::select! {
        served = aegis_aci::remote::serve_mtls(server, listener, tls, remote.allowed_hosts) => {
            match served {
                Ok(()) => 0,
                Err(e) => {
                    eprintln!("aegis-aci: {e}");
                    1
                }
            }
        }
        _ = tokio::signal::ctrl_c() => 0,
        _ = terminate.recv() => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Result<Args, String> {
        parse_args(list.iter().map(|a| (*a).to_owned()))
    }

    #[test]
    fn the_sandbox_runtime_is_taken_only_when_it_is_a_runtime_name() {
        assert_eq!(args(&[]).unwrap().runtime, None);
        assert_eq!(
            args(&["--sandbox-runtime", "runsc"])
                .unwrap()
                .runtime
                .as_deref(),
            Some("runsc")
        );
        assert!(args(&["--sandbox-runtime", "--privileged"]).is_err());
        assert!(args(&["--sandbox-runtime"]).is_err());
    }

    #[test]
    fn the_network_listener_needs_mutual_tls() {
        assert_eq!(args(&[]).unwrap().remote, None);
        let tls = [
            "--tls-cert",
            "/c",
            "--tls-key",
            "/k",
            "--tls-client-ca",
            "/ca",
        ];
        let remote = args(
            &[
                &["--listen", "0.0.0.0:7443"][..],
                &tls,
                &["--allowed-host", "exec.internal"],
            ]
            .concat(),
        )
        .unwrap()
        .remote
        .unwrap();
        assert_eq!(remote.listen, "0.0.0.0:7443");
        assert_eq!(remote.allowed_hosts, ["exec.internal"]);
        assert!(
            args(&["--listen", "0.0.0.0:7443"]).is_err(),
            "no TLS: refused"
        );
        assert!(args(&tls).is_err(), "TLS without a listener");
        assert!(args(&["--allowed-host", "x"]).is_err());
    }
}
