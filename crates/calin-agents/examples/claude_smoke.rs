//! Runs the Claude runner against the real CLI for one prompt and prints every
//! output. Permission requests are answered at once, with allow unless told
//! otherwise. This spends tokens: pick a cheap model and a throwaway directory.
//!
//! ```text
//! cargo run -p calin-agents --example claude_smoke -- \
//!     --cwd /tmp/scratch --model haiku "Read note.txt and write its first line to out.txt"
//! ```
//!
//! Options: `--cwd DIR` (required), `--model NAME`, `--mode ask|accept_edits|autonomous`,
//! `--resume CONVERSATION`, `--deny`, `--interrupt-after SECONDS`, `--stop-after SECONDS`.
//! The binary is `claude` on `PATH`, or `CALIN_CLAUDE_BIN`.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use calin_agents::{
    ClaudeConfig, ClaudeRunner, Exit, PermissionAsk, Runner, RunnerOutput, StartParams,
};
use calin_proto::{PermissionDecision, PermissionMode};

/// Longest line printed per output; a transcript is for reading, not for replay.
const WIDTH: usize = 220;

struct Options {
    params: StartParams,
    decision: PermissionDecision,
    interrupt_after: Option<Duration>,
    stop_after: Option<Duration>,
}

fn options() -> Result<Options, String> {
    let mut params = StartParams {
        session_id: "smoke".into(),
        agent_id: "smoke".into(),
        start_seq: 1,
        ..StartParams::default()
    };
    let mut options = Options {
        params: params.clone(),
        decision: PermissionDecision::Allow,
        interrupt_after: None,
        stop_after: None,
    };
    let mut cwd = None;
    let mut prompt = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or(format!("{arg} needs a value"));
        let seconds = |text: String| {
            text.parse()
                .map(Duration::from_secs_f64)
                .map_err(|_| format!("{text} is not a number of seconds"))
        };
        match arg.as_str() {
            "--cwd" => cwd = Some(PathBuf::from(value()?)),
            "--model" => params.model = Some(value()?),
            "--resume" => params.resume = Some(value()?),
            "--mode" => {
                params.permission_mode = match value()?.as_str() {
                    "ask" => PermissionMode::Ask,
                    "accept_edits" => PermissionMode::AcceptEdits,
                    "autonomous" => PermissionMode::Autonomous,
                    other => return Err(format!("unknown mode {other}")),
                }
            }
            "--deny" => options.decision = PermissionDecision::Deny,
            "--interrupt-after" => options.interrupt_after = Some(seconds(value()?)?),
            "--stop-after" => options.stop_after = Some(seconds(value()?)?),
            flag if flag.starts_with("--") => return Err(format!("unknown option {flag}")),
            _ => prompt.push(arg),
        }
    }
    params.cwd = cwd.ok_or("--cwd DIR is required")?;
    params.prompt = prompt.join(" ");
    if params.prompt.trim().is_empty() {
        return Err("a prompt is required".into());
    }
    options.params = params;
    Ok(options)
}

fn describe(output: &RunnerOutput) -> String {
    let line = match output {
        RunnerOutput::Event(body) => match serde_json::to_string(body) {
            Ok(json) => format!("event {json}"),
            Err(err) => format!("event that does not serialize: {err}"),
        },
        RunnerOutput::PermissionRequest(PermissionAsk { request, .. }) => format!(
            "permission_request {} {} {:?} item={:?}",
            request.id, request.tool_name, request.title, request.item_id
        ),
        RunnerOutput::NativeSession { id, model } => format!("native_session {id} model={model:?}"),
        RunnerOutput::TurnEnded => "turn_ended".to_owned(),
        RunnerOutput::Exited(exit) => format!("exited {exit:?}"),
    };
    if line.chars().count() <= WIDTH {
        return line;
    }
    let mut cut: String = line.chars().take(WIDTH).collect();
    cut.push('…');
    cut
}

/// Resolves when `after` has passed, or never when there is no such deadline.
async fn elapsed(after: Option<Duration>) {
    match after {
        Some(after) => tokio::time::sleep(after).await,
        None => std::future::pending().await,
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let options = match options() {
        Ok(options) => options,
        Err(message) => {
            eprintln!("error: {message}");
            return ExitCode::from(2);
        }
    };
    let runner = ClaudeRunner::new(ClaudeConfig::from_env());
    match runner.probe() {
        Some(version) => println!("# claude {version}"),
        None => {
            eprintln!("error: the claude CLI is not available");
            return ExitCode::FAILURE;
        }
    }
    let mut handle = match runner.start(options.params) {
        Ok(handle) => handle,
        Err(err) => {
            eprintln!("error: {err}");
            return ExitCode::FAILURE;
        }
    };

    let interrupt = elapsed(options.interrupt_after);
    let stop = elapsed(options.stop_after);
    tokio::pin!(interrupt, stop);
    let (mut interrupted, mut stopped) = (false, false);
    loop {
        let output = tokio::select! {
            output = handle.next_output() => output,
            () = &mut interrupt, if !interrupted => {
                interrupted = true;
                println!("# interrupt");
                let _ = handle.interrupt();
                continue;
            }
            () = &mut stop, if !stopped => {
                stopped = true;
                println!("# stop");
                let _ = handle.stop();
                continue;
            }
        };
        let Some(output) = output else {
            eprintln!("error: the agent ended without an exit");
            return ExitCode::FAILURE;
        };
        println!("{}", describe(&output));
        match output {
            RunnerOutput::PermissionRequest(PermissionAsk { request, .. }) => {
                println!("# {:?} {}", options.decision, request.id);
                let _ = handle.resolve_permission(request.id, options.decision, None);
            }
            // One prompt, one turn: then the agent is asked to leave.
            RunnerOutput::TurnEnded if !stopped => {
                stopped = true;
                println!("# stop");
                let _ = handle.stop();
            }
            RunnerOutput::Exited(Exit::Ok) => return ExitCode::SUCCESS,
            RunnerOutput::Exited(Exit::Error(_)) => return ExitCode::FAILURE,
            _ => {}
        }
    }
}
