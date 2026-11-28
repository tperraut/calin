use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use calin_proto::{ItemDelta, PlanStatus};

use super::*;
use crate::runner::ToolAccess;

/// Set to `1` to rewrite the expected files from what the mapper produces now.
const ENV_BLESS: &str = "CALIN_BLESS";

/// The working directory every scrubbed fixture was recorded in.
const FIXTURE_CWD: &str = "/work";

fn mapper() -> Mapper {
    Mapper::new(FIXTURE_CWD.into(), "claude-7".into())
}

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/claude")
}

/// Every recording, as `<cli version>/<name>.jsonl`.
fn fixtures() -> Vec<PathBuf> {
    let mut found = Vec::new();
    for version in std::fs::read_dir(fixtures_dir()).unwrap() {
        let version = version.unwrap().path();
        if !version.is_dir() || version.file_name().unwrap() == "scenarios" {
            continue;
        }
        for file in std::fs::read_dir(&version).unwrap() {
            let file = file.unwrap().path();
            let name = file.file_name().unwrap().to_string_lossy().into_owned();
            if name.ends_with(".jsonl") && !name.ends_with(".expected.jsonl") {
                found.push(file);
            }
        }
    }
    found.sort();
    assert!(found.len() >= 12, "fixtures are missing: {found:?}");
    found
}

fn fixture(name: &str) -> PathBuf {
    let path = fixtures_dir().join("2.1.332").join(format!("{name}.jsonl"));
    assert!(path.is_file(), "no fixture {}", path.display());
    path
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dir {
    /// Written to the CLI's stdin.
    In,
    /// Printed by the CLI.
    Out,
}

fn records(path: &Path) -> Vec<(Dir, Value)> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| {
            let record: Value = serde_json::from_str(line).unwrap();
            let dir = match record["dir"].as_str() {
                Some("in") => Dir::In,
                Some("out") => Dir::Out,
                other => panic!("unknown direction {other:?} in {}", path.display()),
            };
            (dir, record["msg"].clone())
        })
        .collect()
}

/// What the CLI printed, replayed through a fresh mapper, the way the runner
/// sees it: line by line, then the end of the stream.
fn replay(path: &Path) -> Vec<RunnerOutput> {
    let mut mapper = mapper();
    let mut outputs = Vec::new();
    for (dir, message) in records(path) {
        if dir == Dir::Out {
            outputs.extend(mapper.line(&message.to_string()));
        }
    }
    outputs.extend(mapper.finish());
    outputs
}

fn to_json(output: &RunnerOutput) -> Value {
    match output {
        RunnerOutput::Event(body) => json!({ "event": body }),
        RunnerOutput::PermissionRequest(ask) => {
            let access = match &ask.access {
                ToolAccess::Files { paths, write } => json!({ "files": paths, "write": write }),
                ToolAccess::Orchestrator => json!("orchestrator"),
                ToolAccess::Other => json!("other"),
            };
            let mut line = json!({ "permission_request": ask.request, "access": access });
            // Only when a subagent asks, so the lead's lines stay as they were.
            if let Some(key) = &ask.subagent {
                line["subagent"] = json!(key);
            }
            line
        }
        RunnerOutput::SubagentStarted(start) => json!({ "subagent_started": {
            "key": start.key,
            "parent": start.parent,
            "role": start.role,
            "description": start.description,
            "item_id": start.item_id,
        } }),
        RunnerOutput::SubagentEvent { key, body } => {
            json!({ "subagent_event": { "key": key, "event": body } })
        }
        RunnerOutput::SubagentEnded { key, ok } => {
            json!({ "subagent_ended": { "key": key, "ok": ok } })
        }
        RunnerOutput::NativeSession { id, model } => {
            json!({ "native_session": { "id": id, "model": model } })
        }
        RunnerOutput::Limits(windows) => json!({ "limits": windows }),
        RunnerOutput::TurnEnded => json!("turn_ended"),
        RunnerOutput::Exited(exit) => json!({ "exited": format!("{exit:?}") }),
    }
}

/// One line with sorted keys, whatever order the JSON library keeps.
fn canonical(value: &Value) -> String {
    fn sorted(value: &Value) -> Value {
        match value {
            Value::Object(map) => {
                let entries: BTreeMap<&String, Value> = map
                    .iter()
                    .map(|(key, value)| (key, sorted(value)))
                    .collect();
                Value::Object(entries.into_iter().map(|(k, v)| (k.clone(), v)).collect())
            }
            Value::Array(items) => Value::Array(items.iter().map(sorted).collect()),
            other => other.clone(),
        }
    }
    sorted(value).to_string()
}

fn expected_path(fixture: &Path) -> PathBuf {
    fixture.with_extension("expected.jsonl")
}

#[test]
fn every_fixture_maps_to_its_expected_outputs() {
    let bless = std::env::var(ENV_BLESS).is_ok_and(|value| value == "1");
    for path in fixtures() {
        let produced: Vec<Value> = replay(&path).iter().map(to_json).collect();
        let expected_path = expected_path(&path);
        if bless {
            let text: String = produced
                .iter()
                .map(|value| canonical(value) + "\n")
                .collect();
            std::fs::write(&expected_path, text).unwrap();
            continue;
        }
        let expected: Vec<Value> = std::fs::read_to_string(&expected_path)
            .unwrap_or_else(|err| {
                panic!(
                    "{}: {err}; run the tests with {ENV_BLESS}=1 to create it",
                    expected_path.display()
                )
            })
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        for (index, (got, want)) in produced.iter().zip(&expected).enumerate() {
            assert_eq!(
                got,
                want,
                "{} differs at output {}; rerun with {ENV_BLESS}=1 if the change is intended",
                path.display(),
                index + 1
            );
        }
        assert_eq!(
            produced.len(),
            expected.len(),
            "{} yields a different number of outputs",
            path.display()
        );
    }
}

/// The rules every stream of outputs obeys, whatever the transcript.
fn check_invariants(name: &str, outputs: &[RunnerOutput]) -> usize {
    /// A running item: the text its deltas added up to, and whose it is.
    struct Open {
        streamed: String,
        owner: Option<String>,
    }
    let mut ids = HashSet::new();
    let mut open: HashMap<String, Open> = HashMap::new();
    let mut requests = HashSet::new();
    // Running subagents with the subagent that started each, and every key ever used.
    let mut subagents: HashMap<String, Option<String>> = HashMap::new();
    let mut keys = HashSet::new();
    let mut turns = 0;
    for output in outputs {
        let (owner, body) = match output {
            RunnerOutput::Event(body) => (None, Some(body)),
            RunnerOutput::SubagentEvent { key, body } => {
                assert!(
                    subagents.contains_key(key),
                    "{name}: output of {key}, which is not a running subagent"
                );
                (Some(key.clone()), Some(body))
            }
            _ => (None, None),
        };
        match (output, body) {
            (_, Some(EventBody::ItemStarted { item })) => {
                assert_eq!(item.status, ItemStatus::Running, "{name}: {item:?}");
                assert!(ids.insert(item.id.clone()), "{name}: id {} reused", item.id);
                let streamed = String::new();
                open.insert(item.id.clone(), Open { streamed, owner });
            }
            (_, Some(EventBody::ItemDelta { item_id, delta })) => {
                let item = open
                    .get_mut(item_id)
                    .unwrap_or_else(|| panic!("{name}: delta for {item_id}, which is not running"));
                assert_eq!(item.owner, owner, "{name}: {item_id} changed hands");
                match delta {
                    ItemDelta::Text { text: chunk }
                    | ItemDelta::Reasoning { text: chunk }
                    | ItemDelta::Output { text: chunk } => item.streamed.push_str(chunk),
                }
            }
            (_, Some(EventBody::ItemCompleted { item })) => {
                let was = open
                    .remove(&item.id)
                    .unwrap_or_else(|| panic!("{name}: {} completed but is not running", item.id));
                assert_eq!(was.owner, owner, "{name}: {} changed hands", item.id);
                assert_ne!(item.status, ItemStatus::Running, "{name}: {item:?}");
                match &item.content {
                    ItemContent::Text { text } | ItemContent::Reasoning { text } => {
                        assert!(!text.is_empty(), "{name}: empty prose item {}", item.id);
                        assert!(
                            was.streamed.is_empty() || was.streamed == *text,
                            "{name}: deltas of {} do not add up to its text",
                            item.id
                        );
                    }
                    ItemContent::Tool { .. } => assert!(was.streamed.is_empty()),
                }
            }
            (_, Some(EventBody::Usage { .. })) => {
                assert_eq!(owner, None, "{name}: totals are the whole process's");
            }
            (_, Some(_)) => {}
            (RunnerOutput::PermissionRequest(ask), _) => {
                let PermissionAsk {
                    request, subagent, ..
                } = ask;
                assert!(
                    requests.insert(request.id.clone()),
                    "{name}: request reused"
                );
                let item_id = request.item_id.as_ref().expect("requests name their item");
                let item = open
                    .get(item_id)
                    .unwrap_or_else(|| panic!("{name}: request for an idle item"));
                assert_eq!(
                    &item.owner, subagent,
                    "{name}: {item_id} asked for by another"
                );
                if let Some(key) = subagent {
                    assert!(
                        subagents.contains_key(key),
                        "{name}: {key} asks but is over"
                    );
                }
            }
            (RunnerOutput::SubagentStarted(start), _) => {
                assert!(
                    keys.insert(start.key.clone()),
                    "{name}: key {} reused",
                    start.key
                );
                if let Some(parent) = &start.parent {
                    assert!(
                        subagents.contains_key(parent),
                        "{name}: {} started by {parent}, which is not running",
                        start.key
                    );
                }
                let item_id = start.item_id.as_ref().expect("the call is known");
                let call = open
                    .get(item_id)
                    .unwrap_or_else(|| panic!("{name}: {} started by an idle item", start.key));
                assert_eq!(call.owner, start.parent, "{name}: the call is the caller's");
                subagents.insert(start.key.clone(), start.parent.clone());
            }
            (RunnerOutput::SubagentEnded { key, .. }, _) => {
                assert!(
                    subagents.remove(key).is_some(),
                    "{name}: {key} ended but is not running"
                );
                assert!(
                    !subagents
                        .values()
                        .any(|parent| parent.as_ref() == Some(key)),
                    "{name}: {key} ended before a subagent it started"
                );
                assert!(
                    !open.values().any(|item| item.owner.as_ref() == Some(key)),
                    "{name}: {key} ended with items open"
                );
            }
            (RunnerOutput::TurnEnded, _) => {
                // Only a subagent that outlives the turn keeps items open.
                let lead: Vec<&String> = open
                    .iter()
                    .filter(|(_, item)| item.owner.is_none())
                    .map(|(id, _)| id)
                    .collect();
                assert!(
                    lead.is_empty(),
                    "{name}: a turn ended with open items {lead:?}"
                );
                turns += 1;
            }
            (RunnerOutput::Exited(_), _) => panic!("{name}: a mapper never reports an exit"),
            (RunnerOutput::NativeSession { .. }, _) => {}
            (RunnerOutput::Limits(windows), _) => {
                assert!(!windows.is_empty(), "{name}: limits without a window");
                for window in windows {
                    assert!((0.0..=1.0).contains(&window.used), "{name}: {window:?}");
                }
            }
            (RunnerOutput::Event(_) | RunnerOutput::SubagentEvent { .. }, None) => {
                unreachable!("an event has a body")
            }
        }
    }
    assert!(
        open.is_empty(),
        "{name}: the stream ended with open items {:?}",
        open.keys()
    );
    assert!(
        subagents.is_empty(),
        "{name}: the stream ended with running subagents {subagents:?}"
    );
    turns
}

#[test]
fn every_fixture_keeps_the_invariants() {
    for path in fixtures() {
        let name = path.display().to_string();
        let outputs = replay(&path);
        let turns = check_invariants(&name, &outputs);
        // One `TurnEnded` per `result`, each right after its usage.
        let results = records(&path)
            .iter()
            .filter(|(dir, message)| *dir == Dir::Out && message["type"] == "result")
            .count();
        assert_eq!(turns, results, "{name}");
        assert!(turns >= 1, "{name}: no turn");
        if results > 0 {
            // Nothing follows the last turn but the end of what a subagent
            // left running in the background, cut with the stream.
            let after_turns = outputs
                .iter()
                .rev()
                .take_while(|output| **output != RunnerOutput::TurnEnded);
            for output in after_turns {
                assert!(
                    matches!(
                        output,
                        RunnerOutput::SubagentEnded { ok: false, .. }
                            | RunnerOutput::SubagentEvent {
                                body: EventBody::ItemCompleted { .. },
                                ..
                            }
                    ),
                    "{name}: {output:?} after the last turn"
                );
            }
        }
    }
}

/// The lines the runner would write, built by the mapper, are the lines the
/// recordings wrote and the real CLI accepted.
#[test]
fn the_mapper_writes_what_the_recordings_wrote() {
    for path in fixtures() {
        let name = path.display().to_string();
        let mut mapper = mapper();
        let mut outputs = Vec::new();
        for (dir, message) in records(&path) {
            if dir == Dir::Out {
                outputs.extend(mapper.line(&message.to_string()));
                continue;
            }
            match message["type"].as_str() {
                Some("user") => {
                    let text = message["message"]["content"][0]["text"].as_str().unwrap();
                    assert_eq!(Mapper::user_message(text), message, "{name}");
                }
                Some("control_response") => {
                    let answer = &message["response"]["response"];
                    let (decision, said) = match answer["behavior"].as_str() {
                        Some("allow") => (PermissionDecision::Allow, None),
                        Some("deny") => (
                            PermissionDecision::Deny,
                            answer["message"].as_str().map(str::to_owned),
                        ),
                        other => panic!("{name}: unknown behavior {other:?}"),
                    };
                    let request_id = message["response"]["request_id"].as_str().unwrap();
                    let grants = recorded_grants(&answer["updatedPermissions"]);
                    let written =
                        mapper.permission_response_granting(request_id, decision, said, &grants);
                    assert_eq!(written, Some(message.clone()), "{name}");
                    // Answered once; a second decision has nothing to answer.
                    assert_eq!(mapper.permission_response(request_id, decision, None), None);
                }
                Some("control_request") => {
                    let mut written = mapper.interrupt_request();
                    assert_eq!(written["request_id"], "calin-interrupt-1");
                    written["request_id"] = message["request_id"].clone();
                    assert_eq!(written, message, "{name}");
                }
                other => panic!("{name}: unknown input {other:?}"),
            }
        }
        outputs.extend(mapper.finish());
        // Knowing the decisions changes nothing: the CLI's output says it all.
        assert_eq!(outputs, replay(&path), "{name}");
    }
}

/// The grants a recorded allow carried, read back from the CLI's own shapes.
fn recorded_grants(updates: &Value) -> Vec<DirGrant> {
    let updates = updates.as_array().map(Vec::as_slice).unwrap_or_default();
    updates
        .iter()
        .map(|update| match update["type"].as_str() {
            Some("addDirectories") => DirGrant {
                dir: update["directories"][0].as_str().unwrap().into(),
                write: true,
            },
            Some("addRules") => {
                let rule = update["rules"][0]["ruleContent"].as_str().unwrap();
                let dir = rule.strip_prefix('/').and_then(|r| r.strip_suffix("/**"));
                DirGrant {
                    dir: dir.unwrap().into(),
                    write: false,
                }
            }
            other => panic!("unknown permission update {other:?}"),
        })
        .collect()
}

/// Every event with the key of the subagent it belongs to, `None` for the lead.
fn attributed(outputs: &[RunnerOutput]) -> Vec<(Option<&str>, &EventBody)> {
    outputs
        .iter()
        .filter_map(|output| match output {
            RunnerOutput::Event(body) => Some((None, body)),
            RunnerOutput::SubagentEvent { key, body } => Some((Some(key.as_str()), body)),
            _ => None,
        })
        .collect()
}

/// Every event, whoever produced it.
fn events(outputs: &[RunnerOutput]) -> Vec<&EventBody> {
    attributed(outputs)
        .into_iter()
        .map(|(_, body)| body)
        .collect()
}

fn completed(outputs: &[RunnerOutput]) -> Vec<&Item> {
    events(outputs)
        .into_iter()
        .filter_map(|body| match body {
            EventBody::ItemCompleted { item } => Some(item),
            _ => None,
        })
        .collect()
}

fn tool(item: &Item) -> (&str, ToolKind, &str, ItemStatus) {
    match &item.content {
        ItemContent::Tool {
            name,
            tool_kind,
            title,
            ..
        } => (name, *tool_kind, title, item.status),
        other => panic!("not a tool item: {other:?}"),
    }
}

fn tools_of(outputs: &[RunnerOutput]) -> Vec<(&str, ToolKind, &str, ItemStatus)> {
    completed(outputs)
        .into_iter()
        .filter(|item| matches!(item.content, ItemContent::Tool { .. }))
        .map(tool)
        .collect()
}

fn usage(outputs: &[RunnerOutput]) -> Vec<(f64, u64, u64)> {
    events(outputs)
        .into_iter()
        .filter_map(|body| match body {
            EventBody::Usage {
                cost_usd,
                tokens_in,
                tokens_out,
            } => Some((*cost_usd, *tokens_in, *tokens_out)),
            _ => None,
        })
        .collect()
}

fn errors(outputs: &[RunnerOutput]) -> Vec<&str> {
    events(outputs)
        .into_iter()
        .filter_map(|body| match body {
            EventBody::Error { message } => Some(message.as_str()),
            _ => None,
        })
        .collect()
}

// What the recordings mean, spelled out, so that a blessed change to an
// expected file cannot quietly alter it.

#[test]
fn a_denied_call_is_declined_and_an_allowed_one_completes() {
    let denied = replay(&fixture("deny-permission"));
    assert_eq!(
        tools_of(&denied),
        [
            ("Write", ToolKind::Edit, "made.txt", ItemStatus::Declined),
            ("Write", ToolKind::Edit, "made.txt", ItemStatus::Declined),
        ]
    );
    assert!(matches!(
        &completed(&denied)[0].content,
        ItemContent::Tool { output: Some(output), locations, .. }
            if output == "Not in this directory." && locations == &["/work/made.txt"]
    ));
    assert!(errors(&denied).is_empty());

    let allowed = replay(&fixture("two-turns-with-permission"));
    assert_eq!(
        tools_of(&allowed),
        [
            ("Write", ToolKind::Edit, "made.txt", ItemStatus::Completed),
            ("Bash", ToolKind::Execute, "sleep 30", ItemStatus::Failed),
        ]
    );
    let requests: Vec<&PermissionRequest> = allowed
        .iter()
        .filter_map(|output| match output {
            RunnerOutput::PermissionRequest(ask) => Some(&ask.request),
            _ => None,
        })
        .collect();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].id, "uuid-0006");
    assert_eq!(requests[0].item_id.as_deref(), Some("toolu-0001"));
    assert_eq!(
        (requests[0].tool_kind, requests[0].title.as_str()),
        (ToolKind::Edit, "made.txt")
    );
}

#[test]
fn an_interrupted_turn_ends_without_an_error_and_the_next_one_runs() {
    let outputs = replay(&fixture("interrupt"));
    assert_eq!(check_invariants("interrupt", &outputs), 2);
    let tools = tools_of(&outputs);
    assert_eq!(tools.len(), 1);
    assert_eq!(
        (tools[0].0, tools[0].1, tools[0].3),
        ("Bash", ToolKind::Execute, ItemStatus::Failed)
    );
    assert!(errors(&outputs).is_empty(), "an interrupt is not an error");
    let last = completed(&outputs).last().unwrap().content.clone();
    assert_eq!(last, ItemContent::Text { text: "ok".into() });
}

#[test]
fn totals_run_across_turns_and_across_a_resume() {
    let two = usage(&replay(&fixture("two-turns-with-permission")));
    assert_eq!(two.len(), 2);
    assert_eq!(two[0], (0.00369768, 4 + 38108 + 16121, 184));
    assert_eq!(two[1], (0.005931820000000001, 8 + 104742 + 22978, 576));

    // The resumed process reports the conversation's totals, not its own.
    let before = usage(&replay(&fixture("edit-existing-file")));
    let after = usage(&replay(&fixture("resume")));
    assert!(after[0].0 > before[0].0);
    assert!(after[0].1 > before[0].1 && after[0].2 > before[0].2);

    // A subagent's tokens count: `usage` alone would say 493 out.
    let subagent = usage(&replay(&fixture("subagent")));
    assert_eq!(subagent[0].2, 699);
}

#[test]
fn failures_are_reported_once_and_end_the_turn() {
    let unknown = replay(&fixture("unknown-model"));
    assert_eq!(errors(&unknown).len(), 1);
    assert!(errors(&unknown)[0].contains("no-such-model"));
    // The CLI's wording of the failure is not the model speaking.
    assert!(completed(&unknown).is_empty());
    assert_eq!(unknown.last(), Some(&RunnerOutput::TurnEnded));

    let missing = replay(&fixture("resume-missing"));
    assert!(errors(&missing)[0].starts_with("No conversation found with session ID"));
    assert_eq!(missing.last(), Some(&RunnerOutput::TurnEnded));
    assert!(
        !missing
            .iter()
            .any(|output| matches!(output, RunnerOutput::NativeSession { .. }))
    );
}

#[test]
fn a_subagent_shows_as_a_task_with_its_own_tool_calls() {
    let outputs = replay(&fixture("subagent"));
    assert_eq!(
        tools_of(&outputs),
        [
            ("Read", ToolKind::Read, "notes.txt", ItemStatus::Completed),
            (
                "Agent",
                ToolKind::Task,
                "Read first line of notes.txt",
                ItemStatus::Completed
            ),
        ]
    );
    // The task's result is a list of blocks, joined.
    let task = completed(&outputs)
        .into_iter()
        .find(|item| item.id == "toolu-0001")
        .unwrap();
    assert!(matches!(
        &task.content,
        ItemContent::Tool { output: Some(output), .. } if output.contains("Calin fixture note.")
    ));
}

/// The outputs that say who did what, one short line each: item starts and
/// ends (prefixed with the subagent's key when they are one's), subagents
/// starting (`+`) and ending (`-`), and the ends of turns.
fn trace(outputs: &[RunnerOutput]) -> Vec<String> {
    let line = |body: &EventBody| match body {
        EventBody::ItemStarted { item } => Some(format!("start {}", item.id)),
        EventBody::ItemCompleted { item } => {
            let status = format!("{:?}", item.status).to_lowercase();
            Some(format!("{status} {}", item.id))
        }
        _ => None,
    };
    outputs
        .iter()
        .filter_map(|output| match output {
            RunnerOutput::Event(body) => line(body),
            RunnerOutput::SubagentEvent { key, body } => {
                line(body).map(|line| format!("{key}: {line}"))
            }
            RunnerOutput::SubagentStarted(start) => Some(format!(
                "+{} by {}",
                start.key,
                start.parent.as_deref().unwrap_or("lead")
            )),
            RunnerOutput::SubagentEnded { key, ok } => {
                Some(format!("-{key} {}", if *ok { "ok" } else { "cut" }))
            }
            RunnerOutput::TurnEnded => Some("turn".to_owned()),
            _ => None,
        })
        .collect()
}

#[test]
fn the_recorded_subagent_owns_what_it_did_and_ends_with_its_call() {
    let outputs = replay(&fixture("subagent"));
    assert_eq!(
        trace(&outputs),
        [
            "start toolu-0001",
            "+toolu-0001 by lead",
            "toolu-0001: start toolu-0002",
            "toolu-0001: completed toolu-0002",
            // The CLI's notification comes first; the call's result is the end.
            "-toolu-0001 ok",
            "completed toolu-0001",
            "start claude-7-3",
            "completed claude-7-3",
            "turn",
        ]
    );
    let started: Vec<&SubagentStart> = outputs
        .iter()
        .filter_map(|output| match output {
            RunnerOutput::SubagentStarted(start) => Some(start),
            _ => None,
        })
        .collect();
    assert_eq!(
        started,
        [&SubagentStart {
            key: "toolu-0001".into(),
            parent: None,
            role: "general-purpose".into(),
            description: "Read first line of notes.txt".into(),
            item_id: Some("toolu-0001".into()),
        }]
    );
    // Totals are the whole process's, never a subagent's.
    assert_eq!(usage(&outputs).len(), 1);
    assert!(
        attributed(&outputs)
            .iter()
            .all(|(key, body)| key.is_none() || !matches!(body, EventBody::Usage { .. }))
    );
}

/// Two subagents the CLI launched in the background on its own: their
/// calls return at once, their work interleaves on one pipe, one is over
/// before the turn ends and the other outlives it.
#[test]
fn recorded_background_subagents_keep_their_work_apart_and_outlive_their_calls() {
    let outputs = replay(&fixture("parallel-subagents"));
    assert_eq!(
        trace(&outputs),
        [
            "start toolu-0001",
            "+toolu-0001 by lead",
            // "Async agent launched": the call is over, the subagent is not.
            "completed toolu-0001",
            "start toolu-0002",
            "+toolu-0002 by lead",
            "completed toolu-0002",
            "toolu-0002: start toolu-0003",
            "toolu-0002: completed toolu-0003",
            "toolu-0001: start toolu-0004",
            "toolu-0001: completed toolu-0004",
            "start claude-7-2",
            "toolu-0002: start claude-7-3",
            "toolu-0002: completed claude-7-3",
            "completed claude-7-2",
            // Its notification names it.
            "-toolu-0002 ok",
            "turn",
            // Still running when the recording ends: cut with the stream.
            "-toolu-0001 cut",
        ]
    );
    // Neither call asked to run in the background: the result says it did.
    for call in ["toolu-0001", "toolu-0002"] {
        let item = completed(&outputs).into_iter().find(|item| item.id == call);
        let ItemContent::Tool { input, output, .. } = &item.unwrap().content else {
            panic!("{call} is a tool call");
        };
        assert!(input.get("run_in_background").is_none());
        assert!(
            output
                .as_deref()
                .unwrap()
                .starts_with("Async agent launched")
        );
    }
}

#[test]
fn the_conversation_is_announced_once_per_change() {
    let outputs = replay(&fixture("two-turns-with-permission"));
    let announced: Vec<&RunnerOutput> = outputs
        .iter()
        .filter(|output| matches!(output, RunnerOutput::NativeSession { .. }))
        .collect();
    // `init` opens both turns; the second says nothing new.
    assert_eq!(
        announced,
        [&RunnerOutput::NativeSession {
            id: "uuid-0001".into(),
            model: Some("claude-haiku-5-5".into()),
        }]
    );
    assert_eq!(outputs.first(), Some(announced[0]));

    let mut mapper = mapper();
    let init = |session: &str, model: &str| json!({ "type": "system", "subtype": "init", "session_id": session, "model": model });
    assert_eq!(mapper.message(&init("s1", "m1")).len(), 1);
    assert_eq!(mapper.message(&init("s1", "m1")), []);
    assert_eq!(
        mapper.message(&init("s1", "m2")),
        [RunnerOutput::NativeSession {
            id: "s1".into(),
            model: Some("m2".into()),
        }]
    );
    assert_eq!(mapper.message(&init("s2", "m2")).len(), 1);
    // Other system messages, and an init without an id, say nothing.
    assert_eq!(
        mapper.message(&json!({ "type": "system", "subtype": "init" })),
        []
    );
    assert_eq!(
        mapper.message(&json!({ "type": "system", "subtype": "status", "session_id": "s3" })),
        []
    );
}

// Shapes no recording holds, built by hand.

fn stream(event: Value) -> Value {
    json!({
        "type": "stream_event",
        "api_message_id": "msg_1",
        "parent_tool_use_id": null,
        "event": event,
    })
}

fn assistant(message_id: &str, parent: Option<&str>, content: Value) -> Value {
    json!({
        "type": "assistant",
        "parent_tool_use_id": parent,
        "message": { "id": message_id, "role": "assistant", "content": content },
    })
}

fn tool_use(id: &str, name: &str, input: Value) -> Value {
    assistant(
        "msg_tools",
        None,
        json!([{ "type": "tool_use", "id": id, "name": name, "input": input }]),
    )
}

fn tool_result(id: &str, block: Value) -> Value {
    let mut block = block;
    block["type"] = json!("tool_result");
    block["tool_use_id"] = json!(id);
    json!({ "type": "user", "message": { "role": "user", "content": [block] } })
}

fn result() -> Value {
    json!({ "type": "result", "subtype": "success", "is_error": false })
}

fn feed(mapper: &mut Mapper, messages: &[Value]) -> Vec<RunnerOutput> {
    messages
        .iter()
        .flat_map(|message| mapper.line(&message.to_string()))
        .collect()
}

#[test]
fn unknown_and_malformed_lines_are_skipped() {
    let mut mapper = mapper();
    for line in [
        "",
        "   ",
        "not json at all",
        "{\"type\": \"assistant\", \"message\": ",
        "[1, 2, 3]",
        "42",
        "null",
        r#"{"no_type": true}"#,
        r#"{"type": 7}"#,
        r#"{"type": "hologram", "payload": {"deep": [1, 2]}}"#,
        r#"{"type": "system", "subtype": "telepathy"}"#,
        r#"{"type": "stream_event"}"#,
        r#"{"type": "stream_event", "event": {"type": "content_block_teleport", "index": 0}}"#,
        r#"{"type": "stream_event", "event": {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "orphan"}}}"#,
        r#"{"type": "stream_event", "event": {"type": "content_block_stop", "index": 3}}"#,
        r#"{"type": "assistant"}"#,
        r#"{"type": "assistant", "message": {"content": "just a string"}}"#,
        r#"{"type": "assistant", "message": {"content": [{"type": "hologram"}, {"type": "tool_use"}, 5]}}"#,
        r#"{"type": "user", "message": {"content": "a string, not blocks"}}"#,
        r#"{"type": "user", "message": {"content": [{"type": "tool_result"}, {"type": "tool_result", "tool_use_id": "toolu_unknown", "content": "x"}]}}"#,
        r#"{"type": "control_request"}"#,
        r#"{"type": "control_request", "request_id": "r1", "request": {"subtype": "hook_callback"}}"#,
        r#"{"type": "control_request", "request_id": "r2", "request": {"subtype": "can_use_tool"}}"#,
        r#"{"type": "control_response", "response": {"subtype": "success", "request_id": "x"}}"#,
        r#"{"type": "control_cancel_request", "request_id": "nobody"}"#,
        r#"{"type": "rate_limit_event", "rate_limit_info": {}}"#,
    ] {
        assert_eq!(mapper.line(line), [], "{line}");
    }
    assert_eq!(mapper.finish(), []);

    // The mapper still works afterwards, and extra fields change nothing.
    let outputs = feed(
        &mut mapper,
        &[json!({
            "type": "result", "subtype": "success", "is_error": false,
            "total_cost_usd": 0.5, "brand_new_field": { "nested": true },
            "usage": { "input_tokens": 1, "output_tokens": 2, "future_tokens": 9 },
        })],
    );
    assert_eq!(usage(&outputs), [(0.5, 1, 2)]);
    assert_eq!(outputs.last(), Some(&RunnerOutput::TurnEnded));
}

#[test]
fn tool_results_come_as_strings_blocks_or_nothing() {
    let mut mapper = mapper();
    let outputs = feed(
        &mut mapper,
        &[
            tool_use("t1", "Bash", json!({ "command": "true" })),
            tool_use("t2", "Read", json!({ "file_path": "/work/a" })),
            tool_use("t3", "Grep", json!({ "pattern": "x" })),
            tool_use("t4", "Bash", json!({ "command": "false" })),
            tool_result("t1", json!({ "content": "plain" })),
            tool_result(
                "t2",
                json!({ "content": [
                    { "type": "text", "text": "first" },
                    { "type": "image", "source": {} },
                    { "type": "text", "text": "second" },
                ] }),
            ),
            tool_result("t3", json!({})),
            tool_result("t4", json!({ "content": "exit 1", "is_error": true })),
            // A second result for the same call has nothing left to complete.
            tool_result("t1", json!({ "content": "again" })),
        ],
    );
    let finished: Vec<(&str, ItemStatus, Option<&str>)> = completed(&outputs)
        .into_iter()
        .map(|item| match &item.content {
            ItemContent::Tool { output, .. } => (item.id.as_str(), item.status, output.as_deref()),
            other => panic!("unexpected {other:?}"),
        })
        .collect();
    assert_eq!(
        finished,
        [
            ("t1", ItemStatus::Completed, Some("plain")),
            ("t2", ItemStatus::Completed, Some("first\n[image]\nsecond")),
            ("t3", ItemStatus::Completed, Some("")),
            ("t4", ItemStatus::Failed, Some("exit 1")),
        ]
    );
    check_invariants("tool results", &outputs);
}

#[test]
fn long_tool_output_is_truncated_with_a_note() {
    let mut mapper = mapper();
    let long = "x".repeat(tools::OUTPUT_CAP + 1000);
    let outputs = feed(
        &mut mapper,
        &[
            tool_use("t1", "Bash", json!({ "command": "yes" })),
            tool_result("t1", json!({ "content": long })),
        ],
    );
    let ItemContent::Tool {
        output: Some(output),
        ..
    } = &completed(&outputs)[0].content
    else {
        panic!("no output");
    };
    assert!(output.len() < tools::OUTPUT_CAP + 100);
    assert!(output.ends_with(&format!(
        "[output truncated: first {} of {} bytes]",
        tools::OUTPUT_CAP,
        tools::OUTPUT_CAP + 1000
    )));
}

#[test]
fn a_tool_call_keeps_its_final_input_and_names_its_files() {
    let mut mapper = mapper();
    let input = json!({ "file_path": "src/main.rs", "old_string": "a", "new_string": "b" });
    let outputs = feed(
        &mut mapper,
        &[
            // The streamed start carries no input yet and starts nothing.
            stream(json!({
                "type": "content_block_start", "index": 0,
                "content_block": { "type": "tool_use", "id": "t1", "name": "Edit", "input": {} },
            })),
            stream(json!({
                "type": "content_block_delta", "index": 0,
                "delta": { "type": "input_json_delta", "partial_json": "{\"file_pa" },
            })),
            tool_use("t1", "Edit", input.clone()),
            stream(json!({ "type": "content_block_stop", "index": 0 })),
            // Repeated, it does not start again.
            tool_use("t1", "Edit", input.clone()),
        ],
    );
    let expected = Item {
        id: "t1".into(),
        status: ItemStatus::Running,
        content: ItemContent::Tool {
            name: "Edit".into(),
            tool_kind: ToolKind::Edit,
            title: "src/main.rs".into(),
            input,
            output: None,
            locations: vec!["/work/src/main.rs".into()],
        },
    };
    assert_eq!(
        outputs,
        [RunnerOutput::Event(EventBody::ItemStarted {
            item: expected
        })]
    );
}

#[test]
fn a_todo_list_is_also_the_plan() {
    let mut mapper = mapper();
    let todos = json!({ "todos": [
        { "content": "Read the notes", "status": "completed", "activeForm": "Reading the notes" },
        { "content": "Fix the typo", "status": "in_progress", "activeForm": "Fixing the typo" },
        { "content": "Summarize", "status": "pending", "activeForm": "Summarizing" },
    ] });
    let outputs = feed(
        &mut mapper,
        &[
            tool_use("t1", "TodoWrite", todos),
            tool_result(
                "t1",
                json!({ "content": "Todos have been modified successfully" }),
            ),
        ],
    );
    assert_eq!(
        plans(&outputs),
        [[
            ("Read the notes", PlanStatus::Completed),
            ("Fix the typo", PlanStatus::InProgress),
            ("Summarize", PlanStatus::Pending),
        ]]
    );
    // The call itself stays an ordinary tool item.
    assert_eq!(
        tools_of(&outputs),
        [(
            "TodoWrite",
            ToolKind::Other,
            "TodoWrite",
            ItemStatus::Completed
        )]
    );
    assert!(matches!(
        outputs[1],
        RunnerOutput::Event(EventBody::PlanUpdated { .. })
    ));
}

#[test]
fn streamed_text_is_one_item_whichever_way_it_ends() {
    let start = stream(json!({
        "type": "content_block_start", "index": 0, "content_block": { "type": "text", "text": "" },
    }));
    let delta = |text: &str| {
        stream(json!({
            "type": "content_block_delta", "index": 0,
            "delta": { "type": "text_delta", "text": text },
        }))
    };
    let stop = stream(json!({ "type": "content_block_stop", "index": 0 }));
    let whole = assistant(
        "msg_1",
        None,
        json!([{ "type": "text", "text": "Hello there" }]),
    );
    let texts = |outputs: &[RunnerOutput]| -> Vec<String> {
        completed(outputs)
            .into_iter()
            .map(|item| match &item.content {
                ItemContent::Text { text } => text.clone(),
                other => panic!("unexpected {other:?}"),
            })
            .collect()
    };

    // The complete message first, as the CLI does today.
    let mut mapper = self::mapper();
    let outputs = feed(
        &mut mapper,
        &[
            start.clone(),
            delta("Hello "),
            delta("there"),
            whole.clone(),
            stop.clone(),
        ],
    );
    assert_eq!(texts(&outputs), ["Hello there"]);
    assert_eq!(
        outputs.len(),
        4,
        "start, two deltas, completion: {outputs:?}"
    );
    check_invariants("message first", &outputs);

    // The block's end first: the complete message then repeats known text.
    let mut mapper = self::mapper();
    let outputs = feed(
        &mut mapper,
        &[
            start.clone(),
            delta("Hello "),
            delta("there"),
            stop.clone(),
            whole.clone(),
        ],
    );
    assert_eq!(texts(&outputs), ["Hello there"]);
    assert_eq!(outputs.len(), 4);

    // No end at all: the result closes it with what arrived.
    let mut mapper = self::mapper();
    let outputs = feed(&mut mapper, &[start.clone(), delta("Hel"), result()]);
    assert_eq!(texts(&outputs), ["Hel"]);
    check_invariants("cut short", &outputs);

    // A late delta for a finished block is dropped.
    let mut mapper = self::mapper();
    let outputs = feed(
        &mut mapper,
        &[start, delta("Hello there"), whole, delta("!"), stop],
    );
    assert_eq!(texts(&outputs), ["Hello there"]);
    check_invariants("late delta", &outputs);
}

#[test]
fn text_that_was_not_streamed_becomes_an_item_of_its_own() {
    // A subagent's text is forwarded whole, and so is everything when the
    // CLI runs without partial messages.
    let mut mapper = mapper();
    let outputs = feed(
        &mut mapper,
        &[
            assistant(
                "msg_a",
                Some("t9"),
                json!([{ "type": "text", "text": "From the subagent" }]),
            ),
            assistant(
                "msg_b",
                None,
                json!([
                    { "type": "thinking", "thinking": "" },
                    { "type": "thinking", "thinking": "Considering." },
                    { "type": "text", "text": "Done." },
                ]),
            ),
        ],
    );
    let finished: Vec<&ItemContent> = completed(&outputs)
        .iter()
        .map(|item| &item.content)
        .collect();
    assert_eq!(
        finished,
        [
            &ItemContent::Text {
                text: "From the subagent".into()
            },
            &ItemContent::Reasoning {
                text: "Considering.".into()
            },
            &ItemContent::Text {
                text: "Done.".into()
            },
        ]
    );
    check_invariants("whole messages", &outputs);
}

#[test]
fn reasoning_exists_only_once_it_has_text() {
    let start = stream(json!({
        "type": "content_block_start", "index": 0,
        "content_block": { "type": "thinking", "thinking": "" },
    }));
    let delta = |text: &str| {
        stream(json!({
            "type": "content_block_delta", "index": 0,
            "delta": { "type": "thinking_delta", "thinking": text },
        }))
    };
    let stop = stream(json!({ "type": "content_block_stop", "index": 0 }));

    // Empty, as the CLI sends it: no item at all.
    let mut mapper = self::mapper();
    let whole = assistant(
        "msg_1",
        None,
        json!([{ "type": "thinking", "thinking": "" }]),
    );
    let signature = stream(json!({
        "type": "content_block_delta", "index": 0, "delta": { "type": "signature_delta" },
    }));
    let outputs = feed(
        &mut mapper,
        &[start.clone(), delta(""), signature, whole, stop.clone()],
    );
    assert_eq!(outputs, []);

    // With text: started by the first chunk, completed with all of it.
    let mut mapper = self::mapper();
    let outputs = feed(
        &mut mapper,
        &[start, delta(""), delta("First, "), delta("look."), stop],
    );
    check_invariants("reasoning", &outputs);
    assert_eq!(outputs.len(), 4);
    assert_eq!(
        completed(&outputs)[0].content,
        ItemContent::Reasoning {
            text: "First, look.".into()
        }
    );
}

#[test]
fn item_ids_do_not_collide_across_scopes_turns_or_processes() {
    let text = |index: u64, parent: Option<&str>| {
        vec![
            json!({
                "type": "stream_event", "api_message_id": format!("msg_{parent:?}"),
                "parent_tool_use_id": parent,
                "event": { "type": "content_block_start", "index": index,
                           "content_block": { "type": "text", "text": "" } },
            }),
            json!({
                "type": "stream_event", "api_message_id": format!("msg_{parent:?}"),
                "parent_tool_use_id": parent,
                "event": { "type": "content_block_delta", "index": index,
                           "delta": { "type": "text_delta", "text": "hi" } },
            }),
        ]
    };
    let mut ids = HashSet::new();
    let mut count = 0;
    // Two processes of one session: the second resumed the first.
    for prefix in ["claude-3", "claude-41"] {
        let mut mapper = Mapper::new(FIXTURE_CWD.into(), prefix.into());
        let mut messages = Vec::new();
        // The lead and a subagent stream the same block index at once.
        messages.extend(text(0, None));
        messages.extend(text(0, Some("t1")));
        messages.push(result());
        messages.extend(text(0, None));
        messages.push(result());
        let outputs = feed(&mut mapper, &messages);
        check_invariants(prefix, &outputs);
        for item in completed(&outputs) {
            assert!(item.id.starts_with(prefix));
            assert!(ids.insert(item.id.clone()), "{} minted twice", item.id);
            count += 1;
        }
    }
    assert_eq!(count, 6);
}

#[test]
fn a_request_that_comes_before_its_call_starts_the_item() {
    let mut mapper = mapper();
    let input = json!({ "command": "rm -rf build" });
    let request = json!({
        "type": "control_request", "request_id": "r1",
        "request": { "subtype": "can_use_tool", "tool_name": "Bash", "input": input,
                     "tool_use_id": "t1", "description": "Remove the build directory" },
    });
    let outputs = feed(
        &mut mapper,
        &[request, tool_use("t1", "Bash", input.clone())],
    );
    assert_eq!(outputs.len(), 2, "{outputs:?}");
    assert!(matches!(
        &outputs[0],
        RunnerOutput::Event(EventBody::ItemStarted { item }) if item.id == "t1"
    ));
    assert_eq!(
        outputs[1],
        RunnerOutput::PermissionRequest(PermissionAsk {
            request: PermissionRequest {
                id: "r1".into(),
                tool_name: "Bash".into(),
                tool_kind: ToolKind::Execute,
                title: "rm -rf build".into(),
                input,
                item_id: Some("t1".into()),
            },
            access: ToolAccess::Other,
            subagent: None,
        })
    );
}

fn can_use(request_id: &str, tool_use_id: &str, name: &str, input: Value) -> Value {
    json!({
        "type": "control_request", "request_id": request_id,
        "request": { "subtype": "can_use_tool", "tool_name": name, "input": input,
                     "tool_use_id": tool_use_id },
    })
}

fn asks(outputs: &[RunnerOutput]) -> Vec<&PermissionAsk> {
    outputs
        .iter()
        .filter_map(|output| match output {
            RunnerOutput::PermissionRequest(ask) => Some(ask),
            _ => None,
        })
        .collect()
}

#[test]
fn a_request_says_which_paths_it_reads_or_writes() {
    let mut mapper = mapper();
    let files = |paths: &[&str], write| ToolAccess::Files {
        paths: paths.iter().map(PathBuf::from).collect(),
        write,
    };
    let cases = [
        (
            "Read",
            json!({ "file_path": "/other/a.txt" }),
            files(&["/other/a.txt"], false),
        ),
        (
            "Edit",
            json!({ "file_path": "/other/a.txt", "old_string": "a" }),
            files(&["/other/a.txt"], true),
        ),
        (
            "Write",
            json!({ "file_path": "made.txt", "content": "x" }),
            files(&["/work/made.txt"], true),
        ),
        (
            "MultiEdit",
            json!({ "file_path": "../x/y.rs" }),
            files(&["/x/y.rs"], true),
        ),
        (
            "NotebookEdit",
            json!({ "notebook_path": "/other/n.ipynb" }),
            files(&["/other/n.ipynb"], true),
        ),
        // A search reads the tree below its path, or the working directory.
        (
            "Grep",
            json!({ "pattern": "fn", "path": "/other/src" }),
            files(&["/other/src"], false),
        ),
        (
            "Glob",
            json!({ "pattern": "**/*.rs" }),
            files(&["/work"], false),
        ),
        // Calin's own tools, under the server name the runner gave the CLI.
        (
            "mcp__calin__attach_repo",
            json!({ "name": "web" }),
            ToolAccess::Orchestrator,
        ),
        (
            "mcp__calin__list_repos",
            json!({}),
            ToolAccess::Orchestrator,
        ),
        // Everything else says nothing: a command can touch any path.
        (
            "Bash",
            json!({ "command": "cat /other/a.txt" }),
            ToolAccess::Other,
        ),
        (
            "mcp__other__attach_repo",
            json!({ "name": "web" }),
            ToolAccess::Other,
        ),
        (
            "WebFetch",
            json!({ "url": "https://example.com" }),
            ToolAccess::Other,
        ),
        // A file tool that names no file cannot be placed anywhere.
        ("Read", json!({}), ToolAccess::Other),
        ("Edit", json!({ "file_path": 7 }), ToolAccess::Other),
    ];
    for (index, (name, input, expected)) in cases.into_iter().enumerate() {
        let request = can_use(
            &format!("r{index}"),
            &format!("t{index}"),
            name,
            input.clone(),
        );
        let outputs = feed(&mut mapper, &[request]);
        let [ask] = asks(&outputs)[..] else {
            panic!("{name}: expected one request, got {outputs:?}");
        };
        assert_eq!(ask.access, expected, "{name} {input}");
        assert_eq!(ask.request.tool_name, name);
    }
}

#[test]
fn the_recorded_requests_carry_their_access() {
    let access = |name: &str| -> Vec<(String, ToolAccess)> {
        asks(&replay(&fixture(name)))
            .into_iter()
            .map(|ask| (ask.request.tool_name.clone(), ask.access.clone()))
            .collect()
    };
    let files = |path: &str, write| ToolAccess::Files {
        paths: vec![path.into()],
        write,
    };
    // Outside the working directory the CLI asks before a read and before
    // the first write; the grants then keep it from asking again.
    assert_eq!(
        access("grant-directories"),
        [
            ("Read".to_owned(), files("/work-ro/a.txt", false)),
            ("Write".to_owned(), files("/work-rw/one.txt", true)),
        ]
    );
    assert_eq!(
        access("mcp-tool"),
        [
            (
                "mcp__calin__list_repos".to_owned(),
                ToolAccess::Orchestrator
            ),
            (
                "mcp__calin__attach_repo".to_owned(),
                ToolAccess::Orchestrator
            ),
            (
                "mcp__calin__attach_repo".to_owned(),
                ToolAccess::Orchestrator
            ),
        ]
    );
    assert_eq!(
        access("deny-permission")[0],
        ("Write".to_owned(), files("/work/made.txt", true))
    );
}

#[test]
fn an_allow_can_open_directories_in_the_shapes_the_cli_suggests() {
    let mut mapper = mapper();
    let input = json!({ "file_path": "/other/a.txt", "content": "x" });
    feed(
        &mut mapper,
        &[
            can_use("r1", "t1", "Write", input.clone()),
            can_use("r2", "t2", "Write", input.clone()),
            can_use("r3", "t3", "Write", input.clone()),
        ],
    );
    let grants = [
        DirGrant {
            dir: "/repos/web".into(),
            write: true,
        },
        DirGrant {
            dir: "/repos/docs".into(),
            write: false,
        },
    ];
    let allowed = mapper
        .permission_response_granting("r1", PermissionDecision::Allow, None, &grants)
        .unwrap();
    assert_eq!(
        allowed["response"]["response"],
        json!({
            "behavior": "allow",
            "updatedInput": input,
            "updatedPermissions": [
                { "type": "addDirectories", "directories": ["/repos/web"], "destination": "session" },
                { "type": "addRules", "behavior": "allow", "destination": "session",
                  "rules": [{ "toolName": "Read", "ruleContent": "//repos/docs/**" }] },
            ],
        })
    );
    // Nothing to grant, nothing said about permissions.
    let plain = mapper
        .permission_response_granting("r2", PermissionDecision::Allow, None, &[])
        .unwrap();
    assert!(
        plain["response"]["response"]
            .get("updatedPermissions")
            .is_none()
    );
    // A denial grants nothing, whatever it was handed.
    let denied = mapper
        .permission_response_granting("r3", PermissionDecision::Deny, None, &grants)
        .unwrap();
    assert_eq!(
        denied["response"]["response"],
        json!({ "behavior": "deny", "message": DEFAULT_DENIAL })
    );
}

fn plans(outputs: &[RunnerOutput]) -> Vec<Vec<(&str, PlanStatus)>> {
    events(outputs)
        .into_iter()
        .filter_map(|body| match body {
            EventBody::PlanUpdated { entries } => Some(
                entries
                    .iter()
                    .map(|entry| (entry.content.as_str(), entry.status))
                    .collect(),
            ),
            _ => None,
        })
        .collect()
}

#[test]
fn the_recorded_task_tools_become_the_whole_plan_each_time() {
    use PlanStatus::{Completed, InProgress, Pending};
    let outputs = replay(&fixture("task-list"));
    let (read, count, report) = (
        "Read notes.txt",
        "Count lines in notes.txt",
        "Report the line count",
    );
    assert_eq!(
        plans(&outputs),
        [
            vec![(read, Pending)],
            vec![(read, Pending), (count, Pending)],
            vec![(read, Pending), (count, Pending), (report, Pending)],
            vec![(read, InProgress), (count, Pending), (report, Pending)],
            vec![(read, Completed), (count, Pending), (report, Pending)],
            vec![(read, Completed), (count, InProgress), (report, Pending)],
            vec![(read, Completed), (count, Completed), (report, Pending)],
            vec![(read, Completed), (count, Completed), (report, InProgress)],
            vec![(read, Completed), (count, Completed), (report, Completed)],
        ]
    );
    // Each plan follows the completion of the call that changed it.
    for (index, output) in outputs.iter().enumerate() {
        if matches!(output, RunnerOutput::Event(EventBody::PlanUpdated { .. })) {
            let RunnerOutput::Event(EventBody::ItemCompleted { item }) = &outputs[index - 1] else {
                panic!("a plan without its call: {:?}", outputs[index - 1]);
            };
            let ItemContent::Tool { name, .. } = &item.content else {
                panic!("not a tool call: {item:?}");
            };
            assert!(name == "TaskCreate" || name == "TaskUpdate", "{name}");
        }
    }
}

fn task_result(id: &str, content: &str, result: Value) -> Value {
    json!({
        "type": "user",
        "message": { "role": "user", "content": [
            { "type": "tool_result", "tool_use_id": id, "content": content },
        ] },
        "tool_use_result": result,
    })
}

#[test]
fn task_calls_edit_rename_and_delete_plan_entries() {
    use PlanStatus::{Completed, InProgress, Pending};
    let mut mapper = mapper();
    let created = |call: &str, id: &str, subject: &str| {
        [
            tool_use(
                call,
                "TaskCreate",
                json!({ "subject": subject, "description": "d" }),
            ),
            task_result(
                call,
                &format!("Task #{id} created successfully: {subject}"),
                json!({ "task": { "id": id, "subject": subject } }),
            ),
        ]
    };
    let updated = |call: &str, input: Value| {
        [
            tool_use(call, "TaskUpdate", input),
            task_result(call, "Updated task", json!({ "success": true })),
        ]
    };
    let mut messages = Vec::new();
    messages.extend(created("c1", "1", "First"));
    messages.extend(created("c2", "2", "Second"));
    messages.extend(updated(
        "u1",
        json!({ "taskId": "1", "status": "in_progress" }),
    ));
    messages.extend(updated(
        "u2",
        json!({ "taskId": "2", "subject": "Second, renamed" }),
    ));
    messages.extend(updated("u3", json!({ "taskId": "1", "status": "deleted" })));
    messages.extend(updated(
        "u4",
        json!({ "taskId": "2", "status": "completed" }),
    ));
    // A task this process never saw created, and a failed update: no plan.
    messages.extend(updated(
        "u5",
        json!({ "taskId": "9", "status": "completed" }),
    ));
    messages.push(tool_use(
        "u6",
        "TaskUpdate",
        json!({ "taskId": "2", "status": "pending" }),
    ));
    messages.push(task_result(
        "u6",
        "Task not found",
        json!({ "success": false }),
    ));
    messages.push(tool_use(
        "u7",
        "TaskUpdate",
        json!({ "taskId": "2", "status": "pending" }),
    ));
    messages.push(tool_result(
        "u7",
        json!({ "content": "refused", "is_error": true }),
    ));
    // The id is read from the text when the structured result is missing.
    messages.push(tool_use("c3", "TaskCreate", json!({ "subject": "Third" })));
    messages.push(tool_result(
        "c3",
        json!({ "content": "Task #3 created successfully: Third" }),
    ));
    let outputs = feed(&mut mapper, &messages);
    assert_eq!(
        plans(&outputs),
        [
            vec![("First", Pending)],
            vec![("First", Pending), ("Second", Pending)],
            vec![("First", InProgress), ("Second", Pending)],
            vec![("First", InProgress), ("Second, renamed", Pending)],
            vec![("Second, renamed", Pending)],
            vec![("Second, renamed", Completed)],
            vec![("Second, renamed", Completed), ("Third", Pending)],
        ]
    );

    // Statuses the CLI sets by itself arrive as a system message.
    let statuses = json!({
        "type": "system", "subtype": "task_list_statuses",
        "statuses": [{ "id": "3", "status": "in_progress" }, { "id": "8", "status": "completed" }],
    });
    let outputs = feed(&mut mapper, &[statuses.clone(), statuses]);
    assert_eq!(
        plans(&outputs),
        [vec![("Second, renamed", Completed), ("Third", InProgress)]]
    );
}

#[test]
fn decisions_are_written_once_and_only_for_waiting_requests() {
    let mut mapper = mapper();
    let request = |request_id: &str, tool_use_id: &str| {
        json!({
            "type": "control_request", "request_id": request_id,
            "request": { "subtype": "can_use_tool", "tool_name": "Write",
                         "input": { "file_path": "/work/a", "content": "x" },
                         "tool_use_id": tool_use_id },
        })
    };
    feed(
        &mut mapper,
        &[
            request("r1", "t1"),
            request("r2", "t2"),
            request("r3", "t3"),
        ],
    );

    assert_eq!(
        mapper.permission_response("unknown", PermissionDecision::Allow, None),
        None
    );
    assert_eq!(
        mapper.permission_response("r1", PermissionDecision::Allow, Some("ignored".into())),
        Some(json!({
            "type": "control_response",
            "response": {
                "subtype": "success", "request_id": "r1",
                "response": {
                    "behavior": "allow",
                    "updatedInput": { "file_path": "/work/a", "content": "x" },
                },
            },
        }))
    );
    assert_eq!(
        mapper.permission_response("r1", PermissionDecision::Deny, None),
        None
    );

    // A denial without words still tells the agent something.
    let denied = mapper
        .permission_response("r2", PermissionDecision::Deny, Some("  ".into()))
        .unwrap();
    assert_eq!(
        denied["response"]["response"],
        json!({ "behavior": "deny", "message": DEFAULT_DENIAL })
    );

    // The CLI withdrew the third request itself.
    feed(
        &mut mapper,
        &[json!({ "type": "control_cancel_request", "request_id": "r3" })],
    );
    assert_eq!(
        mapper.permission_response("r3", PermissionDecision::Allow, None),
        None
    );

    // The denied call is declined even if the CLI never says why it failed.
    let outputs = feed(
        &mut mapper,
        &[tool_result(
            "t2",
            json!({ "content": "refused", "is_error": true }),
        )],
    );
    assert_eq!(completed(&outputs)[0].status, ItemStatus::Declined);
}

#[test]
fn the_end_of_a_turn_voids_requests_and_closes_calls() {
    let mut mapper = mapper();
    let outputs = feed(
        &mut mapper,
        &[
            tool_use("t1", "Bash", json!({ "command": "make" })),
            tool_use("t2", "Write", json!({ "file_path": "/work/a" })),
            json!({
                "type": "control_request", "request_id": "r1",
                "request": { "subtype": "can_use_tool", "tool_name": "Bash",
                             "input": { "command": "make" }, "tool_use_id": "t1" },
            }),
            json!({
                "type": "control_request", "request_id": "r2",
                "request": { "subtype": "can_use_tool", "tool_name": "Write",
                             "input": { "file_path": "/work/a" }, "tool_use_id": "t2" },
            }),
        ],
    );
    check_invariants_open(&outputs, 2);
    assert!(
        mapper
            .permission_response("r2", PermissionDecision::Deny, None)
            .is_some()
    );

    let ended = feed(
        &mut mapper,
        &[json!({
            "type": "result", "subtype": "error_during_execution", "is_error": true,
            "terminal_reason": "aborted_tools",
        })],
    );
    let closed: Vec<(&str, ItemStatus)> = completed(&ended)
        .into_iter()
        .map(|item| (item.id.as_str(), item.status))
        .collect();
    assert_eq!(
        closed,
        [("t1", ItemStatus::Failed), ("t2", ItemStatus::Declined)]
    );
    assert!(errors(&ended).is_empty());
    assert_eq!(ended.last(), Some(&RunnerOutput::TurnEnded));
    // The unanswered request died with the turn.
    assert_eq!(
        mapper.permission_response("r1", PermissionDecision::Allow, None),
        None
    );
    assert_eq!(mapper.finish(), []);
}

/// `count` items are still running at the end of `outputs`.
fn check_invariants_open(outputs: &[RunnerOutput], count: usize) {
    let started = events(outputs)
        .into_iter()
        .filter(|body| matches!(body, EventBody::ItemStarted { .. }))
        .count();
    assert_eq!(started - completed(outputs).len(), count);
}

#[test]
fn a_stream_that_just_stops_is_closed_by_finish() {
    let mut mapper = mapper();
    let mut outputs = feed(
        &mut mapper,
        &[
            tool_use("t1", "Bash", json!({ "command": "make" })),
            stream(json!({
                "type": "content_block_start", "index": 1,
                "content_block": { "type": "text", "text": "Work" },
            })),
        ],
    );
    check_invariants_open(&outputs, 2);
    outputs.extend(mapper.finish());
    assert_eq!(check_invariants("killed", &outputs), 0);
    let closed = completed(&outputs);
    assert_eq!(
        closed[0].content,
        ItemContent::Text {
            text: "Work".into()
        }
    );
    assert_eq!(closed[1].status, ItemStatus::Failed);
    assert!(matches!(
        &closed[1].content,
        ItemContent::Tool { output: None, .. }
    ));
}

#[test]
fn error_results_say_why_in_a_line() {
    let turn = |result: Value| {
        let mut mapper = mapper();
        let outputs = feed(&mut mapper, &[result]);
        assert_eq!(outputs.last(), Some(&RunnerOutput::TurnEnded));
        errors(&outputs)
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    assert_eq!(turn(result()), Vec::<String>::new());
    assert_eq!(
        turn(json!({ "type": "result", "subtype": "error_max_turns", "is_error": true })),
        ["The agent reached its turn limit."]
    );
    assert_eq!(
        turn(
            json!({ "type": "result", "subtype": "error_during_execution",
                     "errors": ["first problem", "second problem"] })
        ),
        ["first problem\nsecond problem"]
    );
    assert_eq!(
        turn(
            json!({ "type": "result", "subtype": "success", "is_error": true,
                     "result": "Credit balance is too low" })
        ),
        ["Credit balance is too low"]
    );
    assert_eq!(
        turn(json!({ "type": "result", "subtype": "error_new_kind" })),
        ["The turn ended with an error (error_new_kind)."]
    );
    assert_eq!(
        turn(json!({ "type": "result", "is_error": true })),
        ["The turn ended with an error."]
    );
    let long = turn(json!({ "type": "result", "is_error": true, "result": "e".repeat(2000) }));
    assert_eq!(long[0].chars().count(), ERROR_CHARS + 1);

    // An API failure shows when it happens; the result does not repeat it,
    // and the next turn may report the same failure again.
    let mut mapper = mapper();
    let failure = json!({
        "type": "assistant", "is_api_error_message": true, "error": "overloaded",
        "message": { "id": "x", "content": [{ "type": "text", "text": "Overloaded" }] },
    });
    let failed = json!({ "type": "result", "subtype": "success", "is_error": true,
                         "result": "Overloaded" });
    let first = feed(&mut mapper, &[failure.clone(), failed.clone()]);
    assert_eq!(errors(&first), ["Overloaded"]);
    assert!(matches!(
        first[0],
        RunnerOutput::Event(EventBody::Error { .. })
    ));
    let second = feed(&mut mapper, &[failure, failed]);
    assert_eq!(errors(&second), ["Overloaded"]);
}

#[test]
fn tokens_add_up_from_per_turn_usage_when_totals_are_missing() {
    let mut mapper = mapper();
    let turn = |cost: f64, input: u64, output: u64| {
        json!({
            "type": "result", "subtype": "success", "total_cost_usd": cost,
            "usage": { "input_tokens": input, "cache_read_input_tokens": 100,
                       "cache_creation_input_tokens": 10, "output_tokens": output },
        })
    };
    let outputs = feed(&mut mapper, &[turn(0.25, 5, 7), turn(0.75, 1, 3)]);
    assert_eq!(usage(&outputs), [(0.25, 115, 7), (0.75, 226, 10)]);

    // Totals per model, when present, are the totals: several models add up.
    let outputs = feed(
        &mut mapper,
        &[json!({
            "type": "result", "subtype": "success", "total_cost_usd": 1.0,
            "usage": { "input_tokens": 1000, "output_tokens": 1000 },
            "modelUsage": {
                "big": { "inputTokens": 200, "cacheReadInputTokens": 50,
                         "cacheCreationInputTokens": 50, "outputTokens": 40 },
                "small": { "inputTokens": 100, "outputTokens": 10 },
            },
        })],
    );
    assert_eq!(usage(&outputs), [(1.0, 400, 50)]);
}

// Subagents, in shapes the one recording does not hold.

/// `message` as the CLI sends it from inside the subagent run by `call`.
fn inside(call: &str, mut message: Value) -> Value {
    message["parent_tool_use_id"] = json!(call);
    message
}

fn agent_call(id: &str, parent: Option<&str>, input: Value) -> Value {
    assistant(
        &format!("msg_{id}"),
        parent,
        json!([{ "type": "tool_use", "id": id, "name": "Agent", "input": input }]),
    )
}

/// A tool call made inside the subagent run by `call`.
fn call_inside(call: &str, id: &str, name: &str, input: Value) -> Value {
    assistant(
        &format!("msg_{id}"),
        Some(call),
        json!([{ "type": "tool_use", "id": id, "name": name, "input": input }]),
    )
}

fn text_inside(call: &str, message_id: &str, text: &str) -> Value {
    assistant(
        message_id,
        Some(call),
        json!([{ "type": "text", "text": text }]),
    )
}

fn notification(call: &str, status: &str) -> Value {
    json!({
        "type": "system", "subtype": "task_notification", "tool_use_id": call,
        "task_id": "task-1", "status": status,
    })
}

#[test]
fn a_subagent_that_starts_another_is_its_parent_and_outlives_it() {
    let mut mapper = mapper();
    let outputs = feed(
        &mut mapper,
        &[
            agent_call(
                "a1",
                None,
                json!({ "description": "Outer", "subagent_type": "general-purpose", "prompt": "p" }),
            ),
            // `Task` is the tool's earlier name.
            assistant(
                "msg_a2",
                Some("a1"),
                json!([{ "type": "tool_use", "id": "a2", "name": "Task",
                         "input": { "prompt": "Review it\nwith care", "subagent_type": "reviewer" } }]),
            ),
            call_inside("a2", "b1", "Bash", json!({ "command": "make" })),
            inside("a2", tool_result("b1", json!({ "content": "ok" }))),
            text_inside("a2", "msg_t1", "inner done"),
            inside(
                "a1",
                tool_result("a2", json!({ "content": "inner result" })),
            ),
            text_inside("a1", "msg_t2", "outer done"),
            tool_result("a1", json!({ "content": "outer result" })),
            result(),
        ],
    );
    assert_eq!(check_invariants("nested", &outputs), 1);
    assert_eq!(
        trace(&outputs),
        [
            "start a1",
            "+a1 by lead",
            "a1: start a2",
            "+a2 by a1",
            "a2: start b1",
            "a2: completed b1",
            "a2: start claude-7-1",
            "a2: completed claude-7-1",
            "-a2 ok",
            "a1: completed a2",
            "a1: start claude-7-2",
            "a1: completed claude-7-2",
            "-a1 ok",
            "completed a1",
            "turn",
        ]
    );
    let inner = outputs
        .iter()
        .find_map(|output| match output {
            RunnerOutput::SubagentStarted(start) if start.key == "a2" => Some(start),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        inner,
        &SubagentStart {
            key: "a2".into(),
            parent: Some("a1".into()),
            role: "reviewer".into(),
            // No description: the first line of the prompt.
            description: "Review it".into(),
            item_id: Some("a2".into()),
        }
    );
    assert_eq!(mapper.finish(), []);
}

#[test]
fn a_failed_call_fails_its_subagent_and_what_was_running_below_it() {
    let mut mapper = mapper();
    let outputs = feed(
        &mut mapper,
        &[
            agent_call("a1", None, json!({ "description": "Outer" })),
            agent_call("a2", Some("a1"), json!({ "description": "Inner" })),
            call_inside("a2", "b1", "Bash", json!({ "command": "make" })),
            inside(
                "a2",
                stream(json!({
                    "type": "content_block_start", "index": 0,
                    "content_block": { "type": "text", "text": "Half a sen" },
                })),
            ),
            // The outer call fails while the inner subagent is at work.
            tool_result("a1", json!({ "content": "crashed", "is_error": true })),
            // What the dead subagents still say is nobody's.
            inside("a2", tool_result("b1", json!({ "content": "late" }))),
            result(),
        ],
    );
    assert_eq!(check_invariants("failed", &outputs), 1);
    assert_eq!(
        trace(&outputs),
        [
            "start a1",
            "+a1 by lead",
            "a1: start a2",
            "+a2 by a1",
            "a2: start b1",
            "a2: start claude-7-1",
            // Children first, each with what it left open.
            "a2: completed claude-7-1",
            "a2: failed b1",
            "-a2 cut",
            "a1: failed a2",
            "-a1 cut",
            "failed a1",
            "turn",
        ]
    );

    // A subagent whose own result is an error, with nothing left open.
    let mut mapper = self::mapper();
    let outputs = feed(
        &mut mapper,
        &[
            agent_call("a1", None, json!({ "description": "Outer" })),
            tool_result(
                "a1",
                json!({ "content": "no such agent", "is_error": true }),
            ),
            result(),
        ],
    );
    assert_eq!(
        trace(&outputs),
        ["start a1", "+a1 by lead", "-a1 cut", "failed a1", "turn"]
    );
}

#[test]
fn a_turn_or_a_stream_that_ends_cuts_its_subagents_short() {
    let messages = [
        agent_call("a1", None, json!({ "description": "Outer" })),
        call_inside("a1", "b1", "Bash", json!({ "command": "sleep 60" })),
    ];
    let cut = [
        "start a1",
        "+a1 by lead",
        "a1: start b1",
        "a1: failed b1",
        "-a1 cut",
        "failed a1",
    ];

    // An interrupted turn.
    let mut mapper = mapper();
    let mut outputs = feed(&mut mapper, &messages);
    outputs.extend(feed(&mut mapper, &[result()]));
    assert_eq!(check_invariants("interrupted", &outputs), 1);
    let mut expected: Vec<&str> = cut.to_vec();
    expected.push("turn");
    assert_eq!(trace(&outputs), expected);
    assert_eq!(mapper.finish(), []);

    // A process that died.
    let mut mapper = self::mapper();
    let mut outputs = feed(&mut mapper, &messages);
    outputs.extend(mapper.finish());
    assert_eq!(check_invariants("killed", &outputs), 0);
    assert_eq!(trace(&outputs), cut);
}

#[test]
fn a_background_subagent_outlives_its_call_and_its_turn() {
    let launch = [
        agent_call(
            "bg",
            None,
            json!({ "description": "Watch the build", "run_in_background": true }),
        ),
        // The call returns at once.
        tool_result("bg", json!({ "content": "launched" })),
        call_inside("bg", "b1", "Bash", json!({ "command": "make watch" })),
        result(),
    ];
    let launched = [
        "start bg",
        "+bg by lead",
        "completed bg",
        "bg: start b1",
        "turn",
    ];

    let mut mapper = mapper();
    let mut outputs = feed(&mut mapper, &launch);
    assert_eq!(trace(&outputs), launched);
    // Between turns it goes on, streaming included, and its end is said.
    let later = feed(
        &mut mapper,
        &[
            inside("bg", tool_result("b1", json!({ "content": "built" }))),
            inside(
                "bg",
                stream(json!({
                    "type": "content_block_start", "index": 0,
                    "content_block": { "type": "text", "text": "All green" },
                })),
            ),
            // Not this subagent's task: nothing happens.
            notification("someone-else", "completed"),
            notification("bg", "completed"),
        ],
    );
    assert_eq!(
        trace(&later),
        [
            "bg: completed b1",
            "bg: start claude-7-1",
            "bg: completed claude-7-1",
            "-bg ok",
        ]
    );
    outputs.extend(later);
    // Over: what still names it is the lead's, as before there were subagents.
    let stray = feed(&mut mapper, &[text_inside("bg", "msg_late", "late")]);
    assert!(
        stray
            .iter()
            .all(|output| matches!(output, RunnerOutput::Event(_)))
    );
    outputs.extend(stray);
    outputs.extend(mapper.finish());
    assert_eq!(check_invariants("background", &outputs), 1);

    // A task that did not complete is a subagent that failed.
    let mut mapper = self::mapper();
    feed(&mut mapper, &launch);
    assert_eq!(
        trace(&feed(&mut mapper, &[notification("bg", "killed")])),
        ["bg: failed b1", "-bg cut"]
    );

    // The process ends first: so does the subagent.
    let mut mapper = self::mapper();
    let mut outputs = feed(&mut mapper, &launch);
    outputs.extend(mapper.finish());
    assert_eq!(check_invariants("background, killed", &outputs), 1);
    let mut expected: Vec<&str> = launched.to_vec();
    expected.extend(["bg: failed b1", "-bg cut"]);
    assert_eq!(trace(&outputs), expected);

    // A launch that failed leaves nothing running.
    let mut mapper = self::mapper();
    let outputs = feed(
        &mut mapper,
        &[
            launch[0].clone(),
            tool_result("bg", json!({ "content": "no", "is_error": true })),
        ],
    );
    assert_eq!(
        trace(&outputs),
        ["start bg", "+bg by lead", "-bg cut", "failed bg"]
    );

    // One that runs in the foreground ends with its call, whatever the
    // CLI notifies before that (recorded in `subagent`).
    let mut mapper = self::mapper();
    let outputs = feed(
        &mut mapper,
        &[
            agent_call("a1", None, json!({ "description": "Outer" })),
            notification("a1", "completed"),
        ],
    );
    assert_eq!(trace(&outputs), ["start a1", "+a1 by lead"]);
}

#[test]
fn a_request_from_inside_a_subagent_says_which_one_asks() {
    let mut mapper = mapper();
    let write = json!({ "file_path": "/work/x" });
    let command = json!({ "command": "make" });
    let outputs = feed(
        &mut mapper,
        &[
            agent_call("a1", None, json!({ "description": "Outer" })),
            call_inside("a1", "w1", "Write", write.clone()),
            can_use("r1", "w1", "Write", write),
            // A request ahead of its call, should the CLI ever name its scope.
            inside("a1", can_use("r2", "c1", "Bash", command.clone())),
            // The lead's own.
            tool_use("c2", "Bash", command.clone()),
            can_use("r3", "c2", "Bash", command),
        ],
    );
    let asking: Vec<(&str, Option<&str>)> = asks(&outputs)
        .into_iter()
        .map(|ask| (ask.request.id.as_str(), ask.subagent.as_deref()))
        .collect();
    assert_eq!(
        asking,
        [("r1", Some("a1")), ("r2", Some("a1")), ("r3", None)]
    );
    assert_eq!(
        trace(&outputs),
        [
            "start a1",
            "+a1 by lead",
            "a1: start w1",
            "a1: start c1",
            "start c2"
        ]
    );

    // A denied call is declined in the subagent's name, and when the
    // subagent ends, its unanswered request dies with its call.
    assert!(
        mapper
            .permission_response("r1", PermissionDecision::Deny, None)
            .is_some()
    );
    let mut all = outputs;
    all.extend(feed(
        &mut mapper,
        &[
            inside(
                "a1",
                tool_result("w1", json!({ "content": "denied", "is_error": true })),
            ),
            tool_result("a1", json!({ "content": "gave up" })),
        ],
    ));
    assert_eq!(
        trace(&all)[5..],
        ["a1: declined w1", "a1: failed c1", "-a1 ok", "completed a1"]
    );
    assert_eq!(
        mapper.permission_response("r2", PermissionDecision::Allow, None),
        None
    );
    assert!(
        mapper
            .permission_response("r3", PermissionDecision::Allow, None)
            .is_some()
    );
    all.extend(feed(
        &mut mapper,
        &[tool_result("c2", json!({ "content": "ok" })), result()],
    ));
    assert_eq!(check_invariants("requests", &all), 1);
}

#[test]
fn streamed_text_and_plans_inside_a_subagent_are_its_own() {
    let mut mapper = mapper();
    let todos = json!({ "todos": [{ "content": "Look", "status": "in_progress" }] });
    let outputs = feed(
        &mut mapper,
        &[
            agent_call("a1", None, json!({ "description": "Outer" })),
            inside(
                "a1",
                stream(json!({
                    "type": "content_block_start", "index": 0,
                    "content_block": { "type": "text", "text": "" },
                })),
            ),
            inside(
                "a1",
                stream(json!({
                    "type": "content_block_delta", "index": 0,
                    "delta": { "type": "text_delta", "text": "Looking" },
                })),
            ),
            call_inside("a1", "p1", "TodoWrite", todos),
            // The lead streams at the same time, in its own scope.
            stream(json!({
                "type": "content_block_start", "index": 0,
                "content_block": { "type": "text", "text": "Waiting" },
            })),
        ],
    );
    let whose: Vec<(Option<&str>, &str)> = attributed(&outputs)
        .into_iter()
        .map(|(key, body)| {
            let kind = match body {
                EventBody::ItemStarted { .. } => "started",
                EventBody::ItemDelta { .. } => "delta",
                EventBody::ItemCompleted { .. } => "completed",
                EventBody::PlanUpdated { .. } => "plan",
                other => panic!("unexpected {other:?}"),
            };
            (key, kind)
        })
        .collect();
    assert_eq!(
        whose,
        [
            (None, "started"),
            (Some("a1"), "started"),
            (Some("a1"), "delta"),
            (Some("a1"), "started"),
            (Some("a1"), "plan"),
            (None, "started"),
            (None, "delta"),
        ]
    );
}

fn limits(outputs: &[RunnerOutput]) -> Vec<Vec<LimitWindow>> {
    outputs
        .iter()
        .filter_map(|output| match output {
            RunnerOutput::Limits(windows) => Some(windows.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn a_recorded_rate_limit_event_reports_every_window() {
    // The line as CLI 2.1.332 printed it (numbers normalized by the scrubber).
    let recorded = records(&fixture("read-and-shell"))
        .into_iter()
        .find(|(dir, message)| *dir == Dir::Out && message["type"] == "rate_limit_event")
        .expect("the fixture holds a rate_limit_event")
        .1;
    let reported = limits(&mapper().message(&recorded));
    let window = |name: &str| LimitWindow {
        name: name.to_owned(),
        used: 0.1,
        resets_at: Some(1_800_000_000_000),
        refused: false,
    };
    assert_eq!(reported, [vec![window("five_hour"), window("seven_day")]]);
}

#[test]
fn limit_windows_keep_unknown_names_and_say_when_requests_are_refused() {
    let event = |info: Value| json!({ "type": "rate_limit_event", "rate_limit_info": info });
    let mut mapper = mapper();
    let outputs = feed(
        &mut mapper,
        &[event(json!({
            "status": "rejected",
            "unifiedWindows": {
                "five_hour": { "status": "rejected", "utilization": 1.0, "resetsAt": 1_764_000_000 },
                // A window this mapper has never heard of, without a reset time.
                "opus_weekly": { "status": "allowed_warning", "utilization": 0.82 },
                // Refused without saying how much is used: full.
                "seven_day": { "status": "rejected" },
                // Says nothing worth showing.
                "mystery": { "status": "allowed" },
                "": { "status": "rejected", "utilization": 1.0 },
                "odd": "not an object",
            },
        }))],
    );
    assert_eq!(
        limits(&outputs),
        [vec![
            LimitWindow {
                name: "five_hour".into(),
                used: 1.0,
                resets_at: Some(1_764_000_000_000),
                refused: true,
            },
            LimitWindow {
                name: "opus_weekly".into(),
                used: 0.82,
                resets_at: None,
                refused: false,
            },
            LimitWindow {
                name: "seven_day".into(),
                used: 1.0,
                resets_at: None,
                refused: true,
            },
        ]]
    );
    // Nothing else comes of it: no event, no turn end.
    assert_eq!(outputs.len(), 1);
}

#[test]
fn a_rate_limit_event_without_unified_windows_describes_its_own() {
    let mut mapper = mapper();
    let outputs = feed(
        &mut mapper,
        &[
            json!({ "type": "rate_limit_event", "rate_limit_info": {
                "rateLimitType": "seven_day", "status": "allowed", "utilization": 0.4,
                "resetsAt": 1_764_000_000_123_i64,
            }}),
            // Out of range values are held to the range; strings are not numbers.
            json!({ "type": "rate_limit_event", "rate_limit_info": {
                "rateLimitType": "five_hour", "status": "allowed", "utilization": 7.5,
                "resetsAt": "soon",
            }}),
            json!({ "type": "rate_limit_event", "rate_limit_info": {
                "unifiedWindows": {}, "rateLimitType": "five_hour", "utilization": -1,
            }}),
            // Nothing to describe.
            json!({ "type": "rate_limit_event", "rate_limit_info": { "status": "allowed" } }),
            json!({ "type": "rate_limit_event", "rate_limit_info": "full" }),
            json!({ "type": "rate_limit_event" }),
        ],
    );
    let window = |name: &str, used: f64, resets_at: Option<i64>| LimitWindow {
        name: name.into(),
        used,
        resets_at,
        refused: false,
    };
    assert_eq!(
        limits(&outputs),
        [
            // Already in milliseconds: taken as it is.
            vec![window("seven_day", 0.4, Some(1_764_000_000_123))],
            vec![window("five_hour", 1.0, None)],
            vec![window("five_hour", 0.0, None)],
        ]
    );
}
