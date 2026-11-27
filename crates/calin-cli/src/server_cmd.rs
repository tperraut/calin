//! `calin server install | check | uninstall`: thin glue over `calin-install`.
//!
//! Nothing here changes the machine beyond writing the staged files into a
//! directory the user names. The commands that need root are printed for the
//! owner to run (remote access spec, section 5).

use std::fs;
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use calin_install::{
    CheckReport, CheckTarget, InstallInput, InstallInputBuilder, Plan, PlannedFile, SystemProbes,
    Usability, install_plan, render_steps, run_checks, uninstall_plan,
};
use clap::{Args, Subcommand};

use crate::absolute;

#[derive(Subcommand)]
pub enum ServerCommand {
    /// Write the service files to a staging directory and print the commands to run as root
    Install(InstallArgs),
    /// Verify an installation: the service, the public URL, the sign-in provider, the agent logins
    Check(CheckArgs),
    /// Print how to remove an installation. Data and repositories are never removed for you
    Uninstall(LayoutArgs),
}

/// Where the service lives on the server. The defaults suit a fresh machine.
#[derive(Args)]
pub struct LayoutArgs {
    /// The unprivileged user the service runs as
    #[arg(long, value_name = "NAME")]
    user: Option<String>,
    /// Calin's data directory
    #[arg(long, value_name = "DIR")]
    data_dir: Option<PathBuf>,
    /// Where the calin binary is installed
    #[arg(long, value_name = "PATH")]
    binary: Option<PathBuf>,
    /// The loopback port the service listens on
    #[arg(long, value_name = "PORT")]
    port: Option<u16>,
    /// A directory the service may write to, where your repositories live (repeatable)
    #[arg(long = "repos-dir", value_name = "DIR")]
    repos_dirs: Vec<PathBuf>,
}

#[derive(Args)]
pub struct InstallArgs {
    /// The domain users will type, for example calin.example.com
    #[arg(long, value_name = "HOST")]
    domain: String,
    /// The OpenID Connect issuer URL of your sign-in provider
    #[arg(long, value_name = "URL")]
    issuer: String,
    /// The client id registered with the provider
    #[arg(long, value_name = "ID")]
    client_id: String,
    /// A file holding the client secret, if the provider gave one. It is read here, never passed
    /// on the command line, so it stays out of your shell history
    #[arg(long, value_name = "FILE")]
    client_secret_file: Option<PathBuf>,
    /// An identity allowed to sign in: an email address or sub:<subject> (repeatable)
    #[arg(long = "allow", value_name = "IDENTITY", required = true)]
    allowed: Vec<String>,
    #[command(flatten)]
    layout: LayoutArgs,
    /// Where to write the files before you copy them into place
    #[arg(long, value_name = "DIR", default_value = "calin-install")]
    staging: PathBuf,
}

#[derive(Args)]
pub struct CheckArgs {
    /// The domain the installation serves
    #[arg(long, value_name = "HOST")]
    domain: String,
    /// The OpenID Connect issuer URL it signs in with
    #[arg(long, value_name = "URL")]
    issuer: String,
    #[command(flatten)]
    layout: LayoutArgs,
}

pub async fn run(command: ServerCommand) -> Result<()> {
    match command {
        ServerCommand::Install(args) => install(args),
        ServerCommand::Check(args) => check(args).await,
        ServerCommand::Uninstall(layout) => uninstall(layout),
    }
}

fn with_layout(mut builder: InstallInputBuilder, layout: LayoutArgs) -> InstallInputBuilder {
    if let Some(user) = layout.user {
        builder = builder.user(user);
    }
    if let Some(dir) = layout.data_dir {
        builder = builder.data_dir(dir);
    }
    if let Some(binary) = layout.binary {
        builder = builder.binary(binary);
    }
    if let Some(port) = layout.port {
        builder = builder.port(port);
    }
    if !layout.repos_dirs.is_empty() {
        builder = builder.extra_dirs(layout.repos_dirs);
    }
    builder
}

fn install(args: InstallArgs) -> Result<()> {
    let staging = args.staging.clone();
    let input = install_input(args)?;
    let staging = absolute(&staging)?;
    let plan = install_plan(&input, &staging)?;
    write_staging(&staging, &plan.files)?;
    print_install(&plan, &staging, &input);
    Ok(())
}

/// What the flags describe, with the client secret read from its file.
fn install_input(args: InstallArgs) -> Result<InstallInput> {
    let mut builder = InstallInput::builder()
        .domain(args.domain)
        .issuer(args.issuer)
        .client_id(args.client_id)
        .allowed_users(args.allowed);
    if let Some(path) = &args.client_secret_file {
        let secret = fs::read_to_string(path)
            .with_context(|| format!("cannot read the client secret from {}", path.display()))?;
        builder = builder.client_secret(secret.trim_end_matches(['\r', '\n']).to_owned());
    }
    with_layout(builder, args.layout)
        .build()
        .map_err(|problems| anyhow::anyhow!("the installation cannot be planned:\n{problems}"))
}

/// The staging directory holds the environment file with the client secret.
fn write_staging(staging: &Path, files: &[PlannedFile]) -> Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(staging)
        .with_context(|| format!("cannot create {}", staging.display()))?;
    for file in files {
        let path = staging.join(file.staging_name);
        let mode = if file.sensitive { 0o600 } else { 0o644 };
        let mut out = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(mode)
            .open(&path)
            .with_context(|| format!("cannot write {}", path.display()))?;
        out.write_all(file.content.as_bytes())
            .with_context(|| format!("cannot write {}", path.display()))?;
    }
    Ok(())
}

fn print_install(plan: &Plan, staging: &Path, input: &InstallInput) {
    println!("Wrote {} files to {}:", plan.files.len(), staging.display());
    for file in &plan.files {
        println!("  {}  ->  {}", file.staging_name, file.target.display());
    }
    println!("\nNothing on this machine was changed. To install, run these in order:\n");
    print!("{}", render_steps(&plan.steps));
    println!(
        "\nThen verify with: calin server check --domain {} --issuer {}",
        input.domain(),
        input.issuer()
    );
}

async fn check(args: CheckArgs) -> Result<()> {
    check_with(args, |target| {
        let probes = SystemProbes::new()?;
        Ok(run_checks(target, probes.probes()))
    })
    .await
}

/// Checks the installation the flags describe with the report `run` makes,
/// and fails when it is not usable.
async fn check_with(
    args: CheckArgs,
    run: impl FnOnce(&CheckTarget) -> Result<CheckReport> + Send + 'static,
) -> Result<()> {
    let builder = InstallInput::builder()
        .domain(args.domain)
        .issuer(args.issuer);
    let target = with_layout(builder, args.layout)
        .build_check_target()
        .map_err(|problems| anyhow::anyhow!("the installation cannot be checked:\n{problems}"))?;

    // The probes block (commands, files, HTTP with its own runtime).
    let report = tokio::task::spawn_blocking(move || run(&target))
        .await
        .context("the checks did not finish")??;

    print!("{report}");
    match report.usability() {
        Usability::Usable => Ok(()),
        Usability::Unknown => {
            println!("\nSome checks could not run from here; nothing that ran failed.");
            Ok(())
        }
        Usability::NotUsable => bail!("the installation is not usable yet"),
    }
}

fn uninstall(layout: LayoutArgs) -> Result<()> {
    // The uninstall steps depend only on the layout. The sign-in fields are
    // required by the input type and do not appear in the output.
    let builder = InstallInput::builder()
        .domain("calin.invalid")
        .issuer("https://issuer.invalid/")
        .client_id("unused")
        .allowed_user("unused@calin.invalid");
    let input = with_layout(builder, layout)
        .build()
        .map_err(|problems| anyhow::anyhow!("the layout is not valid:\n{problems}"))?;
    println!("Nothing is removed for you. To uninstall, run:\n");
    print!("{}", uninstall_plan(&input));
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use calin_install::{Check, CheckResult, Status};
    use clap::Parser as _;
    use tempfile::TempDir;

    use super::*;

    /// `calin server <args>`, parsed as the binary parses it.
    fn parse(args: &[&str]) -> ServerCommand {
        let cli = crate::Cli::try_parse_from(["calin", "server"].iter().chain(args)).unwrap();
        let Some(crate::Command::Server(command)) = cli.command else {
            panic!("not a server command: {args:?}");
        };
        command
    }

    fn text(path: &Path) -> &str {
        path.to_str().unwrap()
    }

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[tokio::test]
    async fn install_stages_the_files_with_the_layout_and_the_secret_kept_private() {
        let dir = TempDir::new().unwrap();
        let secret = dir.path().join("secret");
        fs::write(&secret, "s3cret\r\n").unwrap();
        let staging = dir.path().join("staging");
        let command = parse(&[
            "install",
            "--domain",
            "calin.example.com",
            "--issuer",
            "https://issuer.example.com/",
            "--client-id",
            "calin",
            "--client-secret-file",
            text(&secret),
            "--allow",
            "alice@example.com",
            "--user",
            "agents",
            "--data-dir",
            "/srv/calin",
            "--binary",
            "/opt/calin/calin",
            "--port",
            "4999",
            "--repos-dir",
            "/srv/repos",
            "--staging",
            text(&staging),
        ]);
        run(command).await.unwrap();

        assert_eq!(mode(&staging), 0o700);
        let env = staging.join("calin.env");
        assert_eq!(mode(&env), 0o600);
        let env = fs::read_to_string(env).unwrap();
        assert!(env.contains("s3cret\n"), "{env}");
        assert!(!env.contains('\r'), "{env}");
        let unit = staging.join("calin.service");
        assert_eq!(mode(&unit), 0o644);
        let unit = fs::read_to_string(unit).unwrap();
        for line in [
            "User=agents",
            "ExecStart=/opt/calin/calin serve --bind 127.0.0.1:4999 --home /srv/calin",
        ] {
            assert!(unit.lines().any(|l| l == line), "{line:?} not in\n{unit}");
        }
        assert!(unit.contains("/srv/repos"), "{unit}");
        let caddy = staging.join("calin.caddy");
        assert_eq!(mode(&caddy), 0o644);
        assert!(
            fs::read_to_string(caddy)
                .unwrap()
                .contains("calin.example.com")
        );
    }

    #[tokio::test]
    async fn install_refuses_before_writing_anything() {
        let dir = TempDir::new().unwrap();
        let staging = dir.path().join("staging");
        let missing = dir.path().join("no-secret");
        let install = |extra: &[&str]| {
            let mut args = vec![
                "install",
                "--domain",
                "calin.example.com",
                "--client-id",
                "calin",
                "--allow",
                "alice@example.com",
                "--staging",
                text(&staging),
            ];
            args.extend_from_slice(extra);
            parse(&args)
        };

        let error = run(install(&[
            "--issuer",
            "https://issuer.example.com/",
            "--client-secret-file",
            text(&missing),
        ]))
        .await
        .unwrap_err();
        assert!(
            format!("{error:#}").contains("cannot read the client secret from"),
            "{error:#}"
        );
        let error = run(install(&[
            "--issuer",
            "http://issuer.example.com/",
            "--port",
            "80",
        ]))
        .await
        .unwrap_err()
        .to_string();
        assert!(
            error.starts_with("the installation cannot be planned:"),
            "{error}"
        );
        assert!(!staging.exists());
    }

    fn check_args(extra: &[&str]) -> CheckArgs {
        let mut args = vec![
            "check",
            "--domain",
            "calin.example.com",
            "--issuer",
            "https://issuer.example.com/",
        ];
        args.extend_from_slice(extra);
        let ServerCommand::Check(args) = parse(&args) else {
            panic!("not a check");
        };
        args
    }

    /// A report with one essential check.
    fn report(status: Status) -> CheckReport {
        CheckReport {
            results: vec![CheckResult {
                check: Check::PublicUrl,
                status,
                reason: "reason".into(),
                remedy: None,
            }],
        }
    }

    #[tokio::test]
    async fn check_fails_only_when_an_essential_check_fails() {
        for (status, usable) in [
            (Status::Pass, true),
            (Status::CouldNotCheck, true),
            (Status::Fail, false),
        ] {
            let result = check_with(check_args(&["--port", "4999"]), move |target| {
                assert_eq!(target.domain(), "calin.example.com");
                assert_eq!(target.issuer(), "https://issuer.example.com/");
                assert_eq!(target.layout().port(), 4999);
                Ok(report(status))
            })
            .await;
            assert_eq!(result.is_ok(), usable, "{status:?}: {result:?}");
            if let Err(error) = result {
                assert_eq!(error.to_string(), "the installation is not usable yet");
            }
        }
    }

    #[tokio::test]
    async fn check_refuses_a_bad_layout_and_reports_probes_that_cannot_start() {
        let error = check_with(check_args(&["--port", "80"]), |_| {
            panic!("checked a layout that is not valid")
        })
        .await
        .unwrap_err()
        .to_string();
        assert!(
            error.starts_with("the installation cannot be checked:"),
            "{error}"
        );
        let error = check_with(check_args(&[]), |_| bail!("no HTTP client"))
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "no HTTP client");
    }

    #[tokio::test]
    async fn uninstall_needs_a_valid_layout_only() {
        run(parse(&["uninstall"])).await.unwrap();
        run(parse(&["uninstall", "--user", "agents", "--port", "4999"]))
            .await
            .unwrap();
        let error = run(parse(&["uninstall", "--port", "80"]))
            .await
            .unwrap_err()
            .to_string();
        assert!(error.starts_with("the layout is not valid:"), "{error}");
    }
}
