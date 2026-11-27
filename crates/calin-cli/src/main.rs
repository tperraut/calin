//! The `calin` binary: argument parsing and wiring only. This is the one
//! place that reads `CALIN_*` environment variables.

use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use calin_agents::{RegistryConfig, RunnerRegistry};
use calin_core::{ManagerConfig, SessionManager, Store, resolve_home};
use calin_server::{AppState, Assets, Server};
use clap::{Args, Parser, Subcommand};
use tracing_subscriber::EnvFilter;

const DEFAULT_BIND: &str = "127.0.0.1:4680";
/// Sets the log filter, in `tracing` syntax: `debug`, `calin_core=trace,info`.
const ENV_LOG: &str = "CALIN_LOG";
const DEFAULT_LOG: &str = "info";
/// Path of the GitHub CLI that opens and reads pull requests.
const ENV_GH_BIN: &str = "CALIN_GH_BIN";

#[derive(Parser)]
#[command(
    name = "calin",
    version,
    about = "A self-hosted orchestrator for coding agents"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the server (the default when no command is given)
    Serve(ServeArgs),
}

#[derive(Args)]
struct ServeArgs {
    /// Address and port to listen on; loopback only until sign-in exists
    #[arg(long, value_name = "ADDR:PORT", default_value = DEFAULT_BIND)]
    bind: String,
    /// Data directory [default: $CALIN_HOME, else ~/.calin]
    #[arg(long, value_name = "DIR")]
    home: Option<PathBuf>,
}

impl Default for ServeArgs {
    fn default() -> Self {
        Self {
            bind: DEFAULT_BIND.to_owned(),
            home: None,
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    init_logging();
    match cli.command {
        Some(Command::Serve(args)) => serve(args).await,
        None => serve(ServeArgs::default()).await,
    }
}

async fn serve(args: ServeArgs) -> Result<()> {
    let addr = parse_bind(&args.bind)?;
    let home = resolve_home(args.home)?;
    // Finds `claude` (or `CALIN_CLAUDE_BIN`) and makes it the default runner
    // outside dry run; the mock runner is always there.
    let registry = RunnerRegistry::new(RegistryConfig::from_env());
    let dry_run = registry.dry_run();
    let runners = registry.ids();
    match runners.split_first() {
        Some((default, _)) if !dry_run => {
            tracing::info!(runners = %runners.join(", "), %default, "runners available");
        }
        _ => tracing::info!("dry run: every agent runs on the mock runner"),
    }

    let store = Store::open(home.clone())
        .await
        .with_context(|| format!("cannot open the data directory {}", home.root().display()))?;
    let manager = SessionManager::start_with(store.clone(), registry, manager_config())
        .await
        .context("cannot recover the sessions of the previous run")?;
    let server = Server::bind(addr, AppState::new(manager.clone()))
        .await
        .with_context(|| format!("cannot listen on {addr}"))?;
    let addr = server.local_addr()?;
    if Assets::embedded().is_empty() {
        tracing::warn!("this binary has no web app in it; `make build` produces one that does");
    }

    println!("Calin is serving on http://{addr}");
    tracing::info!(%addr, home = %home.root().display(), dry_run, "server started");
    server.run(shutdown_signal()).await?;

    // Sessions still running are failed as "server restarted" on the next start.
    manager.shutdown().await;
    store.close().await;
    tracing::info!("server stopped");
    Ok(())
}

/// The defaults, with `gh` replaced by `CALIN_GH_BIN` when that is set.
fn manager_config() -> ManagerConfig {
    with_gh_bin(ManagerConfig::default(), std::env::var_os(ENV_GH_BIN))
}

fn with_gh_bin(mut config: ManagerConfig, gh_bin: Option<std::ffi::OsString>) -> ManagerConfig {
    // An empty value is how a shell spells "not set".
    if let Some(path) = gh_bin.filter(|path| !path.is_empty()) {
        config.gh_bin = PathBuf::from(path);
    }
    config
}

/// Parses `--bind` and refuses anything a remote machine could reach.
fn parse_bind(bind: &str) -> Result<SocketAddr> {
    let addr: SocketAddr = match bind.strip_prefix("localhost:") {
        Some(port) => {
            let port = port
                .parse()
                .with_context(|| format!("invalid port in --bind {bind}"))?;
            SocketAddr::from((Ipv4Addr::LOCALHOST, port))
        }
        None => bind.parse().with_context(|| {
            format!("invalid --bind {bind}: expected <addr:port>, for example {DEFAULT_BIND}")
        })?,
    };
    if !addr.ip().is_loopback() {
        bail!(
            "refusing to listen on {addr}: sign-in is not implemented yet, so Calin only \
             binds loopback addresses (for example {DEFAULT_BIND}). To reach it from another \
             machine, forward the port over SSH."
        );
    }
    Ok(addr)
}

fn init_logging() {
    let requested = std::env::var(ENV_LOG).unwrap_or_else(|_| DEFAULT_LOG.to_owned());
    let (filter, rejected) = match EnvFilter::try_new(&requested) {
        Ok(filter) => (filter, None),
        Err(err) => (EnvFilter::new(DEFAULT_LOG), Some(err)),
    };
    // Logs go to stderr; stdout carries only the serving URL.
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
    if let Some(err) = rejected {
        tracing::warn!(%err, "ignoring {ENV_LOG}={requested}; using {DEFAULT_LOG}");
    }
}

/// Completes on Ctrl-C, or on SIGTERM where that exists.
async fn shutdown_signal() {
    let interrupt = async {
        if tokio::signal::ctrl_c().await.is_err() {
            // Without a signal handler the only way out is to be killed.
            std::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = interrupt => {}
        () = terminate => {}
    }
    tracing::info!("shutting down");
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn the_command_line_is_well_formed() {
        Cli::command().debug_assert();
    }

    #[test]
    fn serve_is_the_default_command() {
        assert!(Cli::parse_from(["calin"]).command.is_none());
        let cli = Cli::parse_from(["calin", "serve"]);
        let Some(Command::Serve(args)) = cli.command else {
            panic!("expected serve");
        };
        assert_eq!(args.bind, DEFAULT_BIND);
        assert_eq!(args.home, None);
        assert_eq!(ServeArgs::default().bind, DEFAULT_BIND);

        let cli = Cli::parse_from(["calin", "serve", "--bind", "[::1]:9", "--home", "/tmp/x"]);
        let Some(Command::Serve(args)) = cli.command else {
            panic!("expected serve");
        };
        assert_eq!(args.bind, "[::1]:9");
        assert_eq!(args.home, Some(PathBuf::from("/tmp/x")));
    }

    #[test]
    fn the_github_cli_is_gh_unless_the_environment_names_another() {
        let default = ManagerConfig::default();
        assert_eq!(default.gh_bin, PathBuf::from("gh"));
        assert_eq!(with_gh_bin(default.clone(), None), default);
        assert_eq!(with_gh_bin(default.clone(), Some("".into())), default);
        let named = with_gh_bin(default.clone(), Some("/opt/gh/bin/gh".into()));
        assert_eq!(named.gh_bin, PathBuf::from("/opt/gh/bin/gh"));
        assert_eq!(named.setup_limit, default.setup_limit);
    }

    #[test]
    fn loopback_addresses_are_accepted() {
        for (bind, port) in [
            ("127.0.0.1:4680", 4680),
            ("127.0.0.1:0", 0),
            ("127.9.9.9:80", 80),
            ("[::1]:4680", 4680),
            ("localhost:8080", 8080),
        ] {
            let addr = parse_bind(bind).unwrap();
            assert!(addr.ip().is_loopback(), "{bind}");
            assert_eq!(addr.port(), port);
        }
    }

    #[test]
    fn other_addresses_are_refused_with_the_reason() {
        for bind in ["0.0.0.0:4680", "192.168.1.10:4680", "[::]:4680"] {
            let err = parse_bind(bind).unwrap_err().to_string();
            assert!(err.contains("sign-in is not implemented yet"), "{err}");
        }
        for bind in ["", "4680", "localhost", "localhost:http", "example.com:80"] {
            let err = parse_bind(bind).unwrap_err().to_string();
            assert!(err.contains("--bind"), "{err}");
        }
    }
}
