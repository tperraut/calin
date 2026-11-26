use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// What to run in a new terminal, and where.
#[derive(Clone, Debug)]
pub struct TerminalConfig {
    /// Directory the program starts in. It must exist.
    pub cwd: PathBuf,
    pub cols: u16,
    pub rows: u16,
    /// `None` runs the user's shell interactively: `$SHELL`, else `/bin/bash`,
    /// else `/bin/sh`.
    pub program: Option<OsString>,
    /// Arguments of `program`; unused when `program` is `None`.
    pub args: Vec<OsString>,
    /// Added on top of the environment inherited from this process. `TERM` and
    /// `COLORTERM` are always overridden.
    pub env: Vec<(OsString, OsString)>,
}

impl TerminalConfig {
    /// The user's shell in `cwd`, 80 columns by 24 rows.
    pub fn new(cwd: impl Into<PathBuf>) -> Self {
        Self {
            cwd: cwd.into(),
            cols: 80,
            rows: 24,
            program: None,
            args: Vec::new(),
            env: Vec::new(),
        }
    }

    #[must_use]
    pub fn size(mut self, cols: u16, rows: u16) -> Self {
        self.cols = cols;
        self.rows = rows;
        self
    }

    /// Run `program` with `args` instead of the user's shell.
    #[must_use]
    pub fn command<I, S>(mut self, program: impl Into<OsString>, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.program = Some(program.into());
        self.args = args.into_iter().map(Into::into).collect();
        self
    }

    #[must_use]
    pub fn env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    /// The program and arguments to execute.
    pub(crate) fn command_line(&self) -> (OsString, Vec<OsString>) {
        match &self.program {
            Some(program) => (program.clone(), self.args.clone()),
            None => (
                default_shell(std::env::var_os("SHELL")).into(),
                vec!["-i".into()],
            ),
        }
    }
}

fn default_shell(shell: Option<OsString>) -> PathBuf {
    let bash = Path::new("/bin/bash");
    match shell.map(PathBuf::from) {
        // A stale `$SHELL` (shell uninstalled, container image) must not make
        // every terminal fail to start.
        Some(shell) if shell.is_file() => shell,
        _ if bash.is_file() => bash.to_path_buf(),
        _ => PathBuf::from("/bin/sh"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_shell_prefers_an_existing_shell_variable() {
        assert_eq!(
            default_shell(Some("/bin/sh".into())),
            PathBuf::from("/bin/sh")
        );
    }

    #[test]
    fn default_shell_falls_back_when_the_variable_is_unusable() {
        let fallback = if Path::new("/bin/bash").is_file() {
            "/bin/bash"
        } else {
            "/bin/sh"
        };
        for shell in [None, Some("".into()), Some("/nonexistent/shell".into())] {
            assert_eq!(default_shell(shell), PathBuf::from(fallback));
        }
    }

    #[test]
    fn default_command_is_an_interactive_shell() {
        let (program, args) = TerminalConfig::new("/").command_line();
        assert!(Path::new(&program).is_file());
        assert_eq!(args, vec![OsString::from("-i")]);
    }

    #[test]
    fn explicit_command_is_used_verbatim() {
        let config = TerminalConfig::new("/").command("/bin/sh", ["-c", "true"]);
        let (program, args) = config.command_line();
        assert_eq!(program, OsString::from("/bin/sh"));
        assert_eq!(args, vec![OsString::from("-c"), OsString::from("true")]);
    }
}
