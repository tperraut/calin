use std::path::{Path, PathBuf};

use calin_proto::{
    EventBody, ItemContent, ItemStatus, PermissionDecision, PermissionMode, PermissionRequest,
};
use serde_json::json;

use super::*;
use crate::claude::testing::{FakeClaude, Step, steps_of};
use crate::claude::{ClaudeConfig, ClaudeRunner};
use crate::runner::{Runner, StartParams};

/// Generous: a loaded machine must not turn a slow test into a failed one.
const TIMEOUT: Duration = Duration::from_secs(20);

const START_SEQ: u64 = 7;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/claude/2.1.332")
        .join(format!("{name}.jsonl"))
}

fn records(fixture: &Path, dir: &str) -> Vec<Value> {
    std::fs::read_to_string(fixture)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|record| record["dir"] == dir)
        .map(|record| record["msg"].clone())
        .collect()
}

/// The text of the `turn`-th user message of a recording.
fn prompt(fixture: &Path, turn: usize) -> String {
    let users: Vec<Value> = records(fixture, "in")
        .into_iter()
        .filter(|message| message["type"] == "user")
        .collect();
    users[turn]["message"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// What the mapper alone makes of a recording's output, for an agent in `cwd`.
fn mapped(fixture: &Path, cwd: &Path) -> Vec<RunnerOutput> {
    let mut mapper = Mapper::new(cwd.to_owned(), format!("claude-{START_SEQ}"));
    let mut outputs = Vec::new();
    for message in records(fixture, "out") {
        outputs.extend(mapper.message(&message));
    }
    outputs
}

fn params(fake: &FakeClaude, prompt: &str) -> StartParams {
    StartParams {
        session_id: "s1".into(),
        agent_id: "a1".into(),
        prompt: prompt.into(),
        cwd: fake.workdir(),
        start_seq: START_SEQ,
        ..StartParams::default()
    }
}

fn start(fake: &FakeClaude, prompt: &str) -> AgentHandle {
    start_with(fake.config(), params(fake, prompt))
}

fn start_with(config: ClaudeConfig, params: StartParams) -> AgentHandle {
    ClaudeRunner::new(config).start(params).unwrap()
}

async fn next(handle: &mut AgentHandle) -> Option<RunnerOutput> {
    timeout(TIMEOUT, handle.next_output())
        .await
        .expect("the agent went quiet")
}

/// Everything up to and including `TurnEnded`. `on_request` answers the
/// permission requests on the way.
async fn turn(
    handle: &mut AgentHandle,
    mut on_request: impl FnMut(&AgentHandle, &PermissionRequest),
) -> Vec<RunnerOutput> {
    let mut outputs = Vec::new();
    loop {
        let output = next(handle).await.expect("the agent ended mid-turn");
        if let RunnerOutput::PermissionRequest(request) = &output {
            on_request(handle, request);
        }
        let ended = output == RunnerOutput::TurnEnded;
        assert!(
            !matches!(output, RunnerOutput::Exited(_)),
            "the agent exited mid-turn: {output:?} after {outputs:?}"
        );
        outputs.push(output);
        if ended {
            return outputs;
        }
    }
}

/// Everything up to `Exited`, which must be the last output, and the exit.
async fn until_exit(handle: &mut AgentHandle) -> (Vec<RunnerOutput>, Exit) {
    let mut outputs = Vec::new();
    loop {
        match next(handle).await.expect("the agent ended without an exit") {
            RunnerOutput::Exited(exit) => {
                assert_eq!(next(handle).await, None, "outputs after the exit");
                return (outputs, exit);
            }
            output => outputs.push(output),
        }
    }
}

fn allow(handle: &AgentHandle, request: &PermissionRequest) {
    handle
        .resolve_permission(request.id.clone(), PermissionDecision::Allow, None)
        .unwrap();
}

fn no_requests(_: &AgentHandle, request: &PermissionRequest) {
    panic!("unexpected request {request:?}");
}

fn statuses(outputs: &[RunnerOutput]) -> Vec<(String, ItemStatus)> {
    outputs
        .iter()
        .filter_map(|output| match output {
            RunnerOutput::Event(EventBody::ItemCompleted { item }) => match &item.content {
                ItemContent::Tool { name, .. } => Some((name.clone(), item.status)),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// True while the process exists and is not a zombie waiting to be reaped.
fn alive(pid: i32) -> bool {
    if let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        // "pid (name) state ...": the name may hold spaces and parentheses.
        let state = stat
            .rsplit(')')
            .next()
            .and_then(|rest| rest.trim().chars().next());
        return !matches!(state, Some('Z' | 'X') | None);
    }
    if Path::new("/proc/self/stat").exists() {
        return false;
    }
    // No procfs here: ask the kernel, which also counts zombies.
    rustix::process::Pid::from_raw(pid)
        .is_some_and(|pid| rustix::process::test_kill_process(pid).is_ok())
}

async fn gone(pids: &[i32]) {
    let all_gone = async {
        while pids.iter().any(|pid| alive(*pid)) {
            sleep(Duration::from_millis(10)).await;
        }
    };
    timeout(TIMEOUT, all_gone)
        .await
        .unwrap_or_else(|_| panic!("processes {pids:?} are still running"));
}

fn init() -> Step {
    Step::out(&json!({
        "type": "system", "subtype": "init", "session_id": "conversation-1", "model": "m",
    }))
}

fn bash_call() -> Step {
    Step::out(&json!({
        "type": "assistant", "parent_tool_use_id": null,
        "message": { "id": "msg_1", "content": [{
            "type": "tool_use", "id": "toolu_1", "name": "Bash", "input": { "command": "make" },
        }] },
    }))
}

fn success() -> Step {
    Step::out(&json!({ "type": "result", "subtype": "success", "is_error": false }))
}

#[tokio::test]
async fn two_turns_run_in_one_process_and_a_permission_is_allowed() {
    let recording = fixture("two-turns-with-permission");
    let fake = FakeClaude::replaying(&recording);
    let mut handle = start(&fake, &prompt(&recording, 0));
    // Sent at once, in the middle of the first turn: it must wait its turn.
    handle.send_message(prompt(&recording, 1)).unwrap();
    // Nobody waits for this decision; nothing is written for it.
    handle
        .resolve_permission("no-such-request", PermissionDecision::Deny, None)
        .unwrap();

    let mut requests = 0;
    let mut outputs = turn(&mut handle, |handle, request| {
        requests += 1;
        allow(handle, request);
    })
    .await;
    outputs.extend(turn(&mut handle, no_requests).await);
    assert_eq!(requests, 1);
    assert_eq!(outputs, mapped(&recording, &fake.workdir()));
    assert_eq!(
        statuses(&outputs),
        [
            ("Write".to_owned(), ItemStatus::Completed),
            ("Bash".to_owned(), ItemStatus::Failed)
        ]
    );

    handle.stop().unwrap();
    assert_eq!(until_exit(&mut handle).await, (vec![], Exit::Ok));
    // The CLI read exactly what the recording wrote: the two prompts in
    // order, one per turn, and the allow with the request's own input.
    assert_eq!(fake.stdin(), records(&recording, "in"));
    let allowed = &fake.stdin()[1]["response"];
    assert_eq!(allowed["subtype"], "success");
    assert_eq!(
        allowed["response"],
        json!({
            "behavior": "allow",
            "updatedInput": { "content": "alpha\n", "file_path": "/work/made.txt" },
        })
    );
}

#[tokio::test]
async fn a_denial_reaches_the_cli_and_the_call_is_declined() {
    let recording = fixture("deny-permission");
    let fake = FakeClaude::replaying(&recording);
    let mut handle = start(&fake, &prompt(&recording, 0));
    let outputs = turn(&mut handle, |handle, request| {
        handle
            .resolve_permission(
                request.id.clone(),
                PermissionDecision::Deny,
                Some("Not in this directory.".into()),
            )
            .unwrap();
    })
    .await;
    assert_eq!(outputs, mapped(&recording, &fake.workdir()));
    assert_eq!(
        statuses(&outputs),
        [
            ("Write".to_owned(), ItemStatus::Declined),
            ("Write".to_owned(), ItemStatus::Declined)
        ]
    );

    handle.stop().unwrap();
    assert_eq!(until_exit(&mut handle).await.1, Exit::Ok);
    assert_eq!(fake.stdin(), records(&recording, "in"));
    for denied in &fake.stdin()[1..] {
        assert_eq!(denied["type"], "control_response");
        assert_eq!(
            denied["response"]["response"],
            json!({ "behavior": "deny", "message": "Not in this directory." })
        );
    }
}

#[tokio::test]
async fn interrupt_ends_the_turn_and_the_process_takes_the_next_one() {
    let recording = fixture("interrupt");
    let fake = FakeClaude::replaying(&recording);
    let mut handle = start(&fake, &prompt(&recording, 0));
    // The fake reads the interrupt where the recording wrote it: while the
    // allowed command runs.
    let first = turn(&mut handle, |handle, request| {
        allow(handle, request);
        handle.interrupt().unwrap();
    })
    .await;
    assert_eq!(statuses(&first), [("Bash".to_owned(), ItemStatus::Failed)]);
    assert!(
        !first
            .iter()
            .any(|output| matches!(output, RunnerOutput::Event(EventBody::Error { .. }))),
        "an interrupted turn is not a failed one"
    );

    // Between turns there is nothing to interrupt, and nothing is written.
    handle.interrupt().unwrap();
    handle.send_message(prompt(&recording, 1)).unwrap();
    let second = turn(&mut handle, no_requests).await;
    let all: Vec<RunnerOutput> = first.into_iter().chain(second).collect();
    assert_eq!(all, mapped(&recording, &fake.workdir()));

    handle.stop().unwrap();
    assert_eq!(until_exit(&mut handle).await, (vec![], Exit::Ok));
    let mut expected = records(&recording, "in");
    assert_eq!(expected[2]["request"]["subtype"], "interrupt");
    expected[2]["request_id"] = json!("calin-interrupt-1");
    assert_eq!(fake.stdin(), expected);
}

#[tokio::test]
async fn the_cli_is_started_as_the_start_asks() {
    let fake = FakeClaude::new(&[]);
    let other = tempfile::tempdir().unwrap();
    let mut handle = start_with(
        fake.config(),
        StartParams {
            permission_mode: PermissionMode::AcceptEdits,
            model: Some("haiku".into()),
            resume: Some("conversation-0".into()),
            add_dirs: vec![other.path().to_owned()],
            ..params(&fake, "hello")
        },
    );
    handle.stop().unwrap();
    assert_eq!(until_exit(&mut handle).await, (vec![], Exit::Ok));

    let expected = format!(
        "-p --input-format stream-json --output-format stream-json --verbose \
         --include-partial-messages --permission-prompt-tool stdio --permission-mode acceptEdits \
         --model haiku --resume conversation-0 --add-dir {}",
        other.path().display()
    );
    assert_eq!(fake.args().join(" "), expected);
    assert_eq!(
        fake.cwd().canonicalize().unwrap(),
        fake.workdir().canonicalize().unwrap()
    );
    // The prompt, then the stop: the unfinished turn is interrupted before
    // stdin closes, so that the CLI winds down instead of being cut off.
    assert_eq!(
        fake.stdin(),
        [
            json!({
                "type": "user",
                "message": { "role": "user", "content": [{ "type": "text", "text": "hello" }] },
            }),
            json!({
                "type": "control_request",
                "request_id": "calin-interrupt-1",
                "request": { "subtype": "interrupt" },
            }),
        ]
    );
}

#[tokio::test]
async fn stop_kills_the_whole_group_of_a_cli_that_will_not_exit() {
    let fake = FakeClaude::new(&[Step::In, init(), Step::Spawn, bash_call(), Step::Hang]);
    let mut handle = start_with(
        ClaudeConfig {
            stop_grace: Duration::from_millis(100),
            ..fake.config()
        },
        params(&fake, "run make"),
    );
    assert!(matches!(
        next(&mut handle).await,
        Some(RunnerOutput::NativeSession { .. })
    ));
    assert!(matches!(
        next(&mut handle).await,
        Some(RunnerOutput::Event(EventBody::ItemStarted { .. }))
    ));
    let pids = [fake.pid(), fake.child_pid()];
    assert!(pids.iter().all(|pid| alive(*pid)));

    handle.stop().unwrap();
    let (outputs, exit) = until_exit(&mut handle).await;
    // The call that was running is closed, and a stop is not a failure.
    assert_eq!(
        statuses(&outputs),
        [("Bash".to_owned(), ItemStatus::Failed)]
    );
    assert_eq!(outputs.len(), 1);
    assert_eq!(exit, Exit::Ok);
    gone(&pids).await;
    assert!(matches!(
        handle.send_message("late"),
        Err(RunnerError::Gone)
    ));
}

#[tokio::test]
async fn dropping_the_handle_kills_the_group() {
    let fake = FakeClaude::new(&[Step::In, Step::Spawn, init(), Step::Hang]);
    let mut handle = start(&fake, "work");
    assert!(matches!(
        next(&mut handle).await,
        Some(RunnerOutput::NativeSession { .. })
    ));
    let pids = [fake.pid(), fake.child_pid()];
    assert!(pids.iter().all(|pid| alive(*pid)));
    drop(handle);
    gone(&pids).await;
}

#[tokio::test]
async fn an_unexpected_exit_closes_the_items_and_reports_stderr() {
    let fake = FakeClaude::new(&[
        Step::In,
        init(),
        bash_call(),
        Step::Stderr("fatal: the model melted".into()),
        Step::Exit(3),
    ]);
    let mut handle = start(&fake, "run make");
    let (outputs, exit) = until_exit(&mut handle).await;
    assert_eq!(outputs.len(), 3, "{outputs:?}");
    assert_eq!(
        statuses(&outputs),
        [("Bash".to_owned(), ItemStatus::Failed)]
    );
    assert_eq!(exit, Exit::Error("fatal: the model melted".into()));
}

#[tokio::test]
async fn a_silent_death_is_described_by_its_exit_status() {
    let fake = FakeClaude::new(&[Step::In, Step::Exit(9)]);
    let mut handle = start(&fake, "hello");
    let (outputs, exit) = until_exit(&mut handle).await;
    assert_eq!(outputs, []);
    assert_eq!(
        exit,
        Exit::Error("claude ended unexpectedly (exit status: 9)".into())
    );
}

#[tokio::test]
async fn a_failed_resume_ends_the_turn_then_the_agent() {
    // The real CLI prints the error result, repeats it on stderr and exits.
    let recording = fixture("resume-missing");
    let said = "No conversation found with session ID: 00000000-0000-4000-8000-000000000000";
    let mut steps = steps_of(&recording);
    steps.extend([Step::Stderr(said.into()), Step::Exit(1)]);
    let fake = FakeClaude::new(&steps);
    let mut handle = start(&fake, &prompt(&recording, 0));

    let outputs = turn(&mut handle, no_requests).await;
    assert!(matches!(
        &outputs[outputs.len() - 2],
        RunnerOutput::Event(EventBody::Error { message })
            if message.starts_with("No conversation found")
    ));
    assert_eq!(
        until_exit(&mut handle).await,
        (vec![], Exit::Error(said.into()))
    );
}

#[tokio::test]
async fn a_flood_on_stderr_neither_blocks_the_cli_nor_the_report() {
    // Far more than a pipe holds: a CLI nobody reads from would stall here.
    let noise = "warning: ".to_owned() + &"x".repeat(200);
    let mut steps = vec![Step::In];
    steps.extend(std::iter::repeat_n(Step::Stderr(noise), 1000));
    steps.extend([init(), Step::Stderr("the last words".into()), Step::Exit(1)]);
    let fake = FakeClaude::new(&steps);
    let mut handle = start(&fake, "hello");

    let (outputs, exit) = until_exit(&mut handle).await;
    assert_eq!(outputs.len(), 1, "the line after the flood arrived");
    let Exit::Error(message) = exit else {
        panic!("expected a failure");
    };
    assert!(message.ends_with("the last words"), "{message}");
    assert!(message.len() <= STDERR_TAIL);
}

#[tokio::test]
async fn a_turn_the_cli_took_by_itself_ends_no_turn_of_ours() {
    let fake = FakeClaude::new(&[Step::In, init(), success(), init(), success()]);
    let mut handle = start(&fake, "hello");
    let first = turn(&mut handle, no_requests).await;
    assert_eq!(first.len(), 3, "conversation, usage, end: {first:?}");

    handle.stop().unwrap();
    let (rest, exit) = until_exit(&mut handle).await;
    assert_eq!(exit, Exit::Ok);
    assert!(
        matches!(rest[..], [RunnerOutput::Event(EventBody::Usage { .. })]),
        "the second result's usage, but no second end: {rest:?}"
    );
}

#[tokio::test]
async fn a_cli_that_cannot_be_started_is_a_start_error() {
    let fake = FakeClaude::new(&[]);
    let missing = ClaudeConfig {
        bin: Some("/nonexistent/calin/claude".into()),
        ..ClaudeConfig::default()
    };
    let error = ClaudeRunner::new(missing)
        .start(params(&fake, "hello"))
        .unwrap_err();
    assert!(
        matches!(&error, RunnerError::Start(message) if message.contains("/nonexistent/calin/claude")),
        "{error}"
    );

    let error = ClaudeRunner::new(fake.config())
        .start(StartParams {
            cwd: fake.workdir().join("not-there"),
            ..params(&fake, "hello")
        })
        .unwrap_err();
    assert!(
        matches!(&error, RunnerError::Start(message) if message.contains("not-there")),
        "{error}"
    );
}
