//! The Claude Code runner: one long-lived `claude -p` process per agent,
//! spoken to over its stream-json protocol.

mod mapper;
mod process;
#[cfg(all(test, unix))]
pub(crate) mod testing;
mod tools;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use calin_proto::PermissionMode;

use crate::runner::{AgentHandle, Runner, RunnerError, StartParams};

pub const CLAUDE_RUNNER_ID: &str = "claude";

/// Path of the CLI to run instead of `claude` on `PATH`.
const ENV_BIN: &str = "CALIN_CLAUDE_BIN";

const DEFAULT_BIN: &str = "claude";

/// Shorter than the orchestrator's own patience with a stopping agent, so
/// that `Exited` still reaches it.
const DEFAULT_STOP_GRACE: Duration = Duration::from_secs(3);

/// `claude --version` answers in milliseconds; anything slower is not it.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaudeConfig {
    /// The CLI to run. `None` means `claude`, looked up on `PATH`.
    pub bin: Option<PathBuf>,
    /// The model of agents whose start names none. `None` keeps the CLI's own default.
    pub model: Option<String>,
    /// How long a stopped agent has to exit after its input closed, before it is killed.
    pub stop_grace: Duration,
}

impl Default for ClaudeConfig {
    fn default() -> Self {
        Self {
            bin: None,
            model: None,
            stop_grace: DEFAULT_STOP_GRACE,
        }
    }
}

impl ClaudeConfig {
    /// Reads `CALIN_CLAUDE_BIN`. Only the binary's wiring calls this;
    /// everything else receives a config, so tests never touch the process
    /// environment.
    pub fn from_env() -> Self {
        Self::from_values(std::env::var_os(ENV_BIN))
    }

    fn from_values(bin: Option<OsString>) -> Self {
        Self {
            bin: bin.filter(|path| !path.is_empty()).map(PathBuf::from),
            ..Self::default()
        }
    }

    /// The program to execute: the configured path, else `claude` on `PATH`.
    pub fn binary(&self) -> &Path {
        self.bin.as_deref().unwrap_or(Path::new(DEFAULT_BIN))
    }
}

#[derive(Clone, Debug)]
pub struct ClaudeRunner {
    config: ClaudeConfig,
}

impl ClaudeRunner {
    pub fn new(config: ClaudeConfig) -> Self {
        Self { config }
    }

    /// The version of the CLI, or `None` when it cannot be run. Runs
    /// `claude --version` and blocks for it: a few milliseconds, at startup.
    pub fn probe(&self) -> Option<String> {
        let mut child = Command::new(self.config.binary())
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let deadline = Instant::now() + PROBE_TIMEOUT;
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
            }
        }
        let output = child.wait_with_output().ok()?;
        if !output.status.success() {
            return None;
        }
        parse_version(&String::from_utf8_lossy(&output.stdout))
    }
}

impl Runner for ClaudeRunner {
    fn id(&self) -> &str {
        CLAUDE_RUNNER_ID
    }

    fn model(&self) -> Option<String> {
        self.config.model.clone()
    }

    fn start(&self, params: StartParams) -> Result<AgentHandle, RunnerError> {
        if !params.cwd.is_dir() {
            return Err(RunnerError::Start(format!(
                "the working directory {} does not exist",
                params.cwd.display()
            )));
        }
        let launch = process::Launch {
            program: self.config.binary().to_owned(),
            args: command_args(&self.config, &params),
            cwd: params.cwd.clone(),
            stop_grace: self.config.stop_grace,
        };
        let mapper = mapper::Mapper::new(params.cwd, format!("claude-{}", params.start_seq));
        process::spawn(launch, mapper, &params.prompt)
    }
}

/// The CLI's arguments for a long-lived agent that takes its turns on stdin.
fn command_args(config: &ClaudeConfig, params: &StartParams) -> Vec<OsString> {
    let mut args: Vec<OsString> = [
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
        // Permission prompts arrive as control requests on stdout.
        "--permission-prompt-tool",
        "stdio",
        "--permission-mode",
        cli_permission_mode(params.permission_mode),
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    if let Some(model) = params.model.as_ref().or(config.model.as_ref()) {
        args.extend(["--model".into(), model.into()]);
    }
    if let Some(conversation) = &params.resume {
        args.extend(["--resume".into(), conversation.into()]);
    }
    for dir in &params.add_dirs {
        args.extend(["--add-dir".into(), dir.into()]);
    }
    if let Some(url) = &params.tool_endpoint {
        // Verified on CLI 2.1.332: the tools appear as `mcp__calin__<tool>`.
        let config = serde_json::json!({
            "mcpServers": { tools::TOOL_SERVER: { "type": "http", "url": url } },
        });
        args.extend(["--mcp-config".into(), config.to_string().into()]);
    }
    if let Some(text) = params
        .system_prompt
        .as_ref()
        .filter(|text| !text.is_empty())
    {
        args.extend(["--append-system-prompt".into(), text.into()]);
    }
    args
}

/// The CLI mode that asks exactly when Calin's mode wants to be asked.
///
/// `autonomous` still runs the CLI in `default`: every request reaches the
/// orchestrator, which allows it by policy and records it. The CLI's own
/// bypass mode would leave nothing to record.
fn cli_permission_mode(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::Ask | PermissionMode::Autonomous => "default",
        PermissionMode::AcceptEdits => "acceptEdits",
    }
}

/// `2.1.332` out of `2.1.332 (Claude Code)`.
fn parse_version(output: &str) -> Option<String> {
    let version = output.split_whitespace().next()?;
    version
        .starts_with(|c: char| c.is_ascii_digit())
        .then(|| version.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(config: &ClaudeConfig, params: &StartParams) -> Vec<String> {
        command_args(config, params)
            .into_iter()
            .map(|arg| arg.into_string().unwrap())
            .collect()
    }

    #[test]
    fn a_plain_start_uses_the_verified_command_shape() {
        let args = args(&ClaudeConfig::default(), &StartParams::default());
        assert_eq!(
            args.join(" "),
            "-p --input-format stream-json --output-format stream-json --verbose \
             --include-partial-messages --permission-prompt-tool stdio --permission-mode default"
        );
    }

    #[test]
    fn model_resume_and_directories_are_passed_on() {
        let config = ClaudeConfig {
            model: Some("sonnet".into()),
            ..ClaudeConfig::default()
        };
        let mut params = StartParams {
            permission_mode: PermissionMode::AcceptEdits,
            resume: Some("conversation-1".into()),
            add_dirs: vec!["/repos/a".into(), "/repos/b".into()],
            ..StartParams::default()
        };
        let tail = |args: Vec<String>| args[9..].join(" ");
        assert_eq!(
            tail(args(&config, &params)),
            "--permission-mode acceptEdits --model sonnet --resume conversation-1 \
             --add-dir /repos/a --add-dir /repos/b"
        );

        // The start's model wins over the runner's.
        params.model = Some("haiku".into());
        assert!(tail(args(&config, &params)).contains("--model haiku --resume"));
    }

    #[test]
    fn the_tool_endpoint_and_the_system_prompt_are_passed_as_single_arguments() {
        let prompt = "You run inside Calin.\nRepos: \"api\" at /repos/api --not-a-flag";
        let params = StartParams {
            tool_endpoint: Some("http://127.0.0.1:4680/api/v1/mcp/abc123".into()),
            system_prompt: Some(prompt.into()),
            add_dirs: vec!["/repos/a".into()],
            ..StartParams::default()
        };
        let args = args(&ClaudeConfig::default(), &params);
        assert_eq!(
            args[11..],
            [
                "--add-dir",
                "/repos/a",
                "--mcp-config",
                r#"{"mcpServers":{"calin":{"type":"http","url":"http://127.0.0.1:4680/api/v1/mcp/abc123"}}}"#,
                "--append-system-prompt",
                prompt,
            ]
        );
        // The server name is what makes its tools `mcp__calin__<tool>`.
        let config: serde_json::Value = serde_json::from_str(&args[14]).unwrap();
        assert_eq!(config["mcpServers"][tools::TOOL_SERVER]["type"], "http");

        // Without either, nothing is added; an empty prompt is no prompt.
        let bare = StartParams {
            system_prompt: Some(String::new()),
            ..StartParams::default()
        };
        let args = self::args(&ClaudeConfig::default(), &bare);
        assert!(!args.iter().any(|arg| arg == "--mcp-config"));
        assert!(!args.iter().any(|arg| arg == "--append-system-prompt"));
    }

    #[test]
    fn every_calin_mode_has_a_cli_mode_that_still_asks() {
        assert_eq!(cli_permission_mode(PermissionMode::Ask), "default");
        assert_eq!(
            cli_permission_mode(PermissionMode::AcceptEdits),
            "acceptEdits"
        );
        assert_eq!(cli_permission_mode(PermissionMode::Autonomous), "default");
    }

    #[test]
    fn the_binary_comes_from_the_config_then_the_environment_value_then_path() {
        assert_eq!(ClaudeConfig::default().binary(), Path::new("claude"));
        assert_eq!(
            ClaudeConfig::from_values(None).binary(),
            Path::new("claude")
        );
        assert_eq!(
            ClaudeConfig::from_values(Some("".into())).binary(),
            Path::new("claude")
        );
        let from_env = ClaudeConfig::from_values(Some("/opt/claude".into()));
        assert_eq!(from_env.binary(), Path::new("/opt/claude"));
        let explicit = ClaudeConfig {
            bin: Some("/usr/local/bin/claude".into()),
            ..from_env
        };
        assert_eq!(explicit.binary(), Path::new("/usr/local/bin/claude"));
    }

    #[test]
    fn versions_are_read_from_the_cli_banner() {
        assert_eq!(
            parse_version("2.1.332 (Claude Code)\n").as_deref(),
            Some("2.1.332")
        );
        assert_eq!(parse_version("2.1.332").as_deref(), Some("2.1.332"));
        assert_eq!(parse_version("command not found"), None);
        assert_eq!(parse_version(""), None);
    }

    #[cfg(unix)]
    #[test]
    fn the_probe_reports_the_version_of_a_cli_that_runs() {
        let fake = testing::FakeClaude::new(&[]);
        assert_eq!(
            ClaudeRunner::new(fake.config()).probe().as_deref(),
            Some("2.1.332")
        );

        // Something executable that is not the CLI.
        let dir = tempfile::tempdir().unwrap();
        let other = dir.path().join("claude");
        testing::write_executable(&other, "#!/bin/sh\necho 'usage: other [options]'\nexit 2\n");
        let runner = ClaudeRunner::new(ClaudeConfig {
            bin: Some(other),
            ..ClaudeConfig::default()
        });
        assert_eq!(runner.probe(), None);
    }

    #[test]
    fn a_missing_binary_is_not_available() {
        let runner = ClaudeRunner::new(ClaudeConfig {
            bin: Some("/nonexistent/calin/claude".into()),
            ..ClaudeConfig::default()
        });
        assert_eq!(runner.probe(), None);
    }
}
