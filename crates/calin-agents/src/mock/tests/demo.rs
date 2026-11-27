//! The demo directives, against a fake tool endpoint on loopback that
//! records each JSON-RPC call and answers with canned results.

use std::sync::{Arc, Mutex};

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::super::demo::{Directive, Script, directives, script};
use super::*;

const PATH: &str = "/api/v1/mcp/agent-token";

/// How the fake endpoint answers one call.
enum Answer {
    /// The `result` of a `tools/call`.
    Result(Value),
    /// A JSON-RPC error.
    Error(&'static str),
    /// Never: the connection stays open with no answer.
    Hang,
}

/// One request as the fake endpoint saw it.
#[derive(Clone, Debug)]
struct Seen {
    path: String,
    host: String,
    body: Value,
}

struct Fake {
    url: String,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Fake {
    fn calls(&self) -> Vec<Value> {
        let seen = self.seen.lock().unwrap();
        seen.iter().map(|seen| seen.body.clone()).collect()
    }

    /// The tool names called, in order.
    fn tools(&self) -> Vec<String> {
        self.calls()
            .iter()
            .map(|call| call["params"]["name"].as_str().unwrap().to_owned())
            .collect()
    }
}

fn result(text: &str, is_error: bool) -> Answer {
    Answer::Result(json!({
        "content": [{ "type": "text", "text": text }],
        "isError": is_error,
    }))
}

/// What Calin's endpoint says when each tool works.
fn canned(call: &Value) -> Answer {
    let arguments = &call["params"]["arguments"];
    match call["params"]["name"].as_str().unwrap() {
        "dispatch" => result(
            &format!(
                "Dispatched child session \"{}\" (id 01CHILD).",
                arguments["title"].as_str().unwrap()
            ),
            false,
        ),
        "send_message" => result("Noted for the user.", false),
        "report" => result("Recorded. End your turn now.", false),
        "ask_user" => result("The question was put to the user (question q1).", false),
        other => panic!("the mock called {other}"),
    }
}

/// A loopback HTTP server that plays the tool endpoint, one request per
/// connection.
async fn fake(answer: impl Fn(&Value) -> Answer + Send + Sync + 'static) -> Fake {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let recorded = seen.clone();
    let answer = Arc::new(answer);
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let recorded = recorded.clone();
            let answer = answer.clone();
            tokio::spawn(async move {
                let mut buffer = Vec::new();
                let mut chunk = [0u8; 4096];
                let header_end = loop {
                    let read = stream.read(&mut chunk).await.unwrap();
                    assert!(read > 0, "the request ended early");
                    buffer.extend_from_slice(&chunk[..read]);
                    if let Some(at) = find(&buffer, b"\r\n\r\n") {
                        break at + 4;
                    }
                };
                let head = String::from_utf8(buffer[..header_end].to_vec()).unwrap();
                let header = |name: &str| {
                    head.lines()
                        .find_map(|line| {
                            let (key, value) = line.split_once(':')?;
                            key.eq_ignore_ascii_case(name)
                                .then(|| value.trim().to_owned())
                        })
                        .unwrap_or_default()
                };
                let length: usize = header("content-length").parse().unwrap();
                while buffer.len() < header_end + length {
                    let read = stream.read(&mut chunk).await.unwrap();
                    assert!(read > 0, "the body ended early");
                    buffer.extend_from_slice(&chunk[..read]);
                }
                let body: Value = serde_json::from_slice(&buffer[header_end..]).unwrap();
                let request_line = head.lines().next().unwrap().to_owned();
                assert!(request_line.starts_with("POST "), "{request_line}");
                assert_eq!(header("content-type"), "application/json");
                recorded.lock().unwrap().push(Seen {
                    path: request_line.split(' ').nth(1).unwrap().to_owned(),
                    host: header("host"),
                    body: body.clone(),
                });
                let reply = match answer(&body) {
                    Answer::Result(result) => {
                        json!({ "jsonrpc": "2.0", "id": body["id"], "result": result })
                    }
                    Answer::Error(message) => json!({
                        "jsonrpc": "2.0", "id": body["id"],
                        "error": { "code": -32602, "message": message },
                    }),
                    Answer::Hang => {
                        // Holds the connection until the client goes away.
                        let _ = stream.read(&mut chunk).await;
                        return;
                    }
                };
                let reply = reply.to_string();
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\
                     connection: close\r\n\r\n{reply}",
                    reply.len()
                );
                stream.write_all(response.as_bytes()).await.unwrap();
                let _ = stream.shutdown().await;
            });
        }
    });
    Fake {
        url: format!("http://127.0.0.1:{port}{PATH}"),
        seen,
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn start_with(endpoint: Option<&str>, mode: PermissionMode, prompt: &str) -> Started {
    let dir = std::sync::Arc::new(tempfile::tempdir().unwrap());
    let handle = MockRunner::new(MockConfig {
        delay: Duration::ZERO,
    })
    .start(StartParams {
        session_id: "s1".into(),
        agent_id: "a1".into(),
        prompt: prompt.into(),
        cwd: dir.path().to_owned(),
        permission_mode: mode,
        start_seq: 3,
        tool_endpoint: endpoint.map(str::to_owned),
        ..StartParams::default()
    })
    .unwrap();
    Started { handle, dir }
}

/// Each finished item in one short line: `text: <text>`, or
/// `<status> <tool name> <title> -> <output>` for a tool call.
fn lines(items: &[Item]) -> Vec<String> {
    items
        .iter()
        .map(|item| match &item.content {
            ItemContent::Text { text } => format!("text: {text}"),
            ItemContent::Reasoning { text } => format!("reasoning: {text}"),
            ItemContent::Tool {
                name,
                title,
                output,
                tool_kind,
                ..
            } => {
                assert_eq!(*tool_kind, ToolKind::Other, "{name}");
                let status = format!("{:?}", item.status).to_lowercase();
                format!(
                    "{status} {name} {title} -> {}",
                    output.as_deref().unwrap_or("<none>")
                )
            }
        })
        .collect()
}

/// The input of each tool item, in order.
fn inputs(items: &[Item]) -> Vec<Value> {
    items
        .iter()
        .filter_map(|item| match &item.content {
            ItemContent::Tool { input, .. } => Some(input.clone()),
            _ => None,
        })
        .collect()
}

fn call(id: u64, name: &str, arguments: Value) -> Value {
    json!({
        "jsonrpc": "2.0", "id": id, "method": "tools/call",
        "params": { "name": name, "arguments": arguments },
    })
}

const BRIEF_A: &str = "Part A of \"Ship the health check\": add a line to the notes file, \
                       then report back.\n\ndemo: child";
const BRIEF_B: &str = "Part B of \"Ship the health check\": add a line to the notes file, \
                       then report back.\n\ndemo: child";
const UNDER_WAY: &str = "Both parts are under way: \"Ship the health check: part A\" and \
                         \"Ship the health check: part B\". Their reports will come back here.";

#[test]
fn directives_are_words_matched_loosely() {
    use Directive::*;
    assert_eq!(directives("Ship it demo: orchestrate"), [Orchestrate]);
    assert_eq!(directives("DEMO:Ask please"), [Ask]);
    assert_eq!(directives("x  Demo  :   child\tnow"), [Child]);
    assert_eq!(directives("demo:note, then demo: ask"), [Note, Ask]);
    // Unknown words, other words that end in `demo`, a missing colon.
    assert!(directives("demo: dance").is_empty());
    assert!(directives("pseudemo: ask").is_empty());
    assert!(directives("demo ask").is_empty());
    assert!(directives("demo: asking").is_empty());
    assert!(directives("demo:").is_empty());
    assert!(directives("a demo of something").is_empty());
}

#[test]
fn what_calin_hands_an_agent_is_recognized_before_any_directive() {
    assert_eq!(
        script("Ship the health check demo: orchestrate"),
        Some(Script::Orchestrate {
            subject: "Ship the health check".into(),
            note: false
        })
    );
    assert_eq!(
        script("Two parts demo: note demo: orchestrate"),
        Some(Script::Orchestrate {
            subject: "Two parts".into(),
            note: true
        })
    );
    // A brief's header names the parent, whose title holds its own directive.
    let brief = "[Calin] Brief from the session that dispatched you (\"Ship it demo: \
                 orchestrate\"). It was written by that session's agent, not by the user.\
                 \n\nPart A of \"Ship it\": add a line.\n\ndemo: child";
    assert_eq!(
        script(brief),
        Some(Script::Child {
            subject: "Part A of \"Ship it\": add a line.".into(),
            note: false
        })
    );
    let report = "[Calin] Report from child session \"Ship it: part A\" (review)\n\n\
                  Done. demo: orchestrate\n";
    assert_eq!(
        script(report),
        Some(Script::Report {
            title: "Ship it: part A".into()
        })
    );
    assert_eq!(
        script("[Calin] Note from child session \"Docs\" (id 01X)\n\nHalf way."),
        Some(Script::ChildNote {
            title: "Docs".into()
        })
    );
    assert_eq!(
        script("[Calin] The user answered: Postgres "),
        Some(Script::Answer {
            answer: "Postgres".into()
        })
    );
    assert_eq!(script("Fix the login test"), None);
    assert_eq!(script("demo: juggle"), None);
}

#[tokio::test]
async fn orchestrate_dispatches_two_children_tells_the_user_and_sums_up_their_reports() {
    let endpoint = fake(canned).await;
    let prompt = "Ship the health check   DEMO :  orchestrate";
    let mut handle = start_with(Some(&endpoint.url), PermissionMode::Autonomous, prompt);
    let turn = run_turn(&mut handle, allow).await;
    let items = check_turn(&turn.events);
    assert!(turn.requests.is_empty() && turn.limits.is_empty());
    assert_eq!(
        lines(&items),
        [
            "text: I'll split \"Ship the health check\" in two and hand each part to a child session.",
            "completed mcp__calin__dispatch Dispatch Ship the health check: part A -> \
             Dispatched child session \"Ship the health check: part A\" (id 01CHILD).",
            "completed mcp__calin__dispatch Dispatch Ship the health check: part B -> \
             Dispatched child session \"Ship the health check: part B\" (id 01CHILD).",
            "completed mcp__calin__send_message Note to the user -> Noted for the user.",
        ]
    );
    let a = json!({ "title": "Ship the health check: part A", "brief": BRIEF_A });
    let b = json!({ "title": "Ship the health check: part B", "brief": BRIEF_B });
    let note = json!({ "to": "user", "text": UNDER_WAY });
    assert_eq!(inputs(&items), [a.clone(), b.clone(), note.clone()]);
    assert_eq!(
        endpoint.calls(),
        [
            call(1, "dispatch", a),
            call(2, "dispatch", b),
            call(3, "send_message", note)
        ]
    );
    for seen in endpoint.seen.lock().unwrap().iter() {
        assert_eq!(seen.path, PATH);
        assert_eq!(seen.host, endpoint.url[7..].split('/').next().unwrap());
    }
    // No subagent, no edit: nothing but the four items.
    assert_eq!(handle.notes(), None);
    assert!(
        turn.outputs
            .iter()
            .all(|output| matches!(output, RunnerOutput::Event(_)))
    );

    // Each report is acknowledged by title; the second also gets a summary.
    handle
        .send_message(
            "[Calin] Report from child session \"Ship the health check: part B\" (review)\n\n\
             Added a line.\n",
        )
        .unwrap();
    let first = check_turn(&collect_turn(&mut handle).await);
    assert_eq!(
        lines(&first),
        ["text: Got the report from \"Ship the health check: part B\"."]
    );
    handle
        .send_message(
            "[Calin] Report from child session \"Ship the health check: part A\" (review)\n\n\
             Added a line.\n",
        )
        .unwrap();
    let second = check_turn(&collect_turn(&mut handle).await);
    assert_eq!(
        lines(&second),
        [
            "text: Got the report from \"Ship the health check: part A\".",
            "text: Summary: All 2 parts are done: \"Ship the health check: part B\" and \
             \"Ship the health check: part A\". Each child's change is in its own session.",
        ]
    );
    assert_eq!(endpoint.calls().len(), 3, "reports start no tool call");
}

#[tokio::test]
async fn orchestrate_with_note_asks_the_second_child_for_a_note() {
    let endpoint = fake(canned).await;
    let mut handle = start_with(
        Some(&endpoint.url),
        PermissionMode::Autonomous,
        "Ship the health check demo: orchestrate demo: note",
    );
    collect_turn(&mut handle).await;
    let briefs: Vec<String> = endpoint
        .calls()
        .iter()
        .take(2)
        .map(|call| {
            call["params"]["arguments"]["brief"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    assert!(briefs[0].ends_with("demo: child"), "{briefs:?}");
    assert!(briefs[1].ends_with("demo: note"), "{briefs:?}");
}

#[tokio::test]
async fn a_child_edits_the_notes_file_and_reports() {
    let endpoint = fake(canned).await;
    let brief = "[Calin] Brief from the session that dispatched you (\"Ship it demo: \
                 orchestrate\"). It was written by that session's agent, not by the user.\
                 \n\nPart A of \"Ship it\": add a line.\n\ndemo: child";
    let mut handle = start_with(Some(&endpoint.url), PermissionMode::Autonomous, brief);
    let items = check_turn(&collect_turn(&mut handle).await);
    let summary = "Added a line to calin-mock-notes.md; nothing is committed. The brief was: \
                   Part A of \"Ship it\": add a line.";
    assert_eq!(items.len(), 2);
    assert_eq!(
        tool_items(&items),
        [
            (ToolKind::Edit, ItemStatus::Completed),
            (ToolKind::Other, ItemStatus::Completed)
        ]
    );
    assert_eq!(
        lines(&items[1..]),
        ["completed mcp__calin__report Report to the parent -> Recorded. End your turn now."]
    );
    assert_eq!(
        endpoint.calls(),
        [call(1, "report", json!({ "summary": summary }))]
    );
    assert_eq!(
        handle.notes().as_deref(),
        Some("- turn 1: Part A of \"Ship it\": add a line.\n")
    );
}

#[tokio::test]
async fn a_child_told_to_note_sends_one_to_its_parent_before_reporting() {
    let endpoint = fake(canned).await;
    let mut handle = start_with(
        Some(&endpoint.url),
        PermissionMode::Autonomous,
        "Part B. demo: note",
    );
    let items = check_turn(&collect_turn(&mut handle).await);
    assert_eq!(
        tool_items(&items),
        [
            (ToolKind::Edit, ItemStatus::Completed),
            (ToolKind::Other, ItemStatus::Completed),
            (ToolKind::Other, ItemStatus::Completed)
        ]
    );
    assert_eq!(
        endpoint.calls(),
        [
            call(
                1,
                "send_message",
                json!({
                    "to": "parent",
                    "text": "Half way: the notes file has its new line. The report follows.",
                })
            ),
            call(
                2,
                "report",
                json!({
                    "summary": "Added a line to calin-mock-notes.md; nothing is \
                                committed. The brief was: Part B.",
                })
            ),
        ]
    );
}

#[tokio::test]
async fn ask_puts_a_question_and_echoes_the_answer() {
    let endpoint = fake(canned).await;
    let mut handle = start_with(
        Some(&endpoint.url),
        PermissionMode::Autonomous,
        "Which database? demo: ask",
    );
    let items = check_turn(&collect_turn(&mut handle).await);
    assert_eq!(
        lines(&items),
        [
            "text: Before I go on, I need a decision from you.",
            "completed mcp__calin__ask_user Ask the user -> The question was put to the user \
             (question q1).",
        ]
    );
    assert_eq!(
        endpoint.calls(),
        [call(
            1,
            "ask_user",
            json!({
                "question": "Which way should I go for \"Which database?\"?",
                "options": ["The simple way", "The thorough way"],
            })
        )]
    );
    handle
        .send_message("[Calin] The user answered: The thorough way")
        .unwrap();
    let answered = check_turn(&collect_turn(&mut handle).await);
    assert_eq!(
        lines(&answered),
        ["text: You answered: The thorough way. I'll go on with that."]
    );
    assert_eq!(endpoint.calls().len(), 1);
}

#[tokio::test]
async fn without_a_usable_endpoint_a_directive_says_so_and_calls_nothing() {
    let endpoint = fake(canned).await;
    let port = endpoint.url[17..].split('/').next().unwrap().to_owned();
    for (url, reason) in [
        (None, "none was given"),
        (
            Some(format!("http://192.0.2.1:{port}{PATH}")),
            "127.0.0.1 or localhost",
        ),
        (
            Some(format!("https://127.0.0.1:{port}{PATH}")),
            "plain http://",
        ),
    ] {
        for prompt in [
            "Go demo: orchestrate",
            "demo: child",
            "demo: ask",
            "demo: note",
        ] {
            let mut handle = start_with(url.as_deref(), PermissionMode::Ask, prompt);
            let items = check_turn(&collect_turn(&mut handle).await);
            let [only] = lines(&items).try_into().unwrap();
            assert!(
                only.starts_with("text: Calin's tool endpoint is unavailable (")
                    && only.contains(reason),
                "{only}"
            );
            assert_eq!(handle.notes(), None);
        }
    }
    assert!(endpoint.calls().is_empty());
}

#[tokio::test]
async fn a_failing_call_completes_as_failed_and_the_script_goes_on() {
    let endpoint = fake(|call| match call["id"].as_u64() {
        Some(1) => result("4 child sessions of this session are still running.", true),
        Some(3) => Answer::Error("unknown tool: send_message"),
        _ => canned(call),
    })
    .await;
    let mut handle = start_with(
        Some(&endpoint.url),
        PermissionMode::Autonomous,
        "Two things demo: orchestrate",
    );
    let items = check_turn(&collect_turn(&mut handle).await);
    assert_eq!(
        lines(&items)[1..],
        [
            "failed mcp__calin__dispatch Dispatch Two things: part A -> 4 child sessions of \
             this session are still running.",
            "completed mcp__calin__dispatch Dispatch Two things: part B -> Dispatched child \
             session \"Two things: part B\" (id 01CHILD).",
            "failed mcp__calin__send_message Note to the user -> unknown tool: send_message",
        ]
    );
    assert_eq!(endpoint.tools(), ["dispatch", "dispatch", "send_message"]);
    // Only part B is under way: its report is the last one expected.
    handle
        .send_message("[Calin] Report from child session \"Two things: part B\" (review)\n\nok\n")
        .unwrap();
    let reported = lines(&check_turn(&collect_turn(&mut handle).await));
    assert_eq!(reported.len(), 2, "{reported:?}");
    assert!(
        reported[1].starts_with("text: Summary: All 1 parts"),
        "{reported:?}"
    );

    // Nobody listening: every call fails, the turn still ends.
    let closed = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "http://localhost:{}{PATH}",
        closed.local_addr().unwrap().port()
    );
    drop(closed);
    let mut handle = start_with(Some(&url), PermissionMode::Autonomous, "demo: orchestrate");
    let items = check_turn(&collect_turn(&mut handle).await);
    assert_eq!(
        tool_items(&items),
        [(ToolKind::Other, ItemStatus::Failed); 3]
    );
    assert!(
        lines(&items)[1].contains("the tool endpoint is unreachable"),
        "{items:?}"
    );
}

#[tokio::test]
async fn in_ask_mode_each_call_asks_as_one_of_calins_tools_and_a_denial_is_declined() {
    let endpoint = fake(canned).await;
    let mut handle = start_with(
        Some(&endpoint.url),
        PermissionMode::Ask,
        "Ship the health check demo: orchestrate",
    );
    let turn = run_turn(&mut handle, |request| match request.title.as_str() {
        "Dispatch Ship the health check: part A" => {
            (PermissionDecision::Deny, Some("not that one".into()))
        }
        _ => (PermissionDecision::Allow, None),
    })
    .await;
    assert_eq!(turn.access, vec![ToolAccess::Orchestrator; 3]);
    let asked: Vec<(&str, &str)> = turn
        .requests
        .iter()
        .map(|request| (request.tool_name.as_str(), request.title.as_str()))
        .collect();
    assert_eq!(
        asked,
        [
            (
                "mcp__calin__dispatch",
                "Dispatch Ship the health check: part A"
            ),
            (
                "mcp__calin__dispatch",
                "Dispatch Ship the health check: part B"
            ),
            ("mcp__calin__send_message", "Note to the user"),
        ]
    );
    let items = check_turn(&turn.events);
    assert_eq!(
        tool_items(&items),
        [
            (ToolKind::Other, ItemStatus::Declined),
            (ToolKind::Other, ItemStatus::Completed),
            (ToolKind::Other, ItemStatus::Completed)
        ]
    );
    assert_eq!(
        lines(&items)[1],
        "declined mcp__calin__dispatch Dispatch Ship the health check: part A -> not that one"
    );
    // The declined call never reached the endpoint.
    assert_eq!(endpoint.tools(), ["dispatch", "send_message"]);
    assert_eq!(endpoint.calls()[0]["id"], 1);
}

#[tokio::test]
async fn the_same_prompt_plays_the_same_events() {
    let endpoint = fake(canned).await;
    let mut one = start_with(
        Some(&endpoint.url),
        PermissionMode::Autonomous,
        "Go demo: orchestrate",
    );
    let first = collect_turn(&mut one).await;
    let mut two = start_with(
        Some(&endpoint.url),
        PermissionMode::Autonomous,
        "Go demo: orchestrate",
    );
    assert_eq!(first, collect_turn(&mut two).await);
    let calls = endpoint.calls();
    assert_eq!(calls[..3], calls[3..]);
}

#[tokio::test]
async fn a_stop_during_a_call_ends_the_turn_and_the_agent_promptly() {
    let endpoint = fake(|_| Answer::Hang).await;
    let mut handle = start_with(
        Some(&endpoint.url),
        PermissionMode::Autonomous,
        "demo: orchestrate",
    );
    let mut events = Vec::new();
    // Up to the start of the first call, which never gets an answer.
    loop {
        match next(&mut handle).await {
            Some(RunnerOutput::Event(body)) => {
                let calling = matches!(
                    &body,
                    EventBody::ItemStarted { item } if matches!(item.content, ItemContent::Tool { .. })
                );
                events.push(body);
                if calling {
                    break;
                }
            }
            Some(RunnerOutput::NativeSession { .. }) => {}
            other => panic!("unexpected output {other:?}"),
        }
    }
    handle.stop().unwrap();
    loop {
        match next(&mut handle).await {
            Some(RunnerOutput::Event(body)) => events.push(body),
            Some(RunnerOutput::Exited(Exit::Ok)) => break,
            other => panic!("unexpected output {other:?}"),
        }
    }
    let items = check_turn(&events);
    assert_eq!(tool_items(&items), [(ToolKind::Other, ItemStatus::Failed)]);
    assert_eq!(next(&mut handle).await, None);
}

#[tokio::test]
async fn an_unknown_directive_plays_the_fixed_turn() {
    let endpoint = fake(canned).await;
    let mut handle = start_with(
        Some(&endpoint.url),
        PermissionMode::Autonomous,
        "Fix it demo: juggle",
    );
    let items = check_turn(&collect_turn(&mut handle).await);
    assert_eq!(items.len(), 8);
    handle.send_message("and again").unwrap();
    assert_eq!(check_turn(&collect_turn(&mut handle).await).len(), 8);
    assert!(endpoint.calls().is_empty());
}
