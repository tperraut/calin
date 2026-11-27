//! What `install` and `uninstall` print: files to stage and the commands
//! the owner runs as root.

use std::fmt;
use std::path::{Path, PathBuf};

use crate::input::InstallInput;
use crate::render::{
    CADDY_DIR, CADDY_IMPORT_LINE, CADDY_PATH, CADDYFILE_PATH, ENV_DIR, ENV_PATH, UNIT_NAME,
    UNIT_PATH, render_caddy, render_env, render_unit,
};

/// One file to install. The CLI writes `content` to
/// `<staging>/<staging_name>` (mode 0600 when `sensitive`), and a step
/// copies it to `target` with `mode` and `owner`.
#[derive(Clone, PartialEq, Eq)]
pub struct PlannedFile {
    pub target: PathBuf,
    pub staging_name: &'static str,
    pub mode: u32,
    pub owner: &'static str,
    pub group: &'static str,
    pub content: String,
    /// The content holds a secret: never print it, stage it with mode 0600.
    pub sensitive: bool,
}

impl fmt::Debug for PlannedFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let content: &dyn fmt::Debug = if self.sensitive {
            &"[redacted]"
        } else {
            &self.content
        };
        f.debug_struct("PlannedFile")
            .field("target", &self.target)
            .field("staging_name", &self.staging_name)
            .field("mode", &format_args!("{:04o}", self.mode))
            .field("owner", &self.owner)
            .field("group", &self.group)
            .field("content", content)
            .field("sensitive", &self.sensitive)
            .finish()
    }
}

/// One thing to do: what and why in one line, and the command, if a
/// command can do it. `root` says the command needs root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub summary: String,
    pub command: Option<String>,
    pub root: bool,
}

impl Step {
    fn root(summary: impl Into<String>, command: impl Into<String>) -> Self {
        Self {
            summary: summary.into(),
            command: Some(command.into()),
            root: true,
        }
    }
    fn manual(summary: impl Into<String>, command: Option<String>, root: bool) -> Self {
        Self {
            summary: summary.into(),
            command,
            root,
        }
    }
}

/// The install: files to stage, then steps in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub files: Vec<PlannedFile>,
    pub steps: Vec<Step>,
}

/// The uninstall: steps that remove what install added, and, kept apart,
/// the destructive ones the owner may choose to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UninstallPlan {
    pub steps: Vec<Step>,
    /// Deletes data, repositories and the user. Never part of `steps`.
    pub optional_destructive: Vec<Step>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlanError {
    #[error("the staging directory must be an absolute path")]
    StagingNotAbsolute,
    #[error("the staging directory must be valid UTF-8 without control characters")]
    StagingNotPrintable,
}

/// Plans the install. `staging` is where the CLI writes the files before
/// the owner copies them into place; it is quoted in the commands.
pub fn install_plan(input: &InstallInput, staging: &Path) -> Result<Plan, PlanError> {
    if !staging.is_absolute() {
        return Err(PlanError::StagingNotAbsolute);
    }
    let staging_text = staging
        .to_str()
        .filter(|s| !s.chars().any(char::is_control))
        .ok_or(PlanError::StagingNotPrintable)?;
    let layout = input.layout();
    let user = layout.user();

    let files = vec![
        PlannedFile {
            target: UNIT_PATH.into(),
            staging_name: UNIT_NAME,
            mode: 0o644,
            owner: "root",
            group: "root",
            content: render_unit(layout),
            sensitive: false,
        },
        PlannedFile {
            target: ENV_PATH.into(),
            staging_name: "calin.env",
            mode: 0o600,
            owner: "root",
            group: "root",
            content: render_env(input),
            sensitive: true,
        },
        PlannedFile {
            target: CADDY_PATH.into(),
            staging_name: "calin.caddy",
            mode: 0o644,
            owner: "root",
            group: "root",
            content: render_caddy(input.domain(), layout),
            sensitive: false,
        },
    ];

    let mut steps = vec![
        Step::root(
            format!("Create the service user `{user}` with a home directory and no login shell"),
            format!(
                "id -u {user} >/dev/null 2>&1 || useradd --system --user-group --create-home --home-dir {home} --shell /usr/sbin/nologin {user}",
                home = layout.home_dir().display()
            ),
        ),
        Step::root(
            "Create the data directory, owned by the service user",
            format!(
                "install -d -m 0750 -o {user} -g {user} {}",
                layout.data_dir().display()
            ),
        ),
    ];
    if !layout.extra_dirs().is_empty() {
        let dirs = layout
            .extra_dirs()
            .iter()
            .map(|d| d.display().to_string())
            .collect::<Vec<_>>()
            .join(" ");
        steps.push(Step::root(
            "Create the directories for repositories, owned by the service user",
            format!("install -d -m 0750 -o {user} -g {user} {dirs}"),
        ));
    }
    steps.push(Step::root(
        "Create the configuration directories",
        format!("install -d -m 0755 -o root -g root {ENV_DIR} {CADDY_DIR}"),
    ));
    for file in &files {
        steps.push(Step::root(
            format!("Install {}", file.target.display()),
            format!(
                "install -m {:04o} -o {} -g {} {} {}",
                file.mode,
                file.owner,
                file.group,
                shell_quote(&format!("{staging_text}/{}", file.staging_name)),
                file.target.display()
            ),
        ));
    }
    steps.extend([
        Step::root(
            format!("Make the main Caddyfile load {CADDY_DIR} (adds the import line once)"),
            format!(
                "grep -qxF '{CADDY_IMPORT_LINE}' {CADDYFILE_PATH} || echo '{CADDY_IMPORT_LINE}' >> {CADDYFILE_PATH}"
            ),
        ),
        Step::root("Make systemd read the new unit", "systemctl daemon-reload"),
        Step::root(
            "Start Calin now and at every boot",
            "systemctl enable --now calin.service",
        ),
        Step::root(
            "Reload Caddy so it serves the domain and obtains its certificate",
            "systemctl reload caddy",
        ),
        Step::manual(
            format!(
                "Sign in the agent CLIs as `{user}`: in Calin's own terminal after your first sign-in, or here (then `gh auth login` the same way)"
            ),
            Some(format!("sudo -u {user} -H claude")),
            true,
        ),
        Step::manual(
            format!(
                "Register {}/auth/callback as a redirect URI with your identity provider",
                input.public_url()
            ),
            None,
            false,
        ),
    ]);
    Ok(Plan { files, steps })
}

/// Plans the uninstall. Nothing in `steps` deletes data, repositories or
/// the user.
pub fn uninstall_plan(input: &InstallInput) -> UninstallPlan {
    uninstall_for(input.layout())
}

pub(crate) fn uninstall_for(layout: &crate::input::ServiceLayout) -> UninstallPlan {
    let user = layout.user();
    let steps = vec![
        Step::root(
            "Stop Calin and keep it from starting at boot",
            "systemctl disable --now calin.service",
        ),
        Step::root("Remove the unit", format!("rm -f {UNIT_PATH}")),
        Step::root("Make systemd forget the unit", "systemctl daemon-reload"),
        Step::root(
            "Remove the environment file, which holds the client secret",
            format!("rm -f {ENV_PATH} && rmdir --ignore-fail-on-non-empty {ENV_DIR}"),
        ),
        Step::root("Remove the Caddy site", format!("rm -f {CADDY_PATH}")),
        Step::root(
            format!(
                "Reload Caddy so it stops serving the domain (the import line in {CADDYFILE_PATH} can stay)"
            ),
            "systemctl reload caddy",
        ),
        Step::manual(
            format!(
                "Kept: the data directory {}, the repositories{}, and the user `{user}` with its agent logins",
                layout.data_dir().display(),
                if layout.extra_dirs().is_empty() {
                    String::new()
                } else {
                    format!(
                        " in {}",
                        layout
                            .extra_dirs()
                            .iter()
                            .map(|d| d.display().to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                }
            ),
            None,
            false,
        ),
    ];
    let mut optional_destructive = vec![Step::root(
        "Delete Calin's data: sessions, their logs, sign-in sessions and device tokens",
        format!("rm -rf {}", layout.data_dir().display()),
    )];
    for dir in layout.extra_dirs() {
        optional_destructive.push(Step::root(
            format!(
                "Delete {} and every repository in it, with any work not pushed",
                dir.display()
            ),
            format!("rm -rf {}", dir.display()),
        ));
    }
    optional_destructive.push(Step::root(
        format!("Delete the user `{user}` and its home, with the agent CLIs' logins"),
        format!("userdel --remove {user}"),
    ));
    UninstallPlan {
        steps,
        optional_destructive,
    }
}

/// The steps as the CLI prints them: numbered, root marked, commands indented.
pub fn render_steps(steps: &[Step]) -> String {
    let mut out = String::new();
    for (index, step) in steps.iter().enumerate() {
        let marker = if step.root && step.command.is_some() {
            " [root]"
        } else {
            ""
        };
        out.push_str(&format!("{}. {}{marker}\n", index + 1, step.summary));
        if let Some(command) = &step.command {
            out.push_str(&format!("   {command}\n"));
        }
    }
    out
}

impl fmt::Display for UninstallPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&render_steps(&self.steps))?;
        f.write_str(
            "\nOptional and destructive: run these only to erase everything. They cannot be undone.\n",
        )?;
        f.write_str(&render_steps(&self.optional_destructive))
    }
}

/// Quotes a word for a POSIX shell when it needs it.
pub(crate) fn shell_quote(word: &str) -> String {
    if !word.is_empty()
        && word.chars().all(|c| {
            c.is_ascii_alphanumeric()
                || matches!(c, '/' | '.' | '_' | '-' | '+' | '@' | ':' | ',' | '=')
        })
    {
        return word.to_owned();
    }
    format!("'{}'", word.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::tests::{AWKWARD_SECRET, awkward, typical};

    #[test]
    fn staging_must_be_absolute_and_printable() {
        let input = typical().build().unwrap();
        assert_eq!(
            install_plan(&input, Path::new("staging")),
            Err(PlanError::StagingNotAbsolute)
        );
        assert_eq!(
            install_plan(&input, Path::new("/tmp/a\nb")),
            Err(PlanError::StagingNotPrintable)
        );
    }

    #[test]
    fn staging_paths_are_quoted() {
        let input = typical().build().unwrap();
        let plan = install_plan(&input, Path::new("/home/me/it's here")).unwrap();
        let commands: Vec<&str> = plan
            .steps
            .iter()
            .filter_map(|s| s.command.as_deref())
            .collect();
        assert!(
            commands.contains(&"install -m 0644 -o root -g root '/home/me/it'\\''s here/calin.service' /etc/systemd/system/calin.service"),
            "{commands:#?}"
        );
    }

    #[test]
    fn files_have_the_right_modes_and_owners() {
        let plan = install_plan(&awkward().build().unwrap(), Path::new("/tmp/stage")).unwrap();
        let summary: Vec<(String, u32, &str, bool)> = plan
            .files
            .iter()
            .map(|f| (f.target.display().to_string(), f.mode, f.owner, f.sensitive))
            .collect();
        assert_eq!(
            summary,
            [
                (UNIT_PATH.to_owned(), 0o644, "root", false),
                (ENV_PATH.to_owned(), 0o600, "root", true),
                (CADDY_PATH.to_owned(), 0o644, "root", false),
            ]
        );
        // Every file is installed by a step from its staging path.
        for file in &plan.files {
            let expected = format!(
                "install -m {:04o} -o root -g root /tmp/stage/{} {}",
                file.mode,
                file.staging_name,
                file.target.display()
            );
            assert!(
                plan.steps
                    .iter()
                    .any(|s| s.command.as_deref() == Some(&expected)),
                "{expected}"
            );
        }
    }

    #[test]
    fn steps_come_in_order() {
        let plan = install_plan(&typical().build().unwrap(), Path::new("/tmp/stage")).unwrap();
        let position = |needle: &str| {
            plan.steps
                .iter()
                .position(|s| s.command.as_deref().is_some_and(|c| c.contains(needle)))
                .unwrap_or_else(|| panic!("no step runs {needle}"))
        };
        let order = [
            position("useradd"),
            position("install -d -m 0750 -o calin -g calin /var/lib/calin"),
            position("/srv/calin"),
            position("calin.service /etc/systemd"),
            position("calin.env"),
            position("calin.caddy"),
            position("daemon-reload"),
            position("enable --now"),
            position("reload caddy"),
            position("sudo -u calin -H claude"),
        ];
        assert!(order.windows(2).all(|w| w[0] < w[1]), "{order:?}");
        let last = plan.steps.last().unwrap();
        assert!(
            last.summary
                .contains("https://calin.example.com/auth/callback")
        );
    }

    #[test]
    fn uninstall_deletes_nothing_of_the_owners() {
        for builder in [typical(), awkward()] {
            let input = builder.build().unwrap();
            let plan = uninstall_plan(&input);
            let layout = input.layout();
            let protected: Vec<String> = std::iter::once(layout.data_dir())
                .chain(layout.extra_dirs().iter().map(|p| p.as_path()))
                .chain(std::iter::once(layout.home_dir()))
                .map(|p| p.display().to_string())
                .collect();
            for step in &plan.steps {
                let Some(command) = &step.command else {
                    continue;
                };
                assert!(!command.contains("rm -r"), "{command}");
                assert!(!command.contains("userdel"), "{command}");
                assert!(!command.contains("deluser"), "{command}");
                for path in &protected {
                    assert!(!command.contains(path.as_str()), "{command} touches {path}");
                }
                // Only the files install wrote are removed.
                for word in command.split_whitespace() {
                    if word.starts_with('/') {
                        assert!(
                            [UNIT_PATH, ENV_PATH, CADDY_PATH, ENV_DIR].contains(&word),
                            "{command}"
                        );
                    }
                }
            }
            // The destructive commands exist, but only in the separate list.
            let destructive: Vec<&str> = plan
                .optional_destructive
                .iter()
                .filter_map(|s| s.command.as_deref())
                .collect();
            for path in &protected[..protected.len() - 1] {
                assert!(destructive.contains(&format!("rm -rf {path}").as_str()));
            }
            assert!(destructive.iter().any(|c| c.starts_with("userdel")));
            let text = plan.to_string();
            let (safe, dangerous) = text.split_once("Optional and destructive").unwrap();
            assert!(!safe.contains("rm -rf") && !safe.contains("userdel"));
            assert!(dangerous.contains("rm -rf"));
        }
    }

    #[test]
    fn the_secret_never_shows_in_debug() {
        let input = awkward().build().unwrap();
        let plan = install_plan(&input, Path::new("/tmp/stage")).unwrap();
        let debug = format!("{plan:?}");
        assert!(!debug.contains("s3cr"), "{debug}");
        assert!(!format!("{:?}", uninstall_plan(&input)).contains("s3cr"));
        assert!(!render_steps(&plan.steps).contains("s3cr"));
        // But it is in the file itself.
        assert!(plan.files[1].content.contains("s3cr"));
        assert!(!AWKWARD_SECRET.is_empty());
    }

    #[test]
    fn shell_quote_table() {
        for (word, quoted) in [
            ("/tmp/stage/calin.env", "/tmp/stage/calin.env"),
            ("a b", "'a b'"),
            ("it's", "'it'\\''s'"),
            ("$HOME", "'$HOME'"),
            ("", "''"),
        ] {
            assert_eq!(shell_quote(word), quoted);
        }
    }
}
