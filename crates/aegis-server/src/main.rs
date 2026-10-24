//! `aegis-server`: the API, the workers, or both (ADR 0003 §1).
//!
//! ```text
//! aegis-server --config config/server.toml [--role api|worker|all]
//! aegis-server grant|revoke|token|revoke-tokens|revoke-sessions …
//! ```
//!
//! The database is `AEGIS_DATABASE_URL`; its schema is brought up to date
//! on start. SIGTERM or Ctrl-C: the API stops taking requests, the worker
//! claims nothing more and drains (`worker.drain_s`). Logs are JSON lines
//! on stderr, filtered by `RUST_LOG` (default `info`).
//!
//! Webhooks (#25) at `/webhooks/github` and `/webhooks/gitlab`, each open
//! once its secret is set: `AEGIS_GITHUB_WEBHOOK_SECRET`,
//! `AEGIS_GITLAB_WEBHOOK_TOKEN`; the trigger label is `AEGIS_WEBHOOK_LABEL`
//! (`aegis`). Sign-in (#29): `[auth]` in the configuration, secrets from
//! `aegis_server::auth::load_secrets`. Workers comment on a GitHub issue when its job ends if
//! `AEGIS_GITHUB_COMMENT_TOKEN` is set (API root: `AEGIS_GITHUB_API`).

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use aegis_lifecycle::{ModeConfig, Modes};
use aegis_server::auth::{self, Auth};
use aegis_server::config::ServerConfig;
use aegis_server::lifecycle::LifecycleRunner;
use aegis_server::webhook::GitHubNotifier;
use aegis_server::{Role, api, worker};
use aegis_store::Store;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

const USAGE: &str = "usage: aegis-server --config <file> [--role api|worker|all]";
const DATABASE_URL_ENV: &str = "AEGIS_DATABASE_URL";

#[derive(Debug)]
struct Args {
    config: PathBuf,
    role: Role,
}

fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut config = None;
    let mut role = Role::All;
    while let Some(flag) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--config" => config = Some(PathBuf::from(value()?)),
            "--role" => {
                let text = value()?;
                role = Role::parse(&text).ok_or_else(|| format!("unknown role `{text}`"))?;
            }
            "--help" | "-h" => return Err(USAGE.into()),
            _ => return Err(format!("unknown argument `{flag}`")),
        }
    }
    Ok(Args {
        config: config.ok_or("--config is required")?,
        role,
    })
}

/// Unique per process, also across machines sharing the database.
fn worker_id() -> String {
    let host = std::env::var("HOSTNAME").unwrap_or_else(|_| "aegis".into());
    let started = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    format!("{host}-{}-{started}", std::process::id())
}

fn on_shutdown_signal() -> CancellationToken {
    let shutdown = CancellationToken::new();
    let token = shutdown.clone();
    tokio::spawn(async move {
        let terminate = async {
            #[cfg(unix)]
            {
                use tokio::signal::unix::{SignalKind, signal};
                match signal(SignalKind::terminate()) {
                    Ok(mut s) => {
                        s.recv().await;
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "cannot listen for SIGTERM");
                        std::future::pending::<()>().await;
                    }
                }
            }
            #[cfg(not(unix))]
            std::future::pending::<()>().await;
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            () = terminate => {}
        }
        tracing::info!("shutting down");
        token.cancel();
    });
    shutdown
}

/// The configuration of each mode in `[modes]`; the `customer-jp` guard
/// is checked here. Relative paths in them are taken from here.
fn modes(config: &ServerConfig) -> Result<Modes, String> {
    let cwd = std::env::current_dir().map_err(|e| format!("current directory: {e}"))?;
    let load = |path: &Option<PathBuf>| {
        path.as_deref()
            .map(|p| ModeConfig::load(p, &cwd).map_err(|e| format!("{}: {e}", p.display())))
            .transpose()
    };
    let modes = Modes::new(load(&config.modes.poc)?, load(&config.modes.customer_jp)?)
        .map_err(|e| e.to_string())?;
    if modes.poc.is_none() && modes.customer_jp.is_none() {
        return Err("[modes]: a worker needs `poc`, `customer_jp` or both".into());
    }
    Ok(modes)
}

async fn run(args: Args) -> Result<(), String> {
    let config = ServerConfig::load(&args.config)?;
    let url = std::env::var(DATABASE_URL_ENV)
        .map_err(|_| format!("set {DATABASE_URL_ENV} to the PostgreSQL URL"))?;
    let store = Store::connect(&url)
        .await
        .map_err(|e| format!("{DATABASE_URL_ENV}: {e}"))?;
    let shutdown = on_shutdown_signal();
    let mut parts: JoinSet<Result<(), String>> = JoinSet::new();

    if args.role.runs_jobs() {
        // Checked before the API opens: a worker that cannot run jobs is
        // a configuration error, not a silent queue.
        let runner = LifecycleRunner::new(store.clone(), modes(&config)?).await?;
        let worker_config = config.worker(worker_id());
        let (store, shutdown) = (store.clone(), shutdown.clone());
        parts.spawn(async move {
            let notifier = Arc::new(GitHubNotifier::from_env());
            worker::run_worker_notifying(
                store,
                Arc::new(runner),
                notifier,
                worker_config,
                shutdown,
            )
            .await;
            Ok(())
        });
    }
    if args.role.serves_api() {
        let listener = tokio::net::TcpListener::bind(config.api.listen)
            .await
            .map_err(|e| format!("listen on {}: {e}", config.api.listen))?;
        tracing::info!(listen = %config.api.listen, "API listening");
        let auth = Auth::new(store.clone(), config.auth.clone(), auth::load_secrets()?)?;
        let app = api::router(store.clone(), auth);
        let shutdown = shutdown.clone();
        parts.spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(shutdown.cancelled_owned())
                .await
                .map_err(|e| format!("API: {e}"))
        });
    }

    let mut result = Ok(());
    while let Some(done) = parts.join_next().await {
        let done = done.map_err(|e| format!("a part of the server panicked: {e}"))?;
        if let Err(e) = done {
            // One part failing stops the others.
            shutdown.cancel();
            result = Err(e);
        }
    }
    store.close().await;
    result
}

/// Roles, tokens and sessions ([`auth::cli`]): straight to the database.
async fn run_auth_command(argv: &[String]) -> ExitCode {
    let done = async {
        let url = std::env::var(DATABASE_URL_ENV)
            .map_err(|_| format!("set {DATABASE_URL_ENV} to the PostgreSQL URL"))?;
        let store = Store::connect(&url)
            .await
            .map_err(|e| format!("{DATABASE_URL_ENV}: {e}"))?;
        let out = auth::cli::run(&store, argv).await;
        store.close().await;
        out
    };
    match done.await {
        Ok(out) => {
            println!("{out}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("aegis-server: {e}");
            ExitCode::from(2)
        }
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .json()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.first().is_some_and(|a| auth::cli::is_command(a)) {
        return run_auth_command(&argv).await;
    }
    let args = match parse_args(argv.into_iter()) {
        Ok(args) => args,
        Err(e) => {
            eprintln!("aegis-server: {e}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    match run(args).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!(error = %e, "aegis-server stopped");
            eprintln!("aegis-server: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(text: &str) -> Result<Args, String> {
        parse_args(text.split_whitespace().map(String::from))
    }

    #[test]
    fn the_role_defaults_to_all() {
        let a = args("--config c.toml").unwrap();
        assert_eq!((a.config, a.role), (PathBuf::from("c.toml"), Role::All));
        assert_eq!(args("--config c --role worker").unwrap().role, Role::Worker);
        assert!(args("--config c --role web").is_err());
        assert!(args("--role api").is_err());
    }
}
