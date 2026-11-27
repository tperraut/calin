//! `calin home init`: thin glue over the `home_init` module.

use std::path::PathBuf;

use anyhow::{Context as _, Result};
use clap::{Args, Subcommand};

use crate::home_init::{self, Options};

#[derive(Subcommand)]
pub enum HomeCommand {
    /// Scaffold a home repo: the repo list, two workflows, a README and a script that links
    /// skills into ~/.claude. Never overwrites a file and never commits
    Init(InitArgs),
}

#[derive(Args)]
pub struct InitArgs {
    /// The home repo's folder; created, and given `git init`, if needed
    #[arg(value_name = "DIR", default_value = ".")]
    dir: PathBuf,
    /// A folder whose subfolders are git clones to list in .calin/repos.yaml (repeatable)
    #[arg(long = "scan", value_name = "DIR")]
    scan: Vec<PathBuf>,
}

/// `server` is the address `calin serve` listens on by default, for the
/// example request printed at the end.
pub async fn run(command: HomeCommand, server: &str) -> Result<()> {
    match command {
        HomeCommand::Init(args) => {
            let options = Options {
                dir: args.dir,
                scan: args.scan,
                git: "git".into(),
            };
            // git and the file system block.
            let report = tokio::task::spawn_blocking(move || home_init::init(&options))
                .await
                .context("the scaffold did not finish")??;
            print!("{}", home_init::summary(&report, server));
            Ok(())
        }
    }
}
