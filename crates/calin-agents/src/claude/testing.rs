//! A stand-in for the `claude` CLI, for tests that need a real process.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;
use tempfile::TempDir;

use super::ClaudeConfig;

/// Plays the file `play` step by step. It answers `--version`, and records
/// its process id, directory, arguments and every stdin line it reads, all
/// next to itself.
const SCRIPT: &str = r#"#!/bin/sh
dir=$(dirname "$0")
if [ "$1" = "--version" ]; then
  echo "2.1.332 (Claude Code)"
  exit 0
fi
echo $$ > "$dir/pid"
pwd > "$dir/cwd"
for arg in "$@"; do printf '%s\n' "$arg"; done > "$dir/args"
while IFS= read -r step <&3; do
  case "$step" in
    "O "*) printf '%s\n' "${step#O }" ;;
    "I") IFS= read -r line || exit 0; printf '%s\n' "$line" >> "$dir/stdin" ;;
    "E "*) printf '%s\n' "${step#E }" >&2 ;;
    "X "*) exit "${step#X }" ;;
    "S") sleep 1000 & echo $! > "$dir/child" ;;
    "H") trap '' TERM; while :; do sleep 1; done ;;
  esac
done 3< "$dir/play"
while IFS= read -r line; do printf '%s\n' "$line" >> "$dir/stdin"; done
exit 0
"#;

/// One step of the fake CLI's script.
#[derive(Clone, Debug)]
pub(crate) enum Step {
    /// Print this line on stdout.
    Out(String),
    /// Wait for one line on stdin; exit quietly if stdin closes instead.
    In,
    /// Print this line on stderr.
    Stderr(String),
    /// Exit with this status.
    Exit(i32),
    /// Leave a child process running in the background.
    Spawn,
    /// Ignore the request to terminate and a closed stdin, forever.
    Hang,
}

impl Step {
    pub(crate) fn out(message: &Value) -> Self {
        Self::Out(message.to_string())
    }
}

/// A fake CLI in a temporary directory of its own.
pub(crate) struct FakeClaude {
    dir: TempDir,
}

impl FakeClaude {
    /// After the last step the fake reads stdin to its end and exits with 0.
    pub(crate) fn new(steps: &[Step]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let play: String = steps
            .iter()
            .map(|step| match step {
                Step::Out(line) => {
                    assert!(!line.contains('\n'), "one line per step");
                    format!("O {line}\n")
                }
                Step::In => "I\n".to_owned(),
                Step::Stderr(line) => format!("E {line}\n"),
                Step::Exit(status) => format!("X {status}\n"),
                Step::Spawn => "S\n".to_owned(),
                Step::Hang => "H\n".to_owned(),
            })
            .collect();
        std::fs::write(dir.path().join("play"), play).unwrap();
        write_executable(&dir.path().join("claude"), SCRIPT);
        Self { dir }
    }

    /// The steps of a recording: print what the CLI printed, wait where it was written to.
    pub(crate) fn replaying(fixture: &Path) -> Self {
        Self::new(&steps_of(fixture))
    }

    pub(crate) fn bin(&self) -> PathBuf {
        self.dir.path().join("claude")
    }

    pub(crate) fn config(&self) -> ClaudeConfig {
        ClaudeConfig {
            bin: Some(self.bin()),
            ..ClaudeConfig::default()
        }
    }

    /// A directory an agent can work in.
    pub(crate) fn workdir(&self) -> PathBuf {
        let path = self.dir.path().join("work");
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    /// Every line the fake has read from stdin so far, parsed.
    pub(crate) fn stdin(&self) -> Vec<Value> {
        self.lines("stdin")
            .iter()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    /// The arguments the fake was started with.
    pub(crate) fn args(&self) -> Vec<String> {
        self.lines("args")
    }

    /// The directory the fake was started in.
    pub(crate) fn cwd(&self) -> PathBuf {
        PathBuf::from(&self.lines("cwd")[0])
    }

    /// The fake's process id, which is also its process group.
    pub(crate) fn pid(&self) -> i32 {
        self.lines("pid")[0].parse().unwrap()
    }

    /// The process a `Spawn` step left behind.
    pub(crate) fn child_pid(&self) -> i32 {
        self.lines("child")[0].parse().unwrap()
    }

    fn lines(&self, file: &str) -> Vec<String> {
        std::fs::read_to_string(self.dir.path().join(file))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

/// The steps that replay a recording.
pub(crate) fn steps_of(fixture: &Path) -> Vec<Step> {
    std::fs::read_to_string(fixture)
        .unwrap()
        .lines()
        .map(|line| {
            let record: Value = serde_json::from_str(line).unwrap();
            match record["dir"].as_str() {
                Some("in") => Step::In,
                Some("out") => Step::out(&record["msg"]),
                other => panic!("unknown direction {other:?}"),
            }
        })
        .collect()
}

/// Writes an executable without this process ever holding it open for
/// writing. Otherwise a test that forks at the wrong moment passes the open
/// file on to its child, and running the script fails with "text file busy".
pub(crate) fn write_executable(path: &Path, content: &str) {
    let mut writer = Command::new("/bin/sh")
        .args(["-c", r#"cat > "$0" && chmod 755 "$0""#])
        .arg(path)
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = writer.stdin.take().unwrap();
    stdin.write_all(content.as_bytes()).unwrap();
    drop(stdin);
    assert!(writer.wait().unwrap().success());
}
