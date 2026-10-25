//! `aegis-server`: the API, the workers, or both (ADR 0003 §1).
//!
//! ```text
//! aegis-server --config config/server.toml [--role api|worker|all]
//! aegis-server --config <file> grant|revoke|token|revoke-tokens|revoke-sessions …
//! ```
//!
//! Secrets come from `[secrets]` (#30): AWS Secrets Manager, or the
//! environment for dev and tests. The database is the secret
//! `AEGIS_DATABASE_URL`; its schema is brought up to date on start. A
//! missing required secret stops the start, naming it. SIGTERM or Ctrl-C:
//! the API stops taking requests, the worker claims nothing more and
//! drains (`worker.drain_s`). Logs are JSON lines on stderr, filtered by
//! `RUST_LOG` (default `info`); each job's lines carry its id ([`logs`]).
//! Every role serves `/metrics` on `[metrics] listen` (#32).
//!
//! [`logs`]: aegis_server::logs
//!
//! Webhooks (#25) at `/webhooks/github` and `/webhooks/gitlab`, each open
//! once its secret is in the store: `AEGIS_GITHUB_WEBHOOK_SECRET`,
//! `AEGIS_GITLAB_WEBHOOK_TOKEN`; the trigger label is the variable
//! `AEGIS_WEBHOOK_LABEL` (`aegis`). Workers comment on a GitHub issue when
//! its job ends if the store has `AEGIS_GITHUB_COMMENT_TOKEN` (API root:
//! the variable `AEGIS_GITHUB_API`). Sign-in (#29): `[auth]` in the
//! configuration; the API needs the secrets `AEGIS_SESSION_KEY` and, with
//! `[auth.oidc]`, `AEGIS_OIDC_CLIENT_SECRET`.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use aegis_lifecycle::{ModeConfig, Modes};
use aegis_server::auth::{self, Auth};
use aegis_server::config::ServerConfig;
use aegis_server::lifecycle::LifecycleRunner;
use aegis_server::secrets::{self, DATABASE_URL};
use aegis_server::webhook::GitHubNotifier;
use aegis_server::{Role, api, metrics, worker};
use aegis_store::Store;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

const USAGE: &str = "usage: aegis-server --config <file> [--role api|worker|all]";

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
    let section = secrets::section(config.secrets.as_ref())?;
    let secrets = section.open(config.modes.customer_jp.is_some()).await?;
    secrets::load_at_start(&secrets, args.role).await?;
    if args.role.serves_api() {
        auth::load_at_start(&secrets, &config.auth).await?;
    }
    secrets::install(&secrets);
    let url = secrets
        .get(DATABASE_URL)
        .ok_or_else(|| format!("{DATABASE_URL}: not loaded"))?;
    let store = Store::connect(url.expose())
        .await
        .map_err(|e| format!("{DATABASE_URL}: {e}"))?
        .with_budget(config.budget_rules())
        .await
        .map_err(|e| format!("[budget]: {e}"))?;
    let shutdown = on_shutdown_signal();
    let mut parts: JoinSet<Result<(), String>> = JoinSet::new();
    let refresh = secrets::keep_fresh(
        secrets.clone(),
        store.clone(),
        section.refresh_every(),
        shutdown.clone(),
    );
    tokio::spawn(refresh);

    {
        let (store, shutdown) = (store.clone(), shutdown.clone());
        let listen = config.metrics.listen;
        parts.spawn(async move { metrics::serve(listen, store, shutdown).await });
    }
    if args.role.runs_jobs() {
        // Checked before the API opens: a worker that cannot run jobs is
        // a configuration error, not a silent queue.
        let runner = LifecycleRunner::new(store.clone(), modes(&config)?, secrets.clone()).await?;
        let notifier = Arc::new(GitHubNotifier::from_secrets(&secrets).await?);
        let worker_config = config.worker(worker_id());
        let (store, shutdown) = (store.clone(), shutdown.clone());
        parts.spawn(async move {
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
        let auth = Auth::new(store.clone(), config.auth.clone(), secrets.clone())?;
        let app = api::router(store.clone(), auth);
        let shutdown = shutdown.clone();
        parts.spawn(async move {
            // The peer's address for the limit of failed requests (#173).
            let app = app.into_make_service_with_connect_info::<std::net::SocketAddr>();
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

/// Roles, tokens and sessions ([`auth::cli`]): straight to the database,
/// whose URL comes from the configuration's secret store.
async fn run_auth_command(config: &std::path::Path, argv: &[String]) -> ExitCode {
    let done = async {
        let config = ServerConfig::load(config)?;
        let section = secrets::section(config.secrets.as_ref())?;
        let secrets = section.open(config.modes.customer_jp.is_some()).await?;
        secrets
            .require(&[DATABASE_URL])
            .await
            .map_err(|e| e.to_string())?;
        let url = secrets
            .get(DATABASE_URL)
            .ok_or_else(|| format!("{DATABASE_URL}: not loaded"))?;
        let store = Store::connect(url.expose())
            .await
            .map_err(|e| format!("{DATABASE_URL}: {e}"))?;
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
    let logs =
        aegis_server::logs::subscriber(std::io::stderr, aegis_server::logs::filter_from_env());
    if let Err(e) = tracing::subscriber::set_global_default(logs) {
        eprintln!("aegis-server: logs: {e}");
        return ExitCode::FAILURE;
    }
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if let ["--config", config, command, ..] =
        argv.iter().map(String::as_str).collect::<Vec<_>>()[..]
        && auth::cli::is_command(command)
    {
        return run_auth_command(std::path::Path::new(config), &argv[2..]).await;
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
