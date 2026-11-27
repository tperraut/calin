//! The `calin` binary: argument parsing and wiring only. This is the one
//! place that reads `CALIN_*` environment variables; those of sign-in are
//! read here through `calin-auth`'s one reader.

use std::ffi::OsString;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};
use calin_agents::{RegistryConfig, RunnerRegistry};
use calin_auth::{AuthMode, ConfigError, LoopbackHttp, SystemClock};
use calin_core::{ManagerConfig, SessionManager, Store, resolve_home};
use calin_push::EndpointPolicy;
use calin_server::{AUTH_FILE, AppState, Assets, Push, PushSetup, Server, SignIn, vapid_subject};
use clap::{Args, Parser, Subcommand};

mod home_cmd;
mod home_init;
mod server_cmd;
use tracing_subscriber::EnvFilter;

const DEFAULT_BIND: &str = "127.0.0.1:4680";
/// Sets the log filter, in `tracing` syntax: `debug`, `calin_core=trace,info`.
const ENV_LOG: &str = "CALIN_LOG";
const DEFAULT_LOG: &str = "info";
/// Path of the GitHub CLI that opens and reads pull requests.
const ENV_GH_BIN: &str = "CALIN_GH_BIN";
/// Development only, and unsafe anywhere else: set to `1`, the public URL
/// and the sign-in provider may be plain `http` on `localhost` or
/// `127.0.0.1`. Browser tests need it; a real server never does.
const ENV_UNSAFE_HTTP: &str = "CALIN_UNSAFE_DEV_ALLOW_HTTP_LOOPBACK";
/// Public mode, optional: a `mailto:` address push services can reach the
/// operator at. The public URL is the contact otherwise.
const ENV_PUSH_CONTACT: &str = "CALIN_PUSH_CONTACT";

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
    /// Install Calin as a service on a Linux server, or verify an installation
    #[command(subcommand)]
    Server(server_cmd::ServerCommand),
    /// Set up a home repo: the repository every session starts from
    #[command(subcommand)]
    Home(home_cmd::HomeCommand),
}

#[derive(Args)]
struct ServeArgs {
    /// Address and port to listen on; loopback only (a reverse proxy makes it public)
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
        Some(Command::Server(command)) => server_cmd::run(command).await,
        Some(Command::Home(command)) => {
            home_cmd::run(command, &format!("http://{DEFAULT_BIND}")).await
        }
        None => serve(ServeArgs::default()).await,
    }
}

async fn serve(args: ServeArgs) -> Result<()> {
    let addr = parse_bind(&args.bind)?;
    // Before anything is opened or listens: a server that was meant to ask
    // for sign-in must not come up without it.
    let mode = read_auth_mode(std::env::var_os(ENV_UNSAFE_HTTP), |loopback_http| {
        AuthMode::from_process_environment(loopback_http)
    })?;
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
    let (sign_in, push) = match mode {
        AuthMode::Local => (None, None),
        AuthMode::Public(config) => {
            let public_url = config.public_origin().to_owned();
            let setup = push_setup(
                &public_url,
                config.loopback_http(),
                std::env::var_os(ENV_PUSH_CONTACT),
            )?;
            if config.loopback_http() == LoopbackHttp::AllowForDevelopment {
                tracing::warn!(
                    "{ENV_UNSAFE_HTTP} is set: plain http is accepted for loopback URLs. \
                     This is for development only."
                );
            }
            let auth_file = home.root().join(AUTH_FILE);
            let sign_in = SignIn::open(config, auth_file, Arc::new(SystemClock))
                .await
                .context("cannot set up sign-in")?;
            // Next to `auth.json`. A corrupt key file stops the server here,
            // naming the file: replacing it would orphan every subscription.
            let push = Push::open(home.root(), setup, Arc::new(calin_push::SystemClock))
                .await
                .context("cannot set up push notifications")?;
            tracing::info!(%public_url, "public mode: sign-in is required");
            (Some(Arc::new(sign_in)), Some(Arc::new(push)))
        }
    };
    let manager = SessionManager::start_with(store.clone(), registry, manager_config())
        .await
        .context("cannot recover the sessions of the previous run")?;
    let mut state = AppState::new(manager.clone());
    if let Some(sign_in) = &sign_in {
        state = state.with_sign_in(sign_in.clone());
    }
    if let Some(push) = push {
        state = state.with_push(push);
    }
    let server = Server::bind(addr, state)
        .await
        .with_context(|| format!("cannot listen on {addr}"))?;
    let addr = server.local_addr()?;
    if Assets::embedded().is_empty() {
        tracing::warn!("this binary has no web app in it; `make build` produces one that does");
    }
    if let Some(sign_in) = sign_in {
        // Not awaited: a provider that is slow or down must not delay serving.
        tokio::spawn(async move { sign_in.warm_up().await });
    }

    println!("Calin is serving on http://{addr}");
    tracing::info!(%addr, home = %home.root().display(), dry_run, "server started");
    // Returns once the sign-in store is flushed.
    server.run(shutdown_signal()).await?;

    // Sessions still running are failed as "server restarted" on the next start.
    manager.shutdown().await;
    store.close().await;
    tracing::info!("server stopped");
    Ok(())
}

/// Reads the mode through `read`, which is `calin-auth`'s reader of the
/// environment, with the development switch applied. Every problem found
/// is in the error, one per line.
fn read_auth_mode(
    switch: Option<OsString>,
    read: impl FnOnce(LoopbackHttp) -> Result<AuthMode, ConfigError>,
) -> Result<AuthMode> {
    let mut problems = Vec::new();
    // An empty value is how a shell spells "not set". Anything but `1` is
    // refused: a switch that turns a check off is not guessed at.
    let loopback_http = match switch.filter(|value| !value.is_empty()) {
        None => LoopbackHttp::Refuse,
        Some(value) if value == "1" => LoopbackHttp::AllowForDevelopment,
        Some(_) => {
            problems.push(format!("{ENV_UNSAFE_HTTP} must be 1 or not set"));
            LoopbackHttp::Refuse
        }
    };
    let mode = match read(loopback_http) {
        Ok(mode) => Some(mode),
        Err(error) => {
            problems.extend(error.problems().iter().map(ToString::to_string));
            None
        }
    };
    match mode {
        Some(mode) if problems.is_empty() => Ok(mode),
        _ => Err(anyhow!(
            "sign-in is not configured correctly, so the server did not start:\n  - {}",
            problems.join("\n  - ")
        )),
    }
}

/// How push notifications are sent: the VAPID contact, and which endpoints
/// may be called. The development switch also lets a fake push service on
/// `http://127.0.0.1:<port>` be used, as it lets the provider be.
fn push_setup(
    public_origin: &str,
    loopback_http: LoopbackHttp,
    contact: Option<OsString>,
) -> Result<PushSetup> {
    // An empty value is how a shell spells "not set".
    let contact = contact
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string_lossy().into_owned());
    let subject = vapid_subject(public_origin, contact.as_deref())
        .map_err(|problem| anyhow!("{ENV_PUSH_CONTACT}: {problem}"))?;
    let endpoint_policy = match loopback_http {
        LoopbackHttp::Refuse => EndpointPolicy::PublicHttpsOnly,
        LoopbackHttp::AllowForDevelopment => EndpointPolicy::AllowLoopbackHttpForDevelopment,
    };
    Ok(PushSetup {
        subject,
        endpoint_policy,
    })
}

/// The defaults, with `gh` replaced by `CALIN_GH_BIN` when that is set.
fn manager_config() -> ManagerConfig {
    with_gh_bin(ManagerConfig::default(), std::env::var_os(ENV_GH_BIN))
}

fn with_gh_bin(mut config: ManagerConfig, gh_bin: Option<OsString>) -> ManagerConfig {
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
            "refusing to listen on {addr}: Calin only binds loopback addresses (for example \
             {DEFAULT_BIND}), with or without sign-in. To reach it from another machine, put a \
             reverse proxy in front of it and set CALIN_PUBLIC_URL, or forward the port over SSH."
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
    fn push_uses_the_public_origin_or_the_contact_and_only_public_endpoints() {
        let setup = push_setup("https://calin.example.com", LoopbackHttp::Refuse, None).unwrap();
        assert_eq!(setup.subject, "https://calin.example.com");
        assert_eq!(setup.endpoint_policy, EndpointPolicy::PublicHttpsOnly);
        let setup = push_setup(
            "https://calin.example.com",
            LoopbackHttp::Refuse,
            Some("".into()),
        )
        .unwrap();
        assert_eq!(setup.subject, "https://calin.example.com");
        let setup = push_setup(
            "https://calin.example.com",
            LoopbackHttp::Refuse,
            Some("mailto:ops@example.com".into()),
        )
        .unwrap();
        assert_eq!(setup.subject, "mailto:ops@example.com");
        let error = push_setup(
            "https://calin.example.com",
            LoopbackHttp::Refuse,
            Some("ops@example.com".into()),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains(ENV_PUSH_CONTACT), "{error}");
        let dev = push_setup(
            "http://localhost:4680",
            LoopbackHttp::AllowForDevelopment,
            None,
        )
        .unwrap();
        assert_eq!(
            dev.endpoint_policy,
            EndpointPolicy::AllowLoopbackHttpForDevelopment
        );
        assert!(dev.subject.starts_with("mailto:"), "{}", dev.subject);
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
            assert!(err.contains("only binds loopback addresses"), "{err}");
        }
        for bind in ["", "4680", "localhost", "localhost:http", "example.com:80"] {
            let err = parse_bind(bind).unwrap_err().to_string();
            assert!(err.contains("--bind"), "{err}");
        }
    }

    /// `calin-auth`'s reader over a map instead of the process environment.
    fn mode_from(vars: &[(&str, &str)], switch: Option<&str>) -> Result<AuthMode> {
        let vars: std::collections::HashMap<String, String> = vars
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect();
        read_auth_mode(switch.map(OsString::from), |loopback_http| {
            AuthMode::from_lookup(|name| vars.get(name).cloned(), loopback_http)
        })
    }

    const PUBLIC: [(&str, &str); 4] = [
        ("CALIN_PUBLIC_URL", "https://calin.example.com"),
        ("CALIN_OIDC_ISSUER", "https://issuer.example.com/"),
        ("CALIN_OIDC_CLIENT_ID", "calin"),
        ("CALIN_ALLOWED_USERS", "alice@example.com"),
    ];

    fn without(name: &str) -> Vec<(&'static str, &'static str)> {
        PUBLIC.into_iter().filter(|(n, _)| *n != name).collect()
    }

    fn with(name: &'static str, value: &'static str) -> Vec<(&'static str, &'static str)> {
        let mut vars = without(name);
        vars.push((name, value));
        vars
    }

    #[test]
    fn no_sign_in_variable_is_local_mode_and_a_complete_set_is_public_mode() {
        assert!(matches!(mode_from(&[], None), Ok(AuthMode::Local)));
        // The switch alone changes nothing: there is no URL for it to allow.
        assert!(matches!(mode_from(&[], Some("1")), Ok(AuthMode::Local)));
        assert!(matches!(mode_from(&[], Some("")), Ok(AuthMode::Local)));
        let Ok(AuthMode::Public(config)) = mode_from(&PUBLIC, None) else {
            panic!("expected public mode");
        };
        assert_eq!(config.public_origin(), "https://calin.example.com");
        assert_eq!(config.loopback_http(), LoopbackHttp::Refuse);
    }

    #[test]
    fn each_configuration_error_refuses_to_start_and_names_the_variable() {
        // (variables, what the error must mention)
        let cases: Vec<(Vec<(&str, &str)>, &str)> = vec![
            (without("CALIN_OIDC_ISSUER"), "CALIN_OIDC_ISSUER is not set"),
            (
                without("CALIN_OIDC_CLIENT_ID"),
                "CALIN_OIDC_CLIENT_ID is not set",
            ),
            (without("CALIN_ALLOWED_USERS"), "CALIN_ALLOWED_USERS"),
            (with("CALIN_ALLOWED_USERS", " , "), "CALIN_ALLOWED_USERS"),
            (
                with("CALIN_ALLOWED_USERS", "not an identity"),
                "CALIN_ALLOWED_USERS",
            ),
            (with("CALIN_PUBLIC_URL", ""), "CALIN_PUBLIC_URL is empty"),
            (
                with("CALIN_PUBLIC_URL", "http://calin.example.com"),
                "CALIN_PUBLIC_URL must be an https URL",
            ),
            (
                with("CALIN_PUBLIC_URL", "http://localhost:4680"),
                "CALIN_PUBLIC_URL must be an https URL",
            ),
            (
                with("CALIN_PUBLIC_URL", "https://calin.example.com/app"),
                "CALIN_PUBLIC_URL must be an origin only",
            ),
            (
                with("CALIN_PUBLIC_URL", "https://user:pw@calin.example.com"),
                "CALIN_PUBLIC_URL must not contain credentials",
            ),
            (
                with("CALIN_OIDC_ISSUER", "http://issuer.example.com"),
                "CALIN_OIDC_ISSUER must be an https URL",
            ),
            (
                with("CALIN_OIDC_ISSUER", "http://127.0.0.1:9000"),
                "CALIN_OIDC_ISSUER must be an https URL",
            ),
            (
                with("CALIN_OIDC_CLIENT_SECRET", " padded "),
                "CALIN_OIDC_CLIENT_SECRET",
            ),
            // Sign-in configured without the URL that turns it on.
            (
                without("CALIN_PUBLIC_URL"),
                "CALIN_PUBLIC_URL is not set although",
            ),
        ];
        for (vars, expected) in cases {
            let error = mode_from(&vars, None).unwrap_err().to_string();
            assert!(error.contains(expected), "{vars:?}: {error}");
            assert!(error.contains("did not start"), "{error}");
        }
    }

    #[test]
    fn every_problem_is_listed_on_its_own_line() {
        let error = mode_from(&[("CALIN_PUBLIC_URL", "ftp://x")], Some("yes"))
            .unwrap_err()
            .to_string();
        for line in [
            "  - CALIN_UNSAFE_DEV_ALLOW_HTTP_LOOPBACK must be 1 or not set",
            "  - CALIN_PUBLIC_URL must be an https URL",
            "  - CALIN_OIDC_ISSUER is not set",
            "  - CALIN_OIDC_CLIENT_ID is not set",
        ] {
            assert!(
                error.lines().any(|l| l == line),
                "{line:?} not in:\n{error}"
            );
        }
        assert!(error.contains("CALIN_ALLOWED_USERS"), "{error}");
    }

    #[test]
    fn the_development_switch_allows_plain_http_on_loopback_and_nowhere_else() {
        let dev = [
            ("CALIN_PUBLIC_URL", "http://localhost:4680"),
            ("CALIN_OIDC_ISSUER", "http://127.0.0.1:9000"),
            ("CALIN_OIDC_CLIENT_ID", "calin"),
            ("CALIN_ALLOWED_USERS", "alice@example.com"),
        ];
        // Without the switch, or with a value that is not `1`: refused.
        for switch in [
            None,
            Some(""),
            Some("0"),
            Some("true"),
            Some("yes"),
            Some(" 1"),
        ] {
            assert!(mode_from(&dev, switch).is_err(), "{switch:?}");
        }
        let Ok(AuthMode::Public(config)) = mode_from(&dev, Some("1")) else {
            panic!("expected public mode");
        };
        assert_eq!(config.public_origin(), "http://localhost:4680");
        assert_eq!(config.loopback_http(), LoopbackHttp::AllowForDevelopment);
        // The switch never covers a host that is not loopback.
        let elsewhere = with("CALIN_PUBLIC_URL", "http://calin.example.com");
        let error = mode_from(&elsewhere, Some("1")).unwrap_err().to_string();
        assert!(
            error.contains("CALIN_PUBLIC_URL must be an https URL"),
            "{error}"
        );
        // A value other than `1` is an error even when nothing needs the switch.
        let error = mode_from(&PUBLIC, Some("on")).unwrap_err().to_string();
        assert!(error.contains(ENV_UNSAFE_HTTP), "{error}");
    }
}
