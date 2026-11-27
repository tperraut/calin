//! Installing Calin as a service on a Linux server with systemd, behind
//! Caddy, and verifying the result. Specified in
//! `docs/specs/2026-11-27-remote-access.md`, section 5.
//!
//! This crate never changes the machine. It renders files and the exact
//! commands the owner runs as root, and it runs read-only checks.
//!
//! # The three operations
//!
//! - **install**: [`install_plan`] turns a validated [`InstallInput`] into a
//!   [`Plan`]: three [`PlannedFile`]s (the systemd unit, the environment
//!   file, the Caddy site) and the ordered [`Step`]s that put them in place
//!   and start the service. The CLI writes the files to a staging directory
//!   and prints the steps, which copy from there.
//! - **check**: [`run_checks`] verifies an installation through [`Probes`]
//!   and returns a [`CheckReport`], whose `Display` is a short table and
//!   which knows whether the installation is usable.
//! - **uninstall**: [`uninstall_plan`] returns the steps that remove what
//!   install added and, kept apart, the optional destructive ones.
//!
//! # How the CLI calls it
//!
//! ```no_run
//! use std::path::Path;
//! use calin_install::{
//!     InstallInput, SystemProbes, install_plan, render_steps, run_checks, uninstall_plan,
//! };
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! // `calin server install`: every problem with the flags comes back at once.
//! let input = InstallInput::builder()
//!     .domain("calin.example.com")
//!     .issuer("https://your-tenant.eu.auth0.com/")
//!     .client_id("abc123")
//!     .allowed_user("you@example.com")
//!     .build()?;
//! let staging = Path::new("/home/you/calin-install");
//! let plan = install_plan(&input, staging)?;
//! for file in &plan.files {
//!     // Write `file.content` to `staging/file.staging_name`, mode 0600 when
//!     // `file.sensitive`. Never print a sensitive file.
//! }
//! print!("{}", render_steps(&plan.steps));
//!
//! // `calin server check`: the domain and the issuer are enough.
//! let target = InstallInput::builder()
//!     .domain("calin.example.com")
//!     .issuer("https://your-tenant.eu.auth0.com/")
//!     .build_check_target()?;
//! let probes = SystemProbes::new()?;
//! // Blocking: from async code, call it inside `spawn_blocking`.
//! let report = run_checks(&target, probes.probes());
//! print!("{report}");
//! let exit_code = if report.is_usable() { 0 } else { 1 };
//!
//! // `calin server uninstall`.
//! print!("{}", uninstall_plan(&input));
//! # let _ = exit_code;
//! # Ok(())
//! # }
//! ```

mod check;
mod envfile;
mod input;
mod plan;
mod probe;
mod render;

#[cfg(test)]
mod golden;

pub use check::{
    Check, CheckReport, CheckResult, CommandError, CommandOutput, CommandRunner, FileInfo,
    FileProbe, HttpError, HttpGet, HttpResponse, Probes, Status, Usability, run_checks,
};
pub use envfile::{EnvParseError, parse_env_file};
pub use input::{
    CheckTarget, DEFAULT_BINARY, DEFAULT_DATA_DIR, DEFAULT_EXTRA_DIR, DEFAULT_PORT, DEFAULT_USER,
    Field, FieldError, InputErrors, InstallInput, InstallInputBuilder, Secret, ServiceLayout,
};
pub use plan::{
    Plan, PlanError, PlannedFile, Step, UninstallPlan, install_plan, render_steps, uninstall_plan,
};
pub use probe::{ProbeSetupError, SystemProbes};
pub use render::{
    CADDY_IMPORT_LINE, CADDY_PATH, ENV_PATH, UNIT_PATH, render_caddy, render_env, render_unit,
};
