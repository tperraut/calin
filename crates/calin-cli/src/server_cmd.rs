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
    InstallInput, InstallInputBuilder, SystemProbes, Usability, install_plan, render_steps,
    run_checks, uninstall_plan,
};
use clap::{Args, Subcommand};

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
    let input = with_layout(builder, args.layout)
        .build()
        .map_err(|problems| anyhow::anyhow!("the installation cannot be planned:\n{problems}"))?;

    let staging = absolute(&args.staging)?;
    let plan = install_plan(&input, &staging)?;

    // The staging directory holds the environment file with the client secret.
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&staging)
        .with_context(|| format!("cannot create {}", staging.display()))?;
    for file in &plan.files {
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
    Ok(())
}

async fn check(args: CheckArgs) -> Result<()> {
    let builder = InstallInput::builder()
        .domain(args.domain)
        .issuer(args.issuer);
    let target = with_layout(builder, args.layout)
        .build_check_target()
        .map_err(|problems| anyhow::anyhow!("the installation cannot be checked:\n{problems}"))?;

    // The probes block (commands, files, HTTP with its own runtime).
    let report = tokio::task::spawn_blocking(move || -> Result<_> {
        let probes = SystemProbes::new()?;
        Ok(run_checks(&target, probes.probes()))
    })
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

fn absolute(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    let cwd = std::env::current_dir().context("cannot read the current directory")?;
    Ok(cwd.join(path))
}
