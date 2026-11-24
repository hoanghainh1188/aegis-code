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
//!
//! `--listen-tls <addr:port> --tls-cert <pem> --tls-key <pem>
//! --tls-client-ca <pem>` serves the same API to a remote worker over
//! mutual TLS (#664): only a client whose certificate chains to the
//! client CA is served, every call checked as on the socket. One of
//! `--listen` and `--listen-tls` is needed; both may be given.

use std::path::PathBuf;
use std::process::ExitCode;

use aegis_docker_proxy::{Policy, Proxy, bind};

const USAGE: &str = "usage: aegis-docker-proxy [--listen <socket>] [--upstream <socket>]
       [--listen-tls <addr:port> --tls-cert <pem> --tls-key <pem> --tls-client-ca <pem>]
       --mount-root <dir>... [--network <name>]... [--writable-image <image>]...
       [--sidecar-image <image>]...";

const DEFAULT_UPSTREAM: &str = "/var/run/docker.sock";

#[derive(Debug, Default, PartialEq, Eq)]
struct Args {
    listen: Option<PathBuf>,
    upstream: Option<PathBuf>,
    policy: Policy,
    listen_tls: Option<String>,
    tls: Option<aegis_mtls::MtlsFiles>,
}

fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut parsed = Args::default();
    let (mut cert, mut key, mut ca) = (None, None, None);
    while let Some(flag) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--listen" => parsed.listen = Some(value()?.into()),
            "--upstream" => parsed.upstream = Some(value()?.into()),
            "--mount-root" => parsed.policy.mount_roots.push(value()?.into()),
            "--network" => parsed.policy.networks.push(value()?),
            "--writable-image" => parsed.policy.writable_images.push(value()?),
            "--sidecar-image" => parsed.policy.sidecar_images.push(value()?),
            "--listen-tls" => parsed.listen_tls = Some(value()?),
            "--tls-cert" => cert = Some(PathBuf::from(value()?)),
            "--tls-key" => key = Some(PathBuf::from(value()?)),
            "--tls-client-ca" => ca = Some(PathBuf::from(value()?)),
            "--help" | "-h" => return Err(USAGE.into()),
            other => return Err(format!("unknown argument {other}\n{USAGE}")),
        }
    }
    parsed.tls = aegis_mtls::MtlsFiles::from_flags(cert, key, ca)?;
    if parsed.listen_tls.is_some() != parsed.tls.is_some() {
        return Err(format!(
            "--listen-tls needs --tls-cert, --tls-key and --tls-client-ca, and they need it\n{USAGE}"
        ));
    }
    if parsed.listen.is_none() && parsed.listen_tls.is_none() {
        return Err(format!("--listen or --listen-tls is required\n{USAGE}"));
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
    let proxy = Proxy {
        upstream: args.upstream.unwrap_or_else(|| DEFAULT_UPSTREAM.into()),
        policy: args.policy,
    };
    let local = match &args.listen {
        Some(path) => match bind(path) {
            Ok(l) => Some((path.clone(), l)),
            Err(e) => {
                eprintln!(
                    "aegis-docker-proxy: cannot listen on {}: {e}",
                    path.display()
                );
                return ExitCode::FAILURE;
            }
        },
        None => None,
    };
    let remote = match (&args.listen_tls, &args.tls) {
        (Some(addr), Some(files)) => {
            let tls = match aegis_mtls::acceptor(files) {
                Ok(tls) => tls,
                Err(e) => {
                    eprintln!("aegis-docker-proxy: TLS: {e}");
                    return ExitCode::FAILURE;
                }
            };
            match tokio::net::TcpListener::bind(addr).await {
                Ok(l) => Some((l, tls)),
                Err(e) => {
                    eprintln!("aegis-docker-proxy: cannot listen on {addr}: {e}");
                    return ExitCode::FAILURE;
                }
            }
        }
        _ => None,
    };
    tracing::info!(
        listen = ?args.listen,
        listen_tls = ?args.listen_tls,
        upstream = %proxy.upstream.display(),
        "serving"
    );
    let socket_path = local.as_ref().map(|(p, _)| p.clone());
    let serve_local = async {
        match local {
            Some((_, l)) => proxy.clone().serve(l).await,
            None => std::future::pending().await,
        }
    };
    let serve_remote = async {
        match remote {
            Some((l, tls)) => proxy.clone().serve_tls(l, tls).await,
            None => std::future::pending().await,
        }
    };
    tokio::select! {
        result = serve_local => {
            if let Err(e) = result {
                eprintln!("aegis-docker-proxy: {e}");
                return ExitCode::FAILURE;
            }
        }
        result = serve_remote => {
            if let Err(e) = result {
                eprintln!("aegis-docker-proxy: {e}");
                return ExitCode::FAILURE;
            }
        }
        _ = tokio::signal::ctrl_c() => {}
        _ = terminate() => {}
    }
    if let Some(path) = socket_path {
        let _ = std::fs::remove_file(&path);
    }
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
        assert_eq!(args.listen_tls, None);
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
        // #664: the TLS listener takes its three files, and only with them.
        let tls = parse(&[
            "--listen-tls",
            "0.0.0.0:2376",
            "--tls-cert",
            "/c",
            "--tls-key",
            "/k",
            "--tls-client-ca",
            "/ca",
            "--mount-root",
            "/w",
        ])
        .unwrap();
        assert_eq!(tls.listen, None);
        assert_eq!(tls.listen_tls.as_deref(), Some("0.0.0.0:2376"));
        assert_eq!(tls.tls.unwrap().ca, PathBuf::from("/ca"));
        for half in [
            &["--listen-tls", "0.0.0.0:2376", "--mount-root", "/w"][..],
            &[
                "--listen",
                "/s",
                "--tls-cert",
                "/c",
                "--tls-key",
                "/k",
                "--tls-client-ca",
                "/ca",
                "--mount-root",
                "/w",
            ],
            &[
                "--listen-tls",
                "0.0.0.0:2376",
                "--tls-cert",
                "/c",
                "--mount-root",
                "/w",
            ],
        ] {
            assert!(parse(half).is_err(), "{half:?}");
        }
        let unknown = parse(&["--listen", "/s", "--mount-root", "/w", "--x"]).unwrap_err();
        assert!(unknown.contains("unknown argument --x"), "{unknown}");
    }
}
