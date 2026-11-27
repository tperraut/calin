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

#[cfg(test)]
mod tests {
    use clap::Parser as _;
    use tempfile::TempDir;

    use super::*;

    /// `calin home <args>`, parsed as the binary parses it.
    fn parse(args: &[&str]) -> HomeCommand {
        let cli = crate::Cli::try_parse_from(["calin", "home"].iter().chain(args)).unwrap();
        let Some(crate::Command::Home(command)) = cli.command else {
            panic!("not a home command: {args:?}");
        };
        command
    }

    #[tokio::test]
    async fn init_scaffolds_the_folder_it_is_given() {
        let dir = TempDir::new().unwrap();
        let home = dir.path().join("home");
        let command = parse(&["init", home.to_str().unwrap()]);
        run(command, "http://127.0.0.1:4680").await.unwrap();
        assert!(home.join(".git").is_dir());
        assert!(home.join(".calin/repos.yaml").is_file());
        assert!(home.join("README.md").is_file());
    }

    #[tokio::test]
    async fn init_reports_what_stops_it() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("file");
        std::fs::write(&file, "").unwrap();
        let error = run(parse(&["init", file.to_str().unwrap()]), "http://x")
            .await
            .unwrap_err();
        assert!(
            error.to_string().ends_with("is a file, not a directory."),
            "{error}"
        );
    }
}
