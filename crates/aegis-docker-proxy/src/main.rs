//! `aegis-docker-proxy --listen <socket> [--upstream <socket>]
//! --mount-root <dir>... [--network <name>]... [--writable-image <image>]...
//! [--sidecar-image <image>]...`:
//! serves the filtered Docker API on `--listen` for aegis, passing allowed
//! calls to the daemon on `--upstream` (`/var/run/docker.sock`). Bind
//! mounts must come from a `--mount-root` (the workspace); `--network`
//! adds a network containers may join with them (the Sonar scanner's), and
//! `--writable-image` an image that may run with a writable root (the
//! scanner's), and `--sidecar-image` an image a gate run's sidecar may run
//! (`postgres:16-alpine`, #490). Logs one line per refused call.

use std::path::PathBuf;
use std::process::ExitCode;

use aegis_docker_proxy::{Policy, Proxy, bind};

const USAGE: &str = "usage: aegis-docker-proxy --listen <socket> [--upstream <socket>]
       --mount-root <dir>... [--network <name>]... [--writable-image <image>]...
       [--sidecar-image <image>]...";

const DEFAULT_UPSTREAM: &str = "/var/run/docker.sock";

#[derive(Debug, Default, PartialEq, Eq)]
struct Args {
    listen: Option<PathBuf>,
    upstream: Option<PathBuf>,
    policy: Policy,
}

fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut parsed = Args::default();
    while let Some(flag) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--listen" => parsed.listen = Some(value()?.into()),
            "--upstream" => parsed.upstream = Some(value()?.into()),
            "--mount-root" => parsed.policy.mount_roots.push(value()?.into()),
            "--network" => parsed.policy.networks.push(value()?),
            "--writable-image" => parsed.policy.writable_images.push(value()?),
            "--sidecar-image" => parsed.policy.sidecar_images.push(value()?),
            "--help" | "-h" => return Err(USAGE.into()),
            other => return Err(format!("unknown argument {other}\n{USAGE}")),
        }
    }
    if parsed.listen.is_none() {
        return Err(format!("--listen is required\n{USAGE}"));
    }
    if parsed.policy.mount_roots.is_empty() {
        return Err(format!("at least one --mount-root is required\n{USAGE}"));
    }
    Ok(parsed)
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    let args = match parse_args(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(e) => {
            eprintln!("aegis-docker-proxy: {e}");
            return ExitCode::from(2);
        }
    };
    let listen = args.listen.unwrap_or_default();
    let listener = match bind(&listen) {
        Ok(l) => l,
        Err(e) => {
            eprintln!(
                "aegis-docker-proxy: cannot listen on {}: {e}",
                listen.display()
            );
            return ExitCode::FAILURE;
        }
    };
    let proxy = Proxy {
        upstream: args.upstream.unwrap_or_else(|| DEFAULT_UPSTREAM.into()),
        policy: args.policy,
    };
    tracing::info!(listen = %listen.display(), upstream = %proxy.upstream.display(), "serving");
    tokio::select! {
        result = proxy.serve(listener) => {
            if let Err(e) = result {
                eprintln!("aegis-docker-proxy: {e}");
                return ExitCode::FAILURE;
            }
        }
        _ = tokio::signal::ctrl_c() => {}
        _ = terminate() => {}
    }
    let _ = std::fs::remove_file(&listen);
    ExitCode::SUCCESS
}

/// SIGTERM, which `docker stop` and systemd send.
async fn terminate() {
    match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
        Ok(mut s) => {
            s.recv().await;
        }
        Err(_) => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Args, String> {
        parse_args(args.iter().map(|a| (*a).to_owned()))
    }

    #[test]
    fn reads_the_policy_from_its_flags() {
        let args = parse(&[
            "--listen",
            "/run/aegis-docker/docker.sock",
            "--mount-root",
            "/srv/aegis/.workspace",
            "--network",
            "sonar",
            "--writable-image",
            "sonarsource/sonar-scanner-cli",
            "--sidecar-image",
            "postgres:16-alpine",
        ])
        .unwrap();
        assert_eq!(args.listen, Some("/run/aegis-docker/docker.sock".into()));
        assert_eq!(args.upstream, None);
        assert_eq!(
            args.policy,
            Policy {
                mount_roots: vec!["/srv/aegis/.workspace".into()],
                networks: vec!["sonar".into()],
                writable_images: vec!["sonarsource/sonar-scanner-cli".into()],
                sidecar_images: vec!["postgres:16-alpine".into()],
            }
        );
    }

    #[test]
    fn needs_a_socket_and_a_mount_root() {
        let no_root = parse(&["--listen", "/s"]).unwrap_err();
        assert!(no_root.contains("--mount-root"), "{no_root}");
        let no_listen = parse(&["--mount-root", "/w"]).unwrap_err();
        assert!(no_listen.contains("--listen"), "{no_listen}");
        let unknown = parse(&["--listen", "/s", "--mount-root", "/w", "--x"]).unwrap_err();
        assert!(unknown.contains("unknown argument --x"), "{unknown}");
    }
}
