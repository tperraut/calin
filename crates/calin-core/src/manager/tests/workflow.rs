//! Workflow runs inside a session (workflows spec, section 6): each node
//! type, the events a run leaves, what the user can do meanwhile, the budget
//! and a restart. The lead agent is the puppet runner, played by hand.

use calin_proto::{
    NodePhase, NodeRunState, Question, Settings, WorkflowEdge, WorkflowNodeRun, WorkflowRun,
    WorkflowStatus,
};

use super::orchestration::{
    Played, all_events, input, nothing_for, play, received, say, sent, status,
};
use super::*;

/// The spec's example, adapted: the review is a turn of the lead, the check
/// passes once `fixed.txt` exists, and a rejected gate leads to a rework.
const EXAMPLE: &str = r#"
version: 1
name: Implement, test, review
nodes:
  - { id: start, type: start }
  - { id: implement, type: agent, prompt: "{{task}}" }
  - { id: tests, type: check, command: "test -f fixed.txt || { echo fixed.txt is missing; exit 3; }" }
  - { id: retry, type: loop, max: 2 }
  - id: fix
    type: agent
    prompt: |
      The checks failed (attempt {{nodes.retry.iteration}} of {{nodes.retry.max}}, exit {{nodes.tests.exit_code}}):
      {{nodes.tests.output}}
  - { id: review, type: agent, prompt: "Review the changes of {{session.title}}.", verdicts: [approve, changes] }
  - { id: gate, type: gate, message: "Open pull requests for {{session.branch}}?" }
  - { id: rework, type: agent, prompt: "The reviewer said: {{nodes.gate.comment}}" }
  - { id: ok, type: end }
  - { id: gave-up, type: end, status: failed }
edges:
  - { from: start, to: implement }
  - { from: implement.done, to: tests }
  - { from: tests.pass, to: review }
  - { from: tests.fail, to: retry }
  - { from: retry.repeat, to: fix }
  - { from: retry.exhausted, to: gave-up }
  - { from: fix.done, to: tests }
  - { from: review.approve, to: gate }
  - { from: review.changes, to: retry }
  - { from: gate.approve, to: ok }
  - { from: gate.reject, to: rework }
  - { from: rework.done, to: ok }
"#;

/// The sentence the engine adds to the prompt of a node with these verdicts.
fn verdicts(words: &[&str]) -> String {
    let words: Vec<String> = words.iter().map(|word| (*word).to_owned()).collect();
    calin_workflow::verdict_instruction(&words)
}

/// A machine whose home repo holds workflows.
struct Flow {
    manager: SessionManager,
    starts: Starts,
    dir: TempDir,
    clones: TempDir,
    home: PathBuf,
}

async fn flow(files: &[(&str, &str)]) -> Flow {
    flow_on(false, "", files).await
}

/// `repos_yaml` is the home repo's `.calin/repos.yaml`, when it has one.
async fn flow_on(dry_run: bool, repos_yaml: &str, files: &[(&str, &str)]) -> Flow {
    let dir = TempDir::new().unwrap();
    let (manager, starts) = manager_in(&dir, dry_run).await;
    let clones = TempDir::new().unwrap();
    let home = git_repo(clones.path(), "home");
    for (name, yaml) in files {
        write_file(&home, &format!(".calin/workflows/{name}.yaml"), yaml);
    }
    if !repos_yaml.is_empty() {
        write_file(&home, ".calin/repos.yaml", repos_yaml);
    }
    let settings = Settings {
        home_repo: Some(home.to_str().unwrap().to_owned()),
        ..Settings::default()
    };
    manager.update_settings(settings).await.unwrap();
    Flow {
        manager,
        starts,
        dir,
        clones,
        home,
    }
}

impl Flow {
    /// A registered clone that commits with an identity of its own.
    async fn repo(&self, name: &str) -> Repo {
        let clone = git_repo(self.clones.path(), name);
        for (key, value) in [
            ("user.name", "Calin Test"),
            ("user.email", "test@calin.invalid"),
            ("commit.gpgsign", "false"),
            ("core.hooksPath", "/dev/null"),
        ] {
            git(&clone, &["config", key, value]);
        }
        let path = clone.to_str().unwrap().to_owned();
        let request = RegisterRepoRequest { path, name: None };
        self.manager.register_repo(request).await.unwrap()
    }

    /// A session of the puppet runner on this workflow.
    async fn session(&self, workflow: &str, prompt: &str, repos: &[&Repo]) -> String {
        let mut wanted = request(prompt, Some("puppet"));
        wanted.workflow = Some(workflow.to_owned());
        wanted.repos = repos.iter().map(|repo| attach(repo, None)).collect();
        let created = self.manager.create_session(wanted).await.unwrap();
        assert_eq!(created.session.workflow.as_deref(), Some(workflow));
        created.session.id
    }

    async fn run(&self, id: &str) -> WorkflowRun {
        self.manager.workflow_run(id).await.unwrap()
    }

    /// Where a session without repos works, and its checks run.
    fn work_dir(&self, id: &str) -> PathBuf {
        self.manager.store().home().work_dir(id)
    }

    /// The session's events up to the first with this label.
    async fn until(&self, id: &str, wanted: &str) -> Vec<Event> {
        events_until(&self.manager, id, 0, |event| label(event) == wanted).await
    }

    /// The session's events once its run is over and it rests in `review`.
    async fn reviewed(&self, id: &str) -> Vec<Event> {
        events_until(&self.manager, id, 0, is_status(SessionStatus::Review)).await
    }

    /// The question the session waits on after `after`, and where the wait began.
    async fn asked(&self, id: &str, after: u64) -> (Question, u64) {
        let waiting = is_status(SessionStatus::Waiting);
        let events = events_until(&self.manager, id, after, waiting).await;
        let question = events.iter().rev().find_map(|event| match &event.body {
            EventBody::QuestionAsked { question } => Some(question.clone()),
            _ => None,
        });
        (question.expect("a question"), events.last().unwrap().seq)
    }

    /// The index rebuilt from a copy of the logs gives the same run.
    async fn assert_survives_a_reindex(&self, id: &str) {
        let live = self.manager.store().workflow_run(id).await.unwrap();
        let copy = TempDir::new().unwrap();
        let sessions = copy.path().join("sessions");
        for entry in std::fs::read_dir(self.dir.path().join("sessions")).unwrap() {
            let from = entry.unwrap().path();
            let to = sessions.join(from.file_name().unwrap());
            std::fs::create_dir_all(&to).unwrap();
            for file in ["session.json", "events.ndjson"] {
                std::fs::copy(from.join(file), to.join(file)).unwrap();
            }
        }
        let store = Store::open(Home::new(copy.path())).await.unwrap();
        let rebuilt = store.workflow_run(id).await.unwrap();
        assert_eq!(rebuilt, live);
        let session = store.session(id).await.unwrap().session;
        let known = self.manager.get(id).await.unwrap().session;
        assert_eq!(session.workflow, known.workflow);
        store.close().await;
    }
}

/// The lead agent as the test plays it.
#[derive(Default)]
struct Lead {
    played: Option<Played>,
    items: u32,
}

impl Lead {
    /// What the lead is handed for its next turn: the prompt it starts
    /// with, then one message per turn.
    async fn prompt(&mut self, starts: &mut Starts) -> String {
        match &mut self.played {
            Some(played) => input(played).await,
            None => {
                let played = play(starts).await;
                let prompt = played.params.prompt.clone();
                self.played = Some(played);
                prompt
            }
        }
    }

    fn played(&mut self) -> &mut Played {
        self.played.as_mut().expect("the lead has started")
    }

    async fn say(&mut self, text: &str) {
        self.items += 1;
        let id = format!("said-{}", self.items);
        say(self.played(), &id, text).await;
    }

    /// Says `text` and ends the turn.
    async fn answer(&mut self, text: &str) {
        self.say(text).await;
        emit(&self.played().io, RunnerOutput::TurnEnded).await;
    }

    fn cwd(&mut self) -> PathBuf {
        self.played().params.cwd.clone()
    }
}

/// A child session's agent says `text` and ends its turn.
async fn report(child: &Played, text: &str) {
    say(child, "report", text).await;
    emit(&child.io, RunnerOutput::TurnEnded).await;
}

fn workflow_labels(events: &[Event]) -> Vec<String> {
    let all = labels(events).into_iter();
    all.filter(|label| label.starts_with("workflow_")).collect()
}

fn node<'a>(run: &'a WorkflowRun, id: &str) -> &'a WorkflowNodeRun {
    let found = run.nodes.iter().find(|node| node.id == id);
    found.unwrap_or_else(|| panic!("no node {id}"))
}

fn edge(from: &str, port: &str, to: &str) -> WorkflowEdge {
    WorkflowEdge {
        from: from.to_owned(),
        port: port.to_owned(),
        to: to.to_owned(),
    }
}

/// The `finished` event of `node`, the last one when it ran more than once.
fn finished<'a>(events: &'a [Event], node: &str) -> (&'a Option<String>, &'a Option<String>) {
    let found = events.iter().rev().find_map(|event| match &event.body {
        EventBody::WorkflowNode {
            node_id,
            phase: NodePhase::Finished,
            port,
            outcome,
        } if node_id == node => Some((port, outcome)),
        _ => None,
    });
    found.unwrap_or_else(|| panic!("{node} never finished"))
}

fn finish_reason(events: &[Event]) -> (WorkflowStatus, Option<String>) {
    let found = events.iter().find_map(|event| match &event.body {
        EventBody::WorkflowFinished { status, reason } => Some((*status, reason.clone())),
        _ => None,
    });
    found.expect("the run finished")
}

/// The check commands on record: title, status and output of each tool item
/// the orchestrator ran, in order.
fn checks(events: &[Event]) -> Vec<(String, ItemStatus, String)> {
    let completed = events.iter().filter_map(|event| match &event.body {
        EventBody::ItemCompleted { item } => Some((event, item)),
        _ => None,
    });
    completed
        .filter_map(|(event, item)| match &item.content {
            ItemContent::Tool {
                name,
                tool_kind,
                title,
                output,
                ..
            } if name == "check" => {
                assert_eq!(event.agent_id, None, "a check is nobody's tool call");
                assert_eq!(*tool_kind, ToolKind::Execute);
                Some((
                    title.clone(),
                    item.status,
                    output.clone().unwrap_or_default(),
                ))
            }
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_run_goes_from_node_to_node_and_ends_in_review() {
    let mut flow = flow(&[("example", EXAMPLE)]).await;
    let id = flow.session("example", "Add a flag", &[]).await;
    let mut lead = Lead::default();

    assert_eq!(lead.prompt(&mut flow.starts).await, "Add a flag");
    assert_eq!(status(&flow.manager, &id).await, SessionStatus::Running);
    std::fs::write(lead.cwd().join("fixed.txt"), "yes").unwrap();
    lead.answer("Added the flag.").await;

    let review = lead.prompt(&mut flow.starts).await;
    let wanted = verdicts(&["approve", "changes"]);
    assert_eq!(
        review,
        format!("Review the changes of Add a flag.\n\n{wanted}")
    );
    lead.answer("Looks right.\nVERDICT: approve").await;

    let (question, _) = flow.asked(&id, 0).await;
    let branch = calin_git::session_branch_name("Add a flag", &id);
    assert_eq!(question.text, format!("Open pull requests for {branch}?"));
    assert!(question.gate);
    assert_eq!(question.options, ["approve", "reject"]);
    let waiting = flow.until(&id, "session_status:waiting").await;
    assert_eq!(
        waiting.last().unwrap().body,
        EventBody::SessionStatus {
            status: SessionStatus::Waiting,
            reason: Some("gate".to_owned()),
        }
    );
    let run = flow.run(&id).await;
    assert_eq!(run.status, WorkflowStatus::Running);
    assert_eq!(node(&run, "gate").state, NodeRunState::Active);
    assert_eq!(node(&run, "review").outcome.as_deref(), Some("approve"));
    flow.assert_survives_a_reindex(&id).await;

    flow.manager
        .answer_question(&id, &question.id, "approve")
        .await
        .unwrap();
    let events = flow.reviewed(&id).await;
    assert_eq!(finish_reason(&events), (WorkflowStatus::Success, None));
    // The gate led straight to the end: the session never ran again.
    let tail = labels(&events[events.len() - 6..]);
    assert_eq!(
        tail,
        [
            "question_answered:approve:user",
            "workflow_node:gate:finished:approve",
            "workflow_node:ok:started",
            "workflow_node:ok:finished",
            "workflow_finished:success",
            "session_status:review",
        ]
    );
    // The user's own prompt is the only user message: the prompts of the
    // nodes are the workflow's.
    let said = labels(&events);
    let user: Vec<&String> = said
        .iter()
        .filter(|label| label.starts_with("user_message"))
        .collect();
    assert_eq!(user, ["user_message:Add a flag"]);
    assert_eq!(
        checks(&events),
        [(
            "test -f fixed.txt || { echo fixed.txt is missing; exit 3; }".to_owned(),
            ItemStatus::Completed,
            String::new()
        )]
    );

    let run = flow.run(&id).await;
    assert_eq!(run.workflow_id, "example");
    assert_eq!(run.name, "Implement, test, review");
    assert_eq!(run.status, WorkflowStatus::Success);
    assert_eq!(run.reason, None);
    assert_eq!(run.budget_usd, None);
    assert_eq!(run.cost_usd, 0.0);
    assert_eq!(
        run.taken,
        [
            edge("start", "next", "implement"),
            edge("implement", "done", "tests"),
            edge("tests", "pass", "review"),
            edge("review", "approve", "gate"),
            edge("gate", "approve", "ok"),
        ]
    );
    let ids: Vec<&str> = run.nodes.iter().map(|node| node.id.as_str()).collect();
    let graph: Vec<&str> = run.graph.nodes.iter().map(|n| n.id.as_str()).collect();
    assert_eq!(ids, graph);
    for done in ["start", "implement", "tests", "review", "gate", "ok"] {
        assert_eq!(node(&run, done).state, NodeRunState::Finished, "{done}");
        assert_eq!(node(&run, done).runs, 1, "{done}");
    }
    for idle in ["retry", "fix", "rework", "gave-up"] {
        assert_eq!(node(&run, idle).state, NodeRunState::Idle, "{idle}");
        assert_eq!(node(&run, idle).runs, 0, "{idle}");
    }
    assert_eq!(node(&run, "implement").last_port.as_deref(), Some("done"));
    assert_eq!(
        node(&run, "implement").outcome.as_deref(),
        Some("Added the flag.")
    );
    assert_eq!(node(&run, "tests").outcome.as_deref(), Some("passed"));
    assert_eq!(node(&run, "gate").outcome.as_deref(), Some("approve"));
    assert_eq!(node(&run, "ok").last_port, None);
    // A loop nobody entered still says its limit.
    assert_eq!(node(&run, "retry").iteration, Some(0));
    assert_eq!(node(&run, "retry").max, Some(2));
    assert_eq!(node(&run, "tests").iteration, None);
    flow.assert_survives_a_reindex(&id).await;

    let detail = flow.manager.get(&id).await.unwrap();
    assert_eq!(detail.session.status, SessionStatus::Review);
    assert_eq!(detail.session.workflow.as_deref(), Some("example"));
    let listed = flow.manager.list().await.unwrap();
    assert_eq!(listed[0].workflow.as_deref(), Some("example"));
}

#[tokio::test]
async fn a_failing_check_loops_through_the_fix_until_it_passes() {
    let mut flow = flow(&[("example", EXAMPLE)]).await;
    let id = flow.session("example", "Add a flag", &[]).await;
    let mut lead = Lead::default();

    lead.prompt(&mut flow.starts).await;
    lead.answer("Added the flag, untested.").await;
    // The fix is told what the check said, and which attempt this is.
    assert_eq!(
        lead.prompt(&mut flow.starts).await,
        "The checks failed (attempt 1 of 2, exit 3):\nfixed.txt is missing\n\n"
    );
    std::fs::write(lead.cwd().join("fixed.txt"), "yes").unwrap();
    lead.answer("Fixed.").await;
    assert!(
        lead.prompt(&mut flow.starts)
            .await
            .starts_with("Review the changes")
    );
    lead.answer("VERDICT: approve").await;
    let (question, _) = flow.asked(&id, 0).await;
    flow.manager
        .answer_question(&id, &question.id, "approve")
        .await
        .unwrap();
    let events = flow.reviewed(&id).await;

    let command = "test -f fixed.txt || { echo fixed.txt is missing; exit 3; }".to_owned();
    assert_eq!(
        checks(&events),
        [
            (
                command.clone(),
                ItemStatus::Failed,
                "fixed.txt is missing\n".to_owned()
            ),
            (command, ItemStatus::Completed, String::new()),
        ]
    );
    let run = flow.run(&id).await;
    assert_eq!(run.status, WorkflowStatus::Success);
    assert_eq!(node(&run, "tests").runs, 2);
    assert_eq!(node(&run, "tests").last_port.as_deref(), Some("pass"));
    let retry = node(&run, "retry");
    assert_eq!(
        (retry.runs, retry.iteration, retry.max),
        (1, Some(1), Some(2))
    );
    assert_eq!(retry.last_port.as_deref(), Some("repeat"));
    assert_eq!(retry.outcome.as_deref(), Some("iteration 1 of 2"));
    assert_eq!(
        run.taken[1..6],
        [
            edge("implement", "done", "tests"),
            edge("tests", "fail", "retry"),
            edge("retry", "repeat", "fix"),
            edge("fix", "done", "tests"),
            edge("tests", "pass", "review"),
        ]
    );
    // What the first check said is on its node's event too.
    let failed = events.iter().find_map(|event| match &event.body {
        EventBody::WorkflowNode {
            node_id,
            port: Some(port),
            outcome,
            ..
        } if node_id == "tests" && port == "fail" => outcome.clone(),
        _ => None,
    });
    let failed = failed.unwrap();
    assert!(
        failed.contains("failed") && failed.contains("exit status 3"),
        "{failed}"
    );
    flow.assert_survives_a_reindex(&id).await;
}

#[tokio::test]
async fn a_loop_that_is_exhausted_ends_the_run_failed_and_the_session_in_review() {
    let mut flow = flow(&[("example", EXAMPLE)]).await;
    let id = flow.session("example", "Add a flag", &[]).await;
    let mut lead = Lead::default();

    lead.prompt(&mut flow.starts).await;
    lead.answer("Did not fix it.").await;
    for attempt in 1..=2 {
        let prompt = lead.prompt(&mut flow.starts).await;
        let wanted = format!("The checks failed (attempt {attempt} of 2, exit 3):");
        assert!(prompt.starts_with(&wanted), "{prompt}");
        lead.answer("Still not.").await;
    }
    let events = flow.reviewed(&id).await;
    assert_eq!(
        finish_reason(&events),
        (
            WorkflowStatus::Failed,
            Some("reached end node `gave-up`".to_owned())
        )
    );
    let tail = labels(&events[events.len() - 8..]);
    assert_eq!(
        tail,
        [
            "item_completed",
            "workflow_node:tests:finished:fail",
            "workflow_node:retry:started",
            "workflow_node:retry:finished:exhausted",
            "workflow_node:gave-up:started",
            "workflow_node:gave-up:finished",
            "workflow_finished:failed",
            "session_status:review",
        ]
    );
    // Never `failed`: that is for an agent that crashed.
    let detail = flow.manager.get(&id).await.unwrap();
    assert_eq!(detail.session.status, SessionStatus::Review);
    assert_eq!(detail.agents[0].status, AgentStatus::Idle);

    let run = flow.run(&id).await;
    assert_eq!(run.status, WorkflowStatus::Failed);
    assert_eq!(run.reason.as_deref(), Some("reached end node `gave-up`"));
    let retry = node(&run, "retry");
    // Entered three times, repeated twice: the count stops at the limit.
    assert_eq!(
        (retry.runs, retry.iteration, retry.max),
        (3, Some(2), Some(2))
    );
    assert_eq!(retry.last_port.as_deref(), Some("exhausted"));
    assert_eq!(retry.outcome.as_deref(), Some("exhausted after 2"));
    assert_eq!(node(&run, "tests").runs, 3);
    assert_eq!(node(&run, "fix").runs, 2);
    assert_eq!(node(&run, "gave-up").state, NodeRunState::Finished);
    assert_eq!(
        run.taken.last(),
        Some(&edge("retry", "exhausted", "gave-up"))
    );
    flow.assert_survives_a_reindex(&id).await;

    // The session is an ordinary one now: a message starts a plain turn.
    let before = events.last().unwrap().seq;
    flow.manager.send_message(&id, "try again").await.unwrap();
    assert_eq!(lead.prompt(&mut flow.starts).await, "try again");
    lead.answer("Tried.").await;
    let after = events_until(&flow.manager, &id, before, is_status(SessionStatus::Review)).await;
    assert!(workflow_labels(&after).is_empty(), "{:?}", labels(&after));
    assert_eq!(flow.run(&id).await, run);
}

#[tokio::test]
async fn a_verdict_picks_the_port_and_an_answer_without_one_fails_the_node() {
    let mut flow = flow(&[("example", EXAMPLE)]).await;
    let id = flow.session("example", "Add a flag", &[]).await;
    let mut lead = Lead::default();

    lead.prompt(&mut flow.starts).await;
    std::fs::write(lead.cwd().join("fixed.txt"), "yes").unwrap();
    lead.answer("Added.").await;
    lead.prompt(&mut flow.starts).await;
    // The last verdict line counts, whatever was said before it.
    lead.answer("VERDICT: approve\nOn second thought:\nverdict: changes")
        .await;
    // `changes` leads back through the loop to the fix.
    let fix = lead.prompt(&mut flow.starts).await;
    assert!(
        fix.starts_with("The checks failed (attempt 1 of 2, exit 0):"),
        "{fix}"
    );
    lead.answer("Changed.").await;
    let again = lead.prompt(&mut flow.starts).await;
    assert!(again.starts_with("Review the changes"), "{again}");
    lead.answer("I approve of this.").await;

    let events = flow.reviewed(&id).await;
    // `failed` is wired to nothing, and is a failure port.
    let reason = "node `review` left through `failed`, which has no outgoing edge";
    assert_eq!(
        finish_reason(&events),
        (WorkflowStatus::Failed, Some(reason.to_owned()))
    );
    assert_eq!(status(&flow.manager, &id).await, SessionStatus::Review);
    let verdicts: Vec<(Option<String>, Option<String>)> = events
        .iter()
        .filter_map(|event| match &event.body {
            EventBody::WorkflowNode {
                node_id,
                phase: NodePhase::Finished,
                port,
                outcome,
            } if node_id == "review" => Some((port.clone(), outcome.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        verdicts,
        [
            (Some("changes".to_owned()), Some("changes".to_owned())),
            (
                Some("failed".to_owned()),
                Some("no verdict in the answer".to_owned())
            ),
        ]
    );
    let run = flow.run(&id).await;
    assert_eq!(node(&run, "review").runs, 2);
    assert_eq!(node(&run, "review").last_port.as_deref(), Some("failed"));
    assert!(run.taken.contains(&edge("review", "changes", "retry")));
    assert_eq!(node(&run, "gate").state, NodeRunState::Idle);
    flow.assert_survives_a_reindex(&id).await;
}

#[tokio::test]
async fn a_gate_takes_approve_or_reject_and_its_comment_reaches_later_prompts() {
    let mut flow = flow(&[("example", EXAMPLE)]).await;
    let id = flow.session("example", "Add a flag", &[]).await;
    let mut lead = Lead::default();
    lead.prompt(&mut flow.starts).await;
    std::fs::write(lead.cwd().join("fixed.txt"), "yes").unwrap();
    lead.answer("Added.").await;
    lead.prompt(&mut flow.starts).await;
    lead.answer("VERDICT: approve").await;
    let (question, _) = flow.asked(&id, 0).await;

    // Anything but the two answers is refused, and the gate stays open.
    for answer in ["maybe", "Approve", "approve it"] {
        let refused = flow
            .manager
            .answer(&id, &question.id, answer, Some("why not".to_owned()))
            .await;
        assert!(
            matches!(refused, Err(Error::Invalid(_))),
            "{answer}: {refused:?}"
        );
    }
    assert_eq!(status(&flow.manager, &id).await, SessionStatus::Waiting);
    assert_eq!(
        flow.manager
            .store()
            .open_questions(Some(&id))
            .await
            .unwrap()
            .len(),
        1
    );

    let comment = "  Not yet: rename the flag.\nThen ship.  ".to_owned();
    flow.manager
        .answer(&id, &question.id, "reject", Some(comment))
        .await
        .unwrap();
    assert_eq!(
        lead.prompt(&mut flow.starts).await,
        "The reviewer said: Not yet: rename the flag.\nThen ship."
    );
    let again = flow
        .manager
        .answer_question(&id, &question.id, "approve")
        .await;
    assert!(matches!(again, Err(Error::Conflict(_))), "{again:?}");
    lead.answer("Renamed.").await;

    let events = flow.reviewed(&id).await;
    // `reject` was wired, so the run went on and ended well.
    assert_eq!(finish_reason(&events), (WorkflowStatus::Success, None));
    assert_eq!(
        finished(&events, "gate"),
        (
            &Some("reject".to_owned()),
            &Some("reject: Not yet: rename the flag.".to_owned())
        )
    );
    let run = flow.run(&id).await;
    assert_eq!(
        run.taken[4..],
        [
            edge("gate", "reject", "rework"),
            edge("rework", "done", "ok")
        ]
    );
    // Answering took the session from `waiting` back to `running`.
    let said = labels(&events);
    let at = said
        .iter()
        .position(|label| label == "question_answered:reject:user");
    assert_eq!(
        said[at.unwrap()..][..4],
        [
            "question_answered:reject:user",
            "workflow_node:gate:finished:reject",
            "session_status:running",
            "workflow_node:rework:started",
        ]
    );
}

const ASK: &str = r#"
version: 1
name: Ask first
nodes:
  - { id: start, type: start }
  - { id: color, type: ask, question: "Which color for {{task}}?", options: [blue, green] }
  - { id: paint, type: agent, prompt: "Paint it {{nodes.color.answer}}." }
  - { id: done, type: end }
edges:
  - { from: start, to: color }
  - { from: color, to: paint }
  - { from: paint.done, to: done }
"#;

#[tokio::test]
async fn an_ask_node_waits_for_the_user_and_hands_the_answer_on() {
    let mut flow = flow(&[("ask", ASK)]).await;
    let id = flow.session("ask", "the shed", &[]).await;
    let (question, _) = flow.asked(&id, 0).await;
    assert_eq!(question.text, "Which color for the shed?");
    assert_eq!(question.options, ["blue", "green"]);
    assert!(!question.gate);
    let events = all_events(&flow.manager, &id).await;
    assert_eq!(
        labels(&events),
        [
            "session_status:queued",
            "user_message:the shed",
            "session_status:running",
            "workflow_started:ask",
            "workflow_node:start:started",
            "workflow_node:start:finished:next",
            "workflow_node:color:started",
            "question_asked:Which color for the shed?",
            "session_status:waiting",
        ]
    );
    assert_eq!(
        events.last().unwrap().body,
        EventBody::SessionStatus {
            status: SessionStatus::Waiting,
            reason: Some("question".to_owned()),
        }
    );
    // The workflow asks, not an agent: there is none yet.
    assert_eq!(events[7].agent_id, None);
    assert!(flow.manager.get(&id).await.unwrap().agents.is_empty());

    // Free text is an answer like any other; a comment is not for an ask.
    flow.manager
        .answer(&id, &question.id, "teal", Some("ignored".to_owned()))
        .await
        .unwrap();
    let mut lead = Lead::default();
    assert_eq!(lead.prompt(&mut flow.starts).await, "Paint it teal.");
    lead.answer("Painted.").await;
    let events = flow.reviewed(&id).await;
    assert_eq!(finish_reason(&events), (WorkflowStatus::Success, None));
    assert_eq!(
        finished(&events, "color"),
        (&Some("answered".to_owned()), &Some("teal".to_owned()))
    );
    flow.assert_survives_a_reindex(&id).await;
}

const COMMIT: &str = r#"
version: 1
name: Commit twice
nodes:
  - { id: start, type: start }
  - { id: work, type: agent, prompt: "{{task}}" }
  - { id: save, type: commit, message: "Save: {{task}}" }
  - { id: again, type: commit, message: "Nothing is left" }
  - { id: ok, type: end }
  - { id: odd, type: end, status: failed }
edges:
  - { from: start, to: work }
  - { from: work.done, to: save }
  - { from: save.done, to: again }
  - { from: save.nothing, to: odd }
  - { from: again.nothing, to: ok }
  - { from: again.done, to: odd }
"#;

#[tokio::test]
async fn a_commit_node_commits_what_there_is_and_says_when_there_is_nothing() {
    let mut flow = flow(&[("commit", COMMIT)]).await;
    let api = flow.repo("api").await;
    // Attached for reading: never committed to.
    let docs = flow.repo("docs").await;
    let mut wanted = request("Add the file", Some("puppet"));
    wanted.workflow = Some("commit".to_owned());
    wanted.repos = vec![attach(&api, None), attach(&docs, Some(RepoMode::Read))];
    let id = flow
        .manager
        .create_session(wanted)
        .await
        .unwrap()
        .session
        .id;
    let mut lead = Lead::default();
    lead.prompt(&mut flow.starts).await;
    let worktree = lead.cwd();
    write_file(&worktree, "added.txt", "new\n");
    write_file(Path::new(&docs.path), "scratch.txt", "the user's own\n");
    lead.answer("Added it.").await;

    let events = flow.reviewed(&id).await;
    assert_eq!(finish_reason(&events), (WorkflowStatus::Success, None));
    assert_eq!(
        finished(&events, "save"),
        (
            &Some("done".to_owned()),
            &Some("committed in api".to_owned())
        )
    );
    assert_eq!(
        finished(&events, "again"),
        (
            &Some("nothing".to_owned()),
            &Some("nothing to commit".to_owned())
        )
    );
    assert_eq!(
        git(&worktree, &["log", "-1", "--format=%s"]),
        "Save: Add the file"
    );
    assert_eq!(git(&worktree, &["status", "--porcelain"]), "");
    assert_eq!(git(&worktree, &["rev-list", "--count", "HEAD"]), "2");
    // The repo attached for reading was left as the user had it.
    let docs = Path::new(&docs.path);
    assert_eq!(git(docs, &["status", "--porcelain"]), "?? scratch.txt");
    assert_eq!(git(docs, &["rev-list", "--count", "HEAD"]), "1");
}

const SHIP: &str = r#"
version: 1
name: Ship it
nodes:
  - { id: start, type: start }
  - { id: work, type: agent, prompt: "{{task}}" }
  - { id: ship, type: ship, title: "Ship: {{task}}" }
  - { id: after, type: agent, prompt: "Announce {{nodes.ship.urls}}" }
  - { id: ok, type: end }
edges:
  - { from: start, to: work }
  - { from: work.done, to: ship }
  - { from: ship.done, to: after }
  - { from: after.done, to: ok }
"#;

#[tokio::test]
async fn a_ship_node_in_dry_run_records_pull_requests_and_the_session_ends_in_shipping() {
    let flow = flow_on(true, "", &[("ship", SHIP)]).await;
    let api = flow.repo("api").await;
    let mut wanted = request("Add the thing", None);
    wanted.workflow = Some("ship".to_owned());
    wanted.repos = vec![attach(&api, None)];
    let id = flow
        .manager
        .create_session(wanted)
        .await
        .unwrap()
        .session
        .id;
    let shipping = is_status(SessionStatus::Shipping);
    let events = events_until(&flow.manager, &id, 0, shipping).await;

    assert_eq!(finish_reason(&events), (WorkflowStatus::Success, None));
    let url = "https://example.invalid/api/pull/1";
    assert_eq!(
        finished(&events, "ship"),
        (&Some("done".to_owned()), &Some("1 pull request".to_owned()))
    );
    let said = labels(&events);
    assert!(
        said.contains(&format!("pull_request_opened:{url}")),
        "{said:?}"
    );
    // No `review` on the way: the run ends where the pull requests are.
    assert!(
        !said.contains(&"session_status:review".to_owned()),
        "{said:?}"
    );
    assert_eq!(
        said[said.len() - 4..],
        [
            "workflow_node:ok:started",
            "workflow_node:ok:finished",
            "workflow_finished:success",
            "session_status:shipping",
        ]
    );
    // The node after it was told where the pull requests are.
    let prompts: Vec<String> = events
        .iter()
        .filter_map(|event| match &event.body {
            EventBody::ItemCompleted { item } if event.agent_id.is_none() => match &item.content {
                ItemContent::Text { text } => Some(text.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(
        prompts,
        ["Add the thing".to_owned(), format!("Announce {url}")]
    );
    let detail = flow.manager.get(&id).await.unwrap();
    assert_eq!(detail.session.status, SessionStatus::Shipping);
    assert_eq!(detail.pull_requests.len(), 1);
    assert_eq!(detail.pull_requests[0].title, "Ship: Add the thing");
    let run = flow.run(&id).await;
    assert_eq!(run.status, WorkflowStatus::Success);
    // The mock agent took two turns.
    assert!((run.cost_usd - 0.025).abs() < 1e-9, "{}", run.cost_usd);
    flow.assert_survives_a_reindex(&id).await;
}

#[tokio::test]
async fn a_ship_node_fails_when_no_pull_request_can_be_opened() {
    // Outside dry run, with a GitHub CLI that does not exist.
    let mut flow = flow(&[("ship", SHIP)]).await;
    let api = flow.repo("api").await;
    let id = flow.session("ship", "Add the thing", &[&api]).await;
    let mut lead = Lead::default();
    lead.prompt(&mut flow.starts).await;
    lead.answer("Added.").await;
    let events = flow.reviewed(&id).await;
    let reason = "node `ship` left through `failed`, which has no outgoing edge";
    assert_eq!(
        finish_reason(&events),
        (WorkflowStatus::Failed, Some(reason.to_owned()))
    );
    let (port, outcome) = finished(&events, "ship");
    assert_eq!(port.as_deref(), Some("failed"));
    let outcome = outcome.as_deref().unwrap();
    assert!(
        outcome.starts_with("pull requests cannot be opened"),
        "{outcome}"
    );
    assert!(
        flow.manager
            .get(&id)
            .await
            .unwrap()
            .pull_requests
            .is_empty()
    );
}

const SMALL: &str = r#"
version: 1
name: Small
budget_usd: 5
nodes:
  - { id: start, type: start }
  - { id: work, type: agent, prompt: "Do this: {{task}}" }
  - { id: tests, type: check, command: "echo checked" }
  - { id: gate, type: gate, message: "Ship it?" }
  - { id: end, type: end }
edges:
  - { from: start, to: work }
  - { from: work.done, to: tests }
  - { from: tests.pass, to: gate }
  - { from: gate.approve, to: end }
"#;

#[tokio::test]
async fn a_small_run_leaves_exactly_these_events_and_this_picture_at_each_step() {
    let mut flow = flow(&[("small", SMALL)]).await;
    let id = flow.session("small", "the task", &[]).await;
    let mut lead = Lead::default();
    assert_eq!(lead.prompt(&mut flow.starts).await, "Do this: the task");

    // Step 1: the first node is at work.
    let events = flow.until(&id, "agent_spawned").await;
    assert_eq!(
        labels(&events),
        [
            "session_status:queued",
            "user_message:the task",
            "session_status:running",
            "workflow_started:small",
            "workflow_node:start:started",
            "workflow_node:start:finished:next",
            "workflow_node:work:started",
            "item_completed",
            "agent_spawned",
        ]
    );
    let EventBody::WorkflowStarted {
        workflow_id,
        name,
        graph,
        budget_usd,
    } = &events[3].body
    else {
        panic!("not the start of a run");
    };
    assert_eq!((workflow_id.as_str(), name.as_str()), ("small", "Small"));
    assert_eq!(*budget_usd, Some(5.0));
    assert_eq!(graph.nodes.len(), 5);
    assert_eq!(graph.edges.len(), 4);
    // The node says the line it starts on; the whole prompt follows as
    // text nobody said, caused by the node, and the turn hangs under it.
    assert_eq!(
        events[6].body,
        EventBody::WorkflowNode {
            node_id: "work".to_owned(),
            phase: NodePhase::Started,
            port: None,
            outcome: Some("Do this: the task".to_owned()),
        }
    );
    assert_eq!(events[6].parent_id.as_ref(), Some(&events[3].id));
    let EventBody::ItemCompleted { item } = &events[7].body else {
        panic!("not the prompt");
    };
    assert_eq!(item.status, ItemStatus::Completed);
    assert_eq!(
        item.content,
        ItemContent::Text {
            text: "Do this: the task".to_owned()
        }
    );
    assert_eq!(events[7].agent_id, None);
    assert_eq!(events[7].parent_id.as_ref(), Some(&events[6].id));
    assert_eq!(events[8].parent_id.as_ref(), Some(&events[6].id));

    let run = flow.run(&id).await;
    assert_eq!(run.status, WorkflowStatus::Running);
    assert_eq!(run.budget_usd, Some(5.0));
    assert_eq!(run.taken, [edge("start", "next", "work")]);
    let states: Vec<NodeRunState> = run.nodes.iter().map(|node| node.state).collect();
    assert_eq!(
        states,
        [
            NodeRunState::Finished,
            NodeRunState::Active,
            NodeRunState::Idle,
            NodeRunState::Idle,
            NodeRunState::Idle,
        ]
    );
    assert_eq!(node(&run, "work").runs, 1);
    assert_eq!(node(&run, "work").last_port, None);
    flow.assert_survives_a_reindex(&id).await;

    // Step 2: the agent answers, the check runs, the gate opens.
    let usage = EventBody::Usage {
        cost_usd: 0.25,
        tokens_in: 10,
        tokens_out: 5,
    };
    emit(&lead.played().io, RunnerOutput::Event(usage)).await;
    lead.answer("Did it.").await;
    let (question, _) = flow.asked(&id, 0).await;
    let events = all_events(&flow.manager, &id).await;
    assert_eq!(
        labels(&events[9..]),
        [
            "usage",
            "item_started",
            "item_completed",
            "agent_status:idle",
            "workflow_node:work:finished:done",
            "workflow_node:tests:started",
            "item_started",
            "item_completed",
            "workflow_node:tests:finished:pass",
            "workflow_node:gate:started",
            "question_asked:Ship it?",
            "session_status:waiting",
        ]
    );
    assert_eq!(
        checks(&events),
        [(
            "echo checked".to_owned(),
            ItemStatus::Completed,
            "checked\n".to_owned()
        )]
    );
    let run = flow.run(&id).await;
    assert_eq!(run.status, WorkflowStatus::Running);
    assert_eq!(run.cost_usd, 0.25);
    assert_eq!(node(&run, "gate").state, NodeRunState::Active);
    assert_eq!(node(&run, "work").outcome.as_deref(), Some("Did it."));
    assert_eq!(run.taken.len(), 3);
    flow.assert_survives_a_reindex(&id).await;

    // Step 3: approved, and over.
    flow.manager
        .answer(&id, &question.id, "approve", Some("go".to_owned()))
        .await
        .unwrap();
    flow.reviewed(&id).await;
    let events = all_events(&flow.manager, &id).await;
    assert_eq!(
        labels(&events[21..]),
        [
            "question_answered:approve:user",
            "workflow_node:gate:finished:approve",
            "workflow_node:end:started",
            "workflow_node:end:finished",
            "workflow_finished:success",
            "session_status:review",
        ]
    );
    assert_eq!(events.len(), 27);
    assert_eq!(
        finished(&events, "gate"),
        (&Some("approve".to_owned()), &Some("approve: go".to_owned()))
    );
    let run = flow.run(&id).await;
    assert_eq!(run.status, WorkflowStatus::Success);
    assert!(
        run.nodes
            .iter()
            .all(|node| node.state == NodeRunState::Finished)
    );
    assert_eq!(run.taken.len(), 4);
    flow.assert_survives_a_reindex(&id).await;
}

const FRESH: &str = r#"
version: 1
name: Fresh review
nodes:
  - { id: start, type: start }
  - { id: implement, type: agent, prompt: "{{task}}" }
  - { id: review, type: agent, fresh: true, prompt: "Review the branch.", verdicts: [approve, changes] }
  - { id: polish, type: agent, prompt: "The review said: {{nodes.review.summary}}" }
  - { id: ok, type: end }
  - { id: redo, type: end, status: failed }
edges:
  - { from: start, to: implement }
  - { from: implement.done, to: review }
  - { from: review.approve, to: polish }
  - { from: review.changes, to: redo }
  - { from: polish.done, to: ok }
"#;

#[tokio::test]
async fn a_fresh_agent_runs_in_a_child_session_whose_report_is_the_nodes_result() {
    let mut flow = flow(&[("fresh", FRESH)]).await;
    let api = flow.repo("api").await;
    let id = flow.session("fresh", "Add a flag", &[&api]).await;
    let mut lead = Lead::default();
    lead.prompt(&mut flow.starts).await;
    let worktree = lead.cwd();
    write_file(&worktree, "flag.txt", "on\n");
    git(&worktree, &["add", "-A"]);
    git(&worktree, &["commit", "-q", "-m", "Add the flag"]);
    let commit = git(&worktree, &["rev-parse", "HEAD"]);
    lead.answer("Added the flag.").await;

    // The review is a child session: forked from the lead's commit, told
    // the node's prompt as its brief.
    let child = play(&mut flow.starts).await;
    let wanted = verdicts(&["approve", "changes"]);
    assert!(
        child
            .params
            .prompt
            .ends_with(&format!("\n\nReview the branch.\n\n{wanted}")),
        "{}",
        child.params.prompt
    );
    let detail = flow.manager.get(&child.id).await.unwrap();
    assert_eq!(
        detail.session.parent_session_id.as_deref(),
        Some(id.as_str())
    );
    assert_eq!(detail.session.title, "review");
    assert_eq!(detail.session.workflow, None);
    let child_tree = PathBuf::from(&detail.repos[0].path);
    assert_eq!(git(&child_tree, &["rev-parse", "HEAD"]), commit);
    let branch = detail.repos[0].branch.clone().unwrap();
    assert_eq!(status(&flow.manager, &id).await, SessionStatus::Running);

    report(&child, "Reads well.\nVERDICT: approve").await;
    // The lead gets the next node's prompt, never a turn for the report,
    // and is told first where the child's work is.
    let polish = lead.prompt(&mut flow.starts).await;
    let note = format!(
        "[Calin] The workflow step `review` ran in child sessions, each on branches of its \
         own:\n- review: api on branch `{branch}`\nBring a branch in with `git merge <branch>` \
         from your worktree of that repository."
    );
    assert_eq!(
        polish,
        format!("{note}\n\nThe review said: Reads well.\nVERDICT: approve")
    );
    lead.answer("Polished.").await;
    let events = flow.reviewed(&id).await;
    nothing_for(lead.played());

    assert_eq!(finish_reason(&events), (WorkflowStatus::Success, None));
    assert_eq!(
        finished(&events, "review"),
        (&Some("approve".to_owned()), &Some("approve".to_owned()))
    );
    let said = labels(&events);
    let at = said
        .iter()
        .position(|l| l == "workflow_node:review:started");
    assert_eq!(
        said[at.unwrap()..][..5],
        [
            "workflow_node:review:started",
            "message_sent:dispatch",
            "message_received:report",
            "workflow_node:review:finished:approve",
            "workflow_node:polish:started",
        ]
    );
    // The transcript keeps the node's own prompt, without the note.
    let EventBody::ItemCompleted { item } = &events[at.unwrap() + 5].body else {
        panic!("not the prompt");
    };
    assert_eq!(
        item.content,
        ItemContent::Text {
            text: "The review said: Reads well.\nVERDICT: approve".to_owned()
        }
    );
    // Two turns of the lead, one per node of its own.
    let turns = said.iter().filter(|l| *l == "agent_status:idle").count();
    assert_eq!(turns, 2);
    flow.assert_survives_a_reindex(&id).await;
}

#[tokio::test]
async fn a_fresh_agent_that_fails_takes_the_failed_port() {
    let mut flow = flow(&[("fresh", FRESH)]).await;
    let id = flow.session("fresh", "Add a flag", &[]).await;
    let mut lead = Lead::default();
    lead.prompt(&mut flow.starts).await;
    lead.answer("Added.").await;
    let child = play(&mut flow.starts).await;
    say(&child, "said", "VERDICT: approve").await;
    let crashed = RunnerOutput::Exited(Exit::Error("out of memory".to_owned()));
    emit(&child.io, crashed).await;

    let events = flow.reviewed(&id).await;
    // Its verdict does not count: the child failed.
    let reason = "node `review` left through `failed`, which has no outgoing edge";
    assert_eq!(
        finish_reason(&events),
        (WorkflowStatus::Failed, Some(reason.to_owned()))
    );
    assert_eq!(
        finished(&events, "review"),
        (
            &Some("failed".to_owned()),
            &Some("This child session failed: out of memory.".to_owned())
        )
    );
    assert_eq!(
        status(&flow.manager, &child.id).await,
        SessionStatus::Failed
    );
    assert_eq!(status(&flow.manager, &id).await, SessionStatus::Review);
    nothing_for(lead.played());
}

const PARALLEL: &str = r#"
version: 1
name: Split the work
nodes:
  - { id: start, type: start }
  - id: split
    type: parallel
    branches:
      - { id: docs, prompt: "Write the docs for {{task}}." }
      - { id: tests, prompt: "Write the tests for {{task}}.", repos: [api] }
  - { id: merge, type: agent, prompt: "Merge:\n{{nodes.split.summary}}" }
  - { id: ok, type: end }
  - { id: lost, type: agent, prompt: "A branch failed:\n{{nodes.split.summary}}" }
edges:
  - { from: start, to: split }
  - { from: split.done, to: merge }
  - { from: split.failed, to: lost }
  - { from: merge.done, to: ok }
  - { from: lost.done, to: ok }
"#;

/// The two children of `split`, whichever started first: `docs`, then `tests`.
async fn children(starts: &mut Starts) -> (Played, Played) {
    let (one, other) = (play(starts).await, play(starts).await);
    match one.params.prompt.contains("Write the docs") {
        true => (one, other),
        false => (other, one),
    }
}

#[tokio::test]
async fn a_parallel_node_waits_for_every_child_and_joins_their_reports() {
    let mut flow = flow(&[("parallel", PARALLEL)]).await;
    let api = flow.repo("api").await;
    let web = flow.repo("web").await;
    let id = flow.session("parallel", "the flag", &[&api, &web]).await;

    // No lead yet: the run starts with the children, whose agents start
    // as their repos get ready.
    let (docs, tests) = children(&mut flow.starts).await;
    assert!(docs.params.prompt.ends_with("Write the docs for the flag."));
    assert!(
        tests
            .params
            .prompt
            .ends_with("Write the tests for the flag.")
    );
    let docs_detail = flow.manager.get(&docs.id).await.unwrap();
    let tests_detail = flow.manager.get(&tests.id).await.unwrap();
    assert_eq!(docs_detail.session.title, "split: docs");
    assert_eq!(tests_detail.session.title, "split: tests");
    // Every `write` repo by default, the named ones otherwise.
    let names = |detail: &SessionDetail| -> Vec<String> {
        detail.repos.iter().map(|repo| repo.name.clone()).collect()
    };
    assert_eq!(names(&docs_detail), ["api", "web"]);
    assert_eq!(names(&tests_detail), ["api"]);
    assert!(flow.manager.get(&id).await.unwrap().agents.is_empty());

    // One report is not enough.
    report(&tests, "Tests written.").await;
    events_until(&flow.manager, &id, 0, |event| {
        label(event) == "message_received:report"
    })
    .await;
    let run = flow.run(&id).await;
    assert_eq!(node(&run, "split").state, NodeRunState::Active);
    assert_eq!(status(&flow.manager, &id).await, SessionStatus::Running);
    report(&docs, "Docs written.\n").await;

    let mut lead = Lead::default();
    let merge = lead.prompt(&mut flow.starts).await;
    let branch = |detail: &SessionDetail, repo: &str| -> String {
        let repo = detail.repos.iter().find(|r| r.name == repo).unwrap();
        repo.branch.clone().unwrap()
    };
    // Each report under its branch id, in the order of the file.
    let note = format!(
        "[Calin] The workflow step `split` ran in child sessions, each on branches of its \
         own:\n- docs: api on branch `{}`, web on branch `{}`\n- tests: api on branch `{}`\n\
         Bring a branch in with `git merge <branch>` from your worktree of that repository.",
        branch(&docs_detail, "api"),
        branch(&docs_detail, "web"),
        branch(&tests_detail, "api"),
    );
    assert_eq!(
        merge,
        format!("{note}\n\nMerge:\n## docs\n\nDocs written.\n\n## tests\n\nTests written.")
    );
    lead.answer("Merged.").await;
    let events = flow.reviewed(&id).await;
    assert_eq!(finish_reason(&events), (WorkflowStatus::Success, None));
    assert_eq!(
        finished(&events, "split"),
        (
            &Some("done".to_owned()),
            &Some("2 branches done".to_owned())
        )
    );
    let said = labels(&events);
    let at = said.iter().position(|l| l == "workflow_node:split:started");
    let at = at.unwrap();
    assert_eq!(
        said[at..at + 6],
        [
            "workflow_node:split:started",
            "message_sent:dispatch",
            "message_sent:dispatch",
            "message_received:report",
            "message_received:report",
            "workflow_node:split:finished:done",
        ]
    );
    // The dispatches are made in the name of the lead that starts later.
    let EventBody::MessageSent { message } = &events[at + 1].body else {
        panic!("not a dispatch");
    };
    let lead_agent = flow.manager.get(&id).await.unwrap().agents[0].id.clone();
    assert_eq!(message.from_agent_id.as_ref(), Some(&lead_agent));
    flow.assert_survives_a_reindex(&id).await;

    // A child that reports again later is on record, and starts nothing.
    let before = events.last().unwrap().seq;
    flow.manager
        .send_message(&docs.id, "one more thing")
        .await
        .unwrap();
    let mut docs = docs;
    assert_eq!(input(&mut docs).await, "one more thing");
    report(&docs, "Done as well.").await;
    events_until(&flow.manager, &id, before, |event| {
        label(event) == "message_received:report"
    })
    .await;
    nothing_for(lead.played());
    assert_eq!(status(&flow.manager, &id).await, SessionStatus::Review);
}

#[tokio::test]
async fn a_parallel_node_fails_when_one_child_fails_or_is_stopped() {
    let mut flow = flow(&[("parallel", PARALLEL)]).await;
    let api = flow.repo("api").await;
    let id = flow.session("parallel", "the flag", &[&api]).await;
    let (docs, mut tests) = children(&mut flow.starts).await;
    report(&docs, "Docs written.").await;
    // A stopped child never reports; the node is told all the same.
    let tests_id = tests.id.clone();
    let stopping = flow.manager.stop(&tests_id);
    let obeying = super::orchestration::exit_on_stop(&mut tests);
    let (stopped, ()) = tokio::join!(stopping, obeying);
    stopped.unwrap();

    let mut lead = Lead::default();
    let lost = lead.prompt(&mut flow.starts).await;
    assert!(
        lost.ends_with(
            "A branch failed:\n## docs\n\nDocs written.\n\n## tests\n\nThis child session was \
             stopped before it finished. It did not report."
        ),
        "{lost}"
    );
    assert!(lost.contains("- tests (failed): api on branch"), "{lost}");
    lead.answer("Noted.").await;
    let events = flow.reviewed(&id).await;
    assert_eq!(
        finished(&events, "split"),
        (
            &Some("failed".to_owned()),
            &Some(
                "failed: tests: This child session was stopped before it finished. It did not \
                 report."
                    .to_owned()
            )
        )
    );
    // `failed` was wired, and what it led to ended well.
    assert_eq!(finish_reason(&events), (WorkflowStatus::Success, None));
}

#[tokio::test]
async fn a_fresh_agent_that_is_stopped_takes_the_failed_port_with_what_happened() {
    let mut flow = flow(&[("fresh", FRESH)]).await;
    let id = flow.session("fresh", "Add a flag", &[]).await;
    let mut lead = Lead::default();
    lead.prompt(&mut flow.starts).await;
    lead.answer("Added.").await;
    let mut child = play(&mut flow.starts).await;
    let child_id = child.id.clone();
    let stopping = flow.manager.stop(&child_id);
    let obeying = super::orchestration::exit_on_stop(&mut child);
    let (stopped, ()) = tokio::join!(stopping, obeying);
    stopped.unwrap();

    let events = flow.reviewed(&id).await;
    let line = "This child session was stopped before it finished. It did not report.";
    assert_eq!(
        finished(&events, "review"),
        (&Some("failed".to_owned()), &Some(line.to_owned()))
    );
    let reason = "node `review` left through `failed`, which has no outgoing edge";
    assert_eq!(
        finish_reason(&events),
        (WorkflowStatus::Failed, Some(reason.to_owned()))
    );
    // One report on each side, and the lead gets no turn for it.
    let child_events = all_events(&flow.manager, &child_id).await;
    assert_eq!(sent(&child_events).len(), 1);
    assert_eq!(sent(&child_events)[0].1.body, line);
    assert_eq!(received(&events).len(), 1);
    nothing_for(lead.played());
}

/// Ends the process: the store is closed and everything else dropped. The
/// next manager on the same directory recovers what it finds.
async fn restart(flow: Flow) -> Flow {
    let Flow {
        manager,
        starts,
        dir,
        clones,
        home,
    } = flow;
    manager.store().close().await;
    drop((manager, starts));
    let (manager, starts) = manager_in(&dir, false).await;
    Flow {
        manager,
        starts,
        dir,
        clones,
        home,
    }
}

/// The report a child the restart failed sends.
const RESTARTED: &str = "This child session failed: server restarted. It did not report.";

/// The child's own end after a restart: failed, then its report.
async fn restarted_child(manager: &SessionManager, id: &str) {
    let events = all_events(manager, id).await;
    let labels = labels(&events);
    assert_eq!(
        labels[labels.len() - 2..],
        ["session_status:failed", "message_sent:report"]
    );
    let reports = sent(&events);
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].1.body, RESTARTED);
}

#[tokio::test]
async fn a_fresh_agent_cut_short_by_a_restart_reports_and_the_lead_hears_of_it_later() {
    let mut flow = flow(&[("fresh", FRESH)]).await;
    let id = flow.session("fresh", "Add a flag", &[]).await;
    let mut lead = Lead::default();
    lead.prompt(&mut flow.starts).await;
    lead.answer("Added.").await;
    let child = play(&mut flow.starts).await;
    events_until(&flow.manager, &child.id, 0, |event| {
        label(event) == "agent_spawned"
    })
    .await;
    let before = flow.manager.get(&id).await.unwrap().session.head_seq;
    // The old agents go with the old process, after its store closed.
    let mut flow = restart(flow).await;
    drop(lead);

    restarted_child(&flow.manager, &child.id).await;
    let events = events_until(&flow.manager, &id, before, is_status(SessionStatus::Failed)).await;
    // The report is on record first; the run is not resumed, so the node
    // ends where the restart cut it.
    assert_eq!(
        labels(&events),
        [
            "message_received:report",
            "workflow_node:review:finished",
            "workflow_finished:failed",
            "agent_status:failed",
            "session_status:failed",
        ]
    );
    assert_eq!(received(&events)[0].1.body, RESTARTED);
    let run = flow.run(&id).await;
    assert_eq!(
        node(&run, "review").outcome.as_deref(),
        Some("stopped: server restarted")
    );

    // Continued, the lead is told before anything else, and only once.
    flow.manager.send_message(&id, "go on").await.unwrap();
    let mut lead = Lead::default();
    let prompt = lead.prompt(&mut flow.starts).await;
    assert_eq!(
        prompt,
        format!("[Calin] Report from child session \"review\" (failed)\n\n{RESTARTED}\n\n\ngo on")
    );
    lead.answer("Went on.").await;
    flow.reviewed(&id).await;
    flow.manager.send_message(&id, "more").await.unwrap();
    assert_eq!(lead.prompt(&mut flow.starts).await, "more");
}

#[tokio::test]
async fn a_parallel_branch_cut_short_by_a_restart_reports_and_the_lead_hears_of_both() {
    let mut flow = flow(&[("parallel", PARALLEL)]).await;
    let api = flow.repo("api").await;
    let id = flow.session("parallel", "the flag", &[&api]).await;
    let (docs, tests) = children(&mut flow.starts).await;
    report(&tests, "Tests written.").await;
    events_until(&flow.manager, &id, 0, |event| {
        label(event) == "message_received:report"
    })
    .await;
    events_until(&flow.manager, &docs.id, 0, |event| {
        label(event) == "agent_spawned"
    })
    .await;
    let before = flow.manager.get(&id).await.unwrap().session.head_seq;
    let mut flow = restart(flow).await;

    // The branch that had reported is left as it was.
    restarted_child(&flow.manager, &docs.id).await;
    assert_eq!(
        status(&flow.manager, &tests.id).await,
        SessionStatus::Review
    );
    let events = events_until(&flow.manager, &id, before, is_status(SessionStatus::Failed)).await;
    assert_eq!(
        labels(&events),
        [
            "message_received:report",
            "workflow_node:split:finished",
            "workflow_finished:failed",
            "session_status:failed",
        ]
    );

    // No lead ever ran: when one starts, it hears of both branches.
    flow.manager.send_message(&id, "go on").await.unwrap();
    let mut lead = Lead::default();
    let prompt = lead.prompt(&mut flow.starts).await;
    assert!(
        prompt.starts_with(
            "[Calin] Report from child session \"split: tests\" (review)\n\nTests written.\n"
        ),
        "{prompt}"
    );
    assert!(
        prompt.contains(&format!(
            "[Calin] Report from child session \"split: docs\" (failed)\n\n{RESTARTED}\n"
        )),
        "{prompt}"
    );
    assert!(prompt.ends_with("the flag\n\ngo on"), "{prompt}");
}

const TWO_STEPS: &str = r#"
version: 1
name: Two steps
nodes:
  - { id: start, type: start }
  - { id: work, type: agent, prompt: "{{task}}" }
  - { id: gate, type: gate, message: "Go on after {{nodes.work.summary}}?" }
  - { id: more, type: agent, prompt: "Go on from: {{nodes.work.summary}}" }
  - { id: end, type: end }
edges:
  - { from: start, to: work }
  - { from: work.done, to: gate }
  - { from: gate.approve, to: more }
  - { from: more.done, to: end }
"#;

#[tokio::test]
async fn a_user_message_waits_for_a_turn_of_the_lead_to_end_and_moves_nothing() {
    let mut flow = flow(&[("steps", TWO_STEPS)]).await;
    let id = flow.session("steps", "the task", &[]).await;
    let mut lead = Lead::default();
    lead.prompt(&mut flow.starts).await;

    // Sent while the lead works: handed over when its turn ends, as a turn
    // of its own, with the node still where it was.
    flow.manager
        .send_message(&id, "also the docs")
        .await
        .unwrap();
    nothing_for(lead.played());
    lead.answer("The work is done.").await;
    assert_eq!(lead.prompt(&mut flow.starts).await, "also the docs");
    let run = flow.run(&id).await;
    assert_eq!(node(&run, "work").state, NodeRunState::Active);
    assert_eq!(status(&flow.manager, &id).await, SessionStatus::Running);
    lead.answer("The docs too.").await;

    // The node's result is what its own turn ended on.
    let (question, _) = flow.asked(&id, 0).await;
    assert_eq!(question.text, "Go on after The work is done.?");

    // Sent at the gate: the lead is not started for it, and the gate stays.
    flow.manager
        .send_message(&id, "and the changelog")
        .await
        .unwrap();
    nothing_for(lead.played());
    assert_eq!(status(&flow.manager, &id).await, SessionStatus::Waiting);
    assert_eq!(
        node(&flow.run(&id).await, "gate").state,
        NodeRunState::Active
    );

    flow.manager
        .answer_question(&id, &question.id, "approve")
        .await
        .unwrap();
    assert_eq!(
        lead.prompt(&mut flow.starts).await,
        "Go on from: The work is done."
    );
    lead.answer("Went on.").await;
    // That turn's end is the next boundary: the message from the gate.
    assert_eq!(lead.prompt(&mut flow.starts).await, "and the changelog");
    assert_eq!(
        node(&flow.run(&id).await, "more").state,
        NodeRunState::Active
    );
    lead.answer("Changelog written.").await;

    let events = flow.reviewed(&id).await;
    assert_eq!(finish_reason(&events), (WorkflowStatus::Success, None));
    let said = labels(&events);
    let users: Vec<&String> = said
        .iter()
        .filter(|label| label.starts_with("user_message"))
        .collect();
    assert_eq!(
        users,
        [
            "user_message:the task",
            "user_message:also the docs",
            "user_message:and the changelog"
        ]
    );
    let run = flow.run(&id).await;
    assert!(
        run.nodes.iter().all(|node| node.runs == 1),
        "{:?}",
        run.nodes
    );
}

#[tokio::test]
async fn a_message_nobody_could_take_starts_a_turn_when_the_run_has_ended_well() {
    let mut flow = flow(&[("small", SMALL)]).await;
    let id = flow.session("small", "the task", &[]).await;
    let mut lead = Lead::default();
    lead.prompt(&mut flow.starts).await;
    lead.answer("Did it.").await;
    let (question, _) = flow.asked(&id, 0).await;
    flow.manager
        .send_message(&id, "thanks, now tidy up")
        .await
        .unwrap();
    flow.manager
        .answer_question(&id, &question.id, "approve")
        .await
        .unwrap();
    // The run is over; the session is an ordinary one and takes the turn.
    assert_eq!(lead.prompt(&mut flow.starts).await, "thanks, now tidy up");
    lead.answer("Tidied.").await;
    let after = flow.until(&id, "workflow_finished:success").await;
    let seq = after.last().unwrap().seq;
    let rest = events_until(
        &flow.manager,
        &id,
        seq + 1,
        is_status(SessionStatus::Review),
    )
    .await;
    assert_eq!(
        labels(&rest),
        [
            "session_status:running",
            "agent_status:running",
            "item_started",
            "item_completed",
            "agent_status:idle",
            "session_status:review",
        ]
    );
}

#[tokio::test]
async fn an_interrupt_ends_the_turn_and_fails_its_node() {
    let mut flow = flow(&[("steps", TWO_STEPS)]).await;
    let id = flow.session("steps", "the task", &[]).await;
    let mut lead = Lead::default();
    lead.prompt(&mut flow.starts).await;
    lead.say("Half way there.").await;
    flow.until(&id, "item_completed").await;

    flow.manager.interrupt(&id).await.unwrap();
    let asked = within(lead.played().io.inputs.recv()).await;
    assert_eq!(asked, Some(RunnerInput::Interrupt));
    emit(&lead.played().io, RunnerOutput::TurnEnded).await;

    let events = flow.reviewed(&id).await;
    assert_eq!(
        finished(&events, "work"),
        (&Some("failed".to_owned()), &Some("interrupted".to_owned()))
    );
    let reason = "node `work` left through `failed`, which has no outgoing edge";
    assert_eq!(
        finish_reason(&events),
        (WorkflowStatus::Failed, Some(reason.to_owned()))
    );
    assert_eq!(
        labels(&events[events.len() - 4..]),
        [
            "agent_status:idle",
            "workflow_node:work:finished:failed",
            "workflow_finished:failed",
            "session_status:review",
        ]
    );
    // There is no turn to interrupt at rest, in a run or out of one.
    let none = flow.manager.interrupt(&id).await;
    assert!(matches!(none, Err(Error::Conflict(_))), "{none:?}");
}

#[tokio::test]
async fn an_agent_that_crashes_fails_its_node_and_not_the_session() {
    let recover = TWO_STEPS.replace(
        "  - { from: work.done, to: gate }",
        "  - { from: work.done, to: gate }\n  - { from: work.failed, to: more }",
    );
    let mut flow = flow(&[("steps", &recover)]).await;
    let id = flow.session("steps", "the task", &[]).await;
    let mut lead = Lead::default();
    lead.prompt(&mut flow.starts).await;
    let crashed = RunnerOutput::Exited(Exit::Error("the model is overloaded".to_owned()));
    emit(&lead.played().io, crashed).await;

    // `failed` is wired: the next node starts the agent again.
    let mut again = Lead::default();
    assert_eq!(again.prompt(&mut flow.starts).await, "Go on from: ");
    assert_eq!(status(&flow.manager, &id).await, SessionStatus::Running);
    again.answer("Went on.").await;
    let events = flow.reviewed(&id).await;
    assert_eq!(finish_reason(&events), (WorkflowStatus::Success, None));
    assert_eq!(
        finished(&events, "work"),
        (
            &Some("failed".to_owned()),
            &Some("the model is overloaded".to_owned())
        )
    );
    let said = labels(&events);
    assert!(said.contains(&"error:the model is overloaded".to_owned()));
    assert!(said.contains(&"agent_status:failed".to_owned()));
    assert!(
        !said.contains(&"session_status:failed".to_owned()),
        "{said:?}"
    );
    // One agent, started twice.
    assert_eq!(flow.manager.get(&id).await.unwrap().agents.len(), 1);
}

const SLOW_CHECK: &str = r#"
version: 1
name: A slow check
nodes:
  - { id: start, type: start }
  - { id: slow, type: check, command: "echo $$ > check.pid; echo begun; sleep 60; echo never" }
  - { id: end, type: end }
edges:
  - { from: start, to: slow }
  - { from: slow.pass, to: end }
"#;

fn alive(pid: &str) -> bool {
    let probe = std::process::Command::new("kill")
        .args(["-0", pid])
        .output();
    probe.unwrap().status.success()
}

#[tokio::test]
async fn stopping_during_a_check_kills_the_command_and_ends_the_run() {
    let flow = flow(&[("slow", SLOW_CHECK)]).await;
    let id = flow.session("slow", "wait", &[]).await;
    flow.until(&id, "item_started").await;
    let pid_file = flow.work_dir(&id).join("check.pid");
    let pid = within(async {
        loop {
            match std::fs::read_to_string(&pid_file) {
                Ok(pid) if pid.ends_with('\n') => return pid.trim().to_owned(),
                _ => tokio::time::sleep(Duration::from_millis(10)).await,
            }
        }
    })
    .await;
    assert!(alive(&pid));

    let started = std::time::Instant::now();
    within(flow.manager.stop(&id)).await.unwrap();
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(!alive(&pid), "the check is still running");

    let events = all_events(&flow.manager, &id).await;
    assert_eq!(
        labels(&events[6..]),
        [
            "workflow_node:slow:started",
            "item_started",
            "item_completed",
            "workflow_node:slow:finished",
            "workflow_finished:failed",
            "session_status:cancelled",
        ]
    );
    let ran = checks(&events);
    assert_eq!(ran[0].1, ItemStatus::Failed);
    assert_eq!(ran[0].2, "begun\n");
    assert_eq!(
        finished(&events, "slow"),
        (&None, &Some("stopped: the session was stopped".to_owned()))
    );
    assert_eq!(
        finish_reason(&events),
        (
            WorkflowStatus::Failed,
            Some("the session was stopped".to_owned())
        )
    );
    let run = flow.run(&id).await;
    assert_eq!(run.status, WorkflowStatus::Failed);
    assert_eq!(node(&run, "slow").state, NodeRunState::Finished);
    assert_eq!(node(&run, "slow").last_port, None);
    flow.assert_survives_a_reindex(&id).await;
}

#[tokio::test]
async fn stopping_at_a_gate_closes_the_question_and_the_run() {
    let mut flow = flow(&[("small", SMALL)]).await;
    let id = flow.session("small", "the task", &[]).await;
    let mut lead = Lead::default();
    lead.prompt(&mut flow.starts).await;
    lead.answer("Did it.").await;
    let (question, seq) = flow.asked(&id, 0).await;

    let stopping = flow.manager.stop(&id);
    let obeying = super::orchestration::exit_on_stop(lead.played());
    let (stopped, ()) = tokio::join!(stopping, obeying);
    stopped.unwrap();
    let events = events_until(&flow.manager, &id, seq, is_status(SessionStatus::Cancelled)).await;
    assert_eq!(
        labels(&events),
        [
            "question_answered::expired",
            "workflow_node:gate:finished",
            "workflow_finished:failed",
            "agent_status:stopped",
            "session_status:cancelled",
        ]
    );
    let late = flow
        .manager
        .answer_question(&id, &question.id, "approve")
        .await;
    assert!(matches!(late, Err(Error::Conflict(_))), "{late:?}");
}

const BUDGET: &str = r#"
version: 1
name: On a budget
budget_usd: 0.05
nodes:
  - { id: start, type: start }
  - { id: work, type: agent, prompt: "{{task}}" }
  - { id: review, type: agent, fresh: true, prompt: "Review it." }
  - { id: end, type: end }
edges:
  - { from: start, to: work }
  - { from: work.done, to: review }
  - { from: review.done, to: end }
"#;

fn usage(cost_usd: f64) -> RunnerOutput {
    RunnerOutput::Event(EventBody::Usage {
        cost_usd,
        tokens_in: 100,
        tokens_out: 10,
    })
}

#[tokio::test]
async fn a_run_over_its_budget_is_cut_in_the_middle_of_a_node() {
    let mut flow = flow(&[("budget", BUDGET)]).await;
    let id = flow.session("budget", "the task", &[]).await;
    let mut lead = Lead::default();
    lead.prompt(&mut flow.starts).await;
    // At the budget is not past it.
    emit(&lead.played().io, usage(0.05)).await;
    lead.say("Working.").await;
    flow.until(&id, "item_completed").await;
    assert_eq!(flow.run(&id).await.status, WorkflowStatus::Running);
    nothing_for(lead.played());

    emit(&lead.played().io, usage(0.0625)).await;
    // The turn is asked to end, and the run is over before it has.
    let asked = within(lead.played().io.inputs.recv()).await;
    assert_eq!(asked, Some(RunnerInput::Interrupt));
    let run = flow.run(&id).await;
    assert_eq!(run.status, WorkflowStatus::Failed);
    assert_eq!(
        run.reason.as_deref(),
        Some(
            "the budget of $0.05 was passed: the session and its child sessions have cost \
             $0.0625"
        )
    );
    assert_eq!(run.budget_usd, Some(0.05));
    assert_eq!(run.cost_usd, 0.0625);
    assert_eq!(node(&run, "work").state, NodeRunState::Finished);
    assert_eq!(
        node(&run, "work").outcome.as_deref(),
        Some("stopped: the budget was passed")
    );
    assert_eq!(node(&run, "review").state, NodeRunState::Idle);

    emit(&lead.played().io, RunnerOutput::TurnEnded).await;
    let events = flow.reviewed(&id).await;
    assert_eq!(
        labels(&events[events.len() - 5..]),
        [
            "usage",
            "workflow_node:work:finished",
            "workflow_finished:failed",
            "agent_status:idle",
            "session_status:review",
        ]
    );
    flow.assert_survives_a_reindex(&id).await;
}

#[tokio::test]
async fn what_child_sessions_cost_counts_towards_the_budget() {
    let mut flow = flow(&[("budget", BUDGET)]).await;
    let id = flow.session("budget", "the task", &[]).await;
    let mut lead = Lead::default();
    lead.prompt(&mut flow.starts).await;
    emit(&lead.played().io, usage(0.03)).await;
    lead.answer("Done.").await;
    let mut child = play(&mut flow.starts).await;
    emit(&child.io, usage(0.02)).await;
    say(&child, "said", "Reading.").await;
    events_until(&flow.manager, &child.id, 0, |event| {
        label(event) == "item_completed"
    })
    .await;
    assert_eq!(flow.run(&id).await.status, WorkflowStatus::Running);

    // The child's own total passes it: 0.03 + 0.03.
    emit(&child.io, usage(0.03)).await;
    // The child is stopped, and its work stays on its branch.
    super::orchestration::exit_on_stop(&mut child).await;
    let events = flow.reviewed(&id).await;
    let (status_, reason) = finish_reason(&events);
    assert_eq!(status_, WorkflowStatus::Failed);
    let reason = reason.unwrap();
    assert!(
        reason.contains("$0.05") && reason.contains("$0.06"),
        "{reason}"
    );
    assert_eq!(finished(&events, "review").0, &None);
    events_until(
        &flow.manager,
        &child.id,
        0,
        is_status(SessionStatus::Cancelled),
    )
    .await;
    // Nothing of it reaches the lead.
    nothing_for(lead.played());
    assert_eq!(status(&flow.manager, &id).await, SessionStatus::Review);
    let run = flow.run(&id).await;
    assert!((run.cost_usd - 0.06).abs() < 1e-9);
}

const CHECKS: &str = r#"
version: 1
name: Checks
nodes:
  - { id: start, type: start }
  - { id: slow, type: check, command: "echo begun; sleep 30; echo never", timeout_seconds: 1 }
  - { id: loud, type: check, command: "i=0; while [ $i -lt 9000 ]; do echo line-$i; i=$((i+1)); done; exit 4" }
  - { id: where, type: check, command: "basename $(pwd); cat marker.txt", repo: web }
  - { id: nowhere, type: check, command: "true", repo: gone }
  - id: tell
    type: agent
    prompt: "slow=[{{nodes.slow.exit_code}}] {{nodes.slow.output}}loud=[{{nodes.loud.exit_code}}] where=[{{nodes.where.exit_code}}] {{nodes.where.output}}nowhere={{nodes.nowhere.output}}"
  - { id: end, type: end }
edges:
  - { from: start, to: slow }
  - { from: slow.fail, to: loud }
  - { from: loud.fail, to: where }
  - { from: where.pass, to: nowhere }
  - { from: nowhere.fail, to: tell }
  - { from: tell.done, to: end }
"#;

#[tokio::test]
async fn a_check_has_a_time_limit_a_capped_output_and_a_repo_to_run_in() {
    let mut flow = flow(&[("checks", CHECKS)]).await;
    let api = flow.repo("api").await;
    let web = flow.repo("web").await;
    write_file(Path::new(&web.path), "marker.txt", "in web\n");
    git(Path::new(&web.path), &["add", "-A"]);
    git(Path::new(&web.path), &["commit", "-q", "-m", "marker"]);
    let id = flow.session("checks", "check", &[&api, &web]).await;
    let mut lead = Lead::default();
    let told = lead.prompt(&mut flow.starts).await;
    assert_eq!(
        told,
        "slow=[] begun\n[stopped: the check ran longer than 1 s]\nloud=[4] where=[0] web\nin \
         web\nnowhere=no repository named gone is attached to this session"
    );
    lead.answer("Told.").await;
    let events = flow.reviewed(&id).await;

    let ran = checks(&events);
    assert_eq!(ran.len(), 3, "a check without a place to run runs nothing");
    assert_eq!(ran[0].1, ItemStatus::Failed);
    assert_eq!(
        ran[0].2,
        "begun\n[stopped: the check ran longer than 1 s]\n"
    );
    // The last 16 KiB, which is where a failing command says why.
    assert_eq!(ran[1].1, ItemStatus::Failed);
    assert_eq!(ran[1].2.len(), 16 * 1024);
    assert!(ran[1].2.ends_with("line-8998\nline-8999\n"));
    assert_eq!(
        ran[2],
        (
            "basename $(pwd); cat marker.txt".to_owned(),
            ItemStatus::Completed,
            "web\nin web\n".to_owned()
        )
    );
    let (port, outcome) = finished(&events, "slow");
    assert_eq!(port.as_deref(), Some("fail"));
    let outcome = outcome.as_deref().unwrap();
    assert!(outcome.contains("timed out in api after 1 s"), "{outcome}");
    // Without `repo`, the primary repo's worktree.
    let input = events.iter().find_map(|event| match &event.body {
        EventBody::ItemStarted { item } => match &item.content {
            ItemContent::Tool { name, input, .. } if name == "check" => Some(input.clone()),
            _ => None,
        },
        _ => None,
    });
    assert_eq!(input.unwrap()["repo"], "api");
}

const DEFAULTS: &str = r#"
version: 1
name: Each repo's own checks
nodes:
  - { id: start, type: start }
  - { id: work, type: agent, prompt: "{{task}}" }
  - { id: tests, type: check }
  - { id: retry, type: loop, max: 3 }
  - { id: fix, type: agent, prompt: "exit {{nodes.tests.exit_code}}: {{nodes.tests.output}}" }
  - { id: end, type: end }
edges:
  - { from: start, to: work }
  - { from: work.done, to: tests }
  - { from: tests.fail, to: retry }
  - { from: retry.repeat, to: fix }
  - { from: fix.done, to: tests }
  - { from: tests.pass, to: end }
"#;

const DECLARED: &str = r#"
repos:
  api: { checks: ["echo api-one", "test -f ok.txt || { echo api-two is broken; exit 3; }"] }
  web: { checks: "echo web-one >> ../web-ran.txt; echo web-one" }
  docs: { checks: "echo docs-one" }
  plain: {}
"#;

#[tokio::test]
async fn without_a_command_a_check_runs_the_checks_of_each_changed_repo_in_order() {
    let mut flow = flow_on(false, DECLARED, &[("defaults", DEFAULTS)]).await;
    let api = flow.repo("api").await;
    let web = flow.repo("web").await;
    let docs = flow.repo("docs").await;
    let plain = flow.repo("plain").await;
    flow.manager.refresh_repos().await.unwrap();
    let mut wanted = request("change two repos", Some("puppet"));
    wanted.workflow = Some("defaults".to_owned());
    wanted.repos = vec![
        attach(&api, None),
        attach(&web, None),
        attach(&docs, None),
        attach(&plain, None),
    ];
    let id = flow
        .manager
        .create_session(wanted)
        .await
        .unwrap()
        .session
        .id;
    let mut lead = Lead::default();
    lead.prompt(&mut flow.starts).await;

    // Nothing changed yet: there is nothing to check, which passes.
    let detail = flow.manager.get(&id).await.unwrap();
    let tree = |name: &str| -> PathBuf {
        let repo = detail.repos.iter().find(|repo| repo.name == name).unwrap();
        PathBuf::from(&repo.path)
    };
    // `api`, `web` and `plain` change; `docs` does not.
    write_file(&tree("api"), "change.txt", "x\n");
    write_file(&tree("web"), "change.txt", "x\n");
    write_file(&tree("plain"), "change.txt", "x\n");
    lead.answer("Changed.").await;

    // The first repo's second check fails: nothing after it runs.
    assert_eq!(
        lead.prompt(&mut flow.starts).await,
        "exit 3: api-two is broken\n"
    );
    let ran_marker = tree("web").parent().unwrap().join("web-ran.txt");
    assert!(!ran_marker.exists());
    write_file(&tree("api"), "ok.txt", "x\n");
    lead.answer("Mended.").await;
    let events = flow.reviewed(&id).await;
    assert_eq!(finish_reason(&events), (WorkflowStatus::Success, None));

    let ran: Vec<(String, ItemStatus)> = checks(&events)
        .into_iter()
        .map(|(title, status, _)| (title, status))
        .collect();
    let api_two = "test -f ok.txt || { echo api-two is broken; exit 3; }".to_owned();
    let web_one = "echo web-one >> ../web-ran.txt; echo web-one".to_owned();
    assert_eq!(
        ran,
        [
            ("echo api-one".to_owned(), ItemStatus::Completed),
            (api_two.clone(), ItemStatus::Failed),
            ("echo api-one".to_owned(), ItemStatus::Completed),
            (api_two, ItemStatus::Completed),
            // `docs` did not change and `plain` declares none.
            (web_one, ItemStatus::Completed),
        ]
    );
    assert_eq!(std::fs::read_to_string(ran_marker).unwrap(), "web-one\n");
    let (port, outcome) = finished(&events, "tests");
    assert_eq!(port.as_deref(), Some("pass"));
    assert_eq!(outcome.as_deref(), Some("3 commands passed"));
    let first = events.iter().find_map(|event| match &event.body {
        EventBody::WorkflowNode {
            node_id,
            port: Some(port),
            outcome,
            ..
        } if node_id == "tests" && port == "fail" => outcome.clone(),
        _ => None,
    });
    let first = first.unwrap();
    assert!(first.contains("failed in api (exit status 3)"), "{first}");
}

#[tokio::test]
async fn a_session_with_no_changed_repo_passes_its_checks() {
    let mut flow = flow_on(false, DECLARED, &[("defaults", DEFAULTS)]).await;
    let api = flow.repo("api").await;
    flow.manager.refresh_repos().await.unwrap();
    let id = flow.session("defaults", "change nothing", &[&api]).await;
    let mut lead = Lead::default();
    lead.prompt(&mut flow.starts).await;
    lead.answer("Left it alone.").await;
    let events = flow.reviewed(&id).await;
    assert_eq!(finish_reason(&events), (WorkflowStatus::Success, None));
    assert!(checks(&events).is_empty());
    assert_eq!(
        finished(&events, "tests"),
        (
            &Some("pass".to_owned()),
            &Some("nothing to check".to_owned())
        )
    );
}

#[tokio::test]
async fn a_restart_ends_the_run_where_it_was_and_the_session_goes_on_plain() {
    let Flow {
        manager,
        mut starts,
        dir,
        clones: _clones,
        home: _home,
    } = flow(&[("steps", TWO_STEPS)]).await;
    let mut wanted = request("the task", Some("puppet"));
    wanted.workflow = Some("steps".to_owned());
    let id = manager.create_session(wanted).await.unwrap().session.id;
    let mut lead = Lead::default();
    lead.prompt(&mut starts).await;
    lead.say("Half way.").await;
    // The lead's own item: the run records an item of its own before it.
    events_until(
        &manager,
        &id,
        0,
        |event| matches!(&event.body, EventBody::ItemCompleted { item } if item.id == "said-1"),
    )
    .await;
    let before = manager.get(&id).await.unwrap().session.head_seq;
    // The process dies here.
    manager.store().close().await;
    drop((manager, lead, starts));

    let (manager, mut starts) = manager_in(&dir, false).await;
    let events = events_until(&manager, &id, before, is_status(SessionStatus::Failed)).await;
    assert_eq!(
        labels(&events),
        [
            "workflow_node:work:finished",
            "workflow_finished:failed",
            "agent_status:failed",
            "session_status:failed",
        ]
    );
    assert_eq!(
        events[1].body,
        EventBody::WorkflowFinished {
            status: WorkflowStatus::Failed,
            reason: Some("server restarted".to_owned()),
        }
    );
    let run = manager.workflow_run(&id).await.unwrap();
    assert_eq!(run.status, WorkflowStatus::Failed);
    assert_eq!(run.reason.as_deref(), Some("server restarted"));
    assert_eq!(node(&run, "work").state, NodeRunState::Finished);
    assert_eq!(
        node(&run, "work").outcome.as_deref(),
        Some("stopped: server restarted")
    );
    assert_eq!(node(&run, "gate").state, NodeRunState::Idle);
    let detail = manager.get(&id).await.unwrap();
    assert_eq!(detail.session.workflow.as_deref(), Some("steps"));

    // Continuing it is a plain turn: the run is not taken up again.
    let head = detail.session.head_seq;
    manager.send_message(&id, "go on").await.unwrap();
    let mut lead = Lead::default();
    assert_eq!(lead.prompt(&mut starts).await, "go on");
    lead.answer("Went on.").await;
    let after = events_until(&manager, &id, head, is_status(SessionStatus::Review)).await;
    assert!(workflow_labels(&after).is_empty(), "{:?}", labels(&after));
    assert_eq!(manager.workflow_run(&id).await.unwrap(), run);

    // A second restart finds nothing left to close.
    let head = manager.get(&id).await.unwrap().session.head_seq;
    manager.store().close().await;
    drop((manager, lead, starts));
    let (manager, _starts) = manager_in(&dir, false).await;
    assert_eq!(manager.get(&id).await.unwrap().session.head_seq, head);
}

#[tokio::test]
async fn quick_and_no_workflow_are_todays_session_without_a_run() {
    let flow = flow(&[("small", SMALL)]).await;
    for workflow in [None, Some("quick"), Some("  ")] {
        let mut wanted = request("just do it", None);
        wanted.workflow = workflow.map(str::to_owned);
        let created = flow.manager.create_session(wanted).await.unwrap();
        assert_eq!(created.session.workflow, None, "{workflow:?}");
        let id = created.session.id;
        let events = flow.reviewed(&id).await;
        assert!(workflow_labels(&events).is_empty(), "{workflow:?}");
        assert_eq!(
            labels(&events[..3]),
            [
                "session_status:queued",
                "user_message:just do it",
                "session_status:running"
            ]
        );
        assert_eq!(flow.manager.get(&id).await.unwrap().session.workflow, None);
        let none = flow.manager.workflow_run(&id).await;
        assert!(matches!(none, Err(Error::Missing(_))), "{none:?}");
        assert_eq!(flow.manager.store().workflow_run(&id).await.unwrap(), None);
        // Nothing about a workflow is in what a reindex reads either.
        let meta = flow
            .dir
            .path()
            .join("sessions")
            .join(&id)
            .join("session.json");
        let meta: serde_json::Value =
            serde_json::from_slice(&std::fs::read(meta).unwrap()).unwrap();
        assert!(meta["workflow"].is_null(), "{meta}");
    }
    let unknown = flow
        .manager
        .workflow_run("01ARZ3NDEKTSV4RRFFQ69G5FAV")
        .await;
    assert!(matches!(unknown, Err(Error::NotFound(_))), "{unknown:?}");
}

#[tokio::test]
async fn a_session_is_only_created_with_a_workflow_that_exists_and_validates() {
    let broken = "version: 1\nname: Broken\nnodes:\n  - { id: start, type: start }\nedges: []\n";
    let flow = flow(&[("small", SMALL), ("broken", broken)]).await;
    let before = flow.manager.list().await.unwrap().len();
    for workflow in ["missing", "broken", "Small", "small.yaml"] {
        let mut wanted = request("do it", Some("puppet"));
        wanted.workflow = Some(workflow.to_owned());
        let refused = flow.manager.create_session(wanted).await;
        assert!(
            matches!(refused, Err(Error::Invalid(_))),
            "{workflow}: {refused:?}"
        );
    }
    assert_eq!(flow.manager.list().await.unwrap().len(), before);

    // The run keeps the workflow it started with, whatever the file becomes.
    let mut flow = flow;
    let id = flow.session("small", "the task", &[]).await;
    let mut lead = Lead::default();
    lead.prompt(&mut flow.starts).await;
    write_file(&flow.home, ".calin/workflows/small.yaml", broken);
    lead.answer("Did it.").await;
    let (question, _) = flow.asked(&id, 0).await;
    assert_eq!(question.text, "Ship it?");
    let mut wanted = request("again", Some("puppet"));
    wanted.workflow = Some("small".to_owned());
    let refused = flow.manager.create_session(wanted).await;
    assert!(matches!(refused, Err(Error::Invalid(_))), "{refused:?}");
}

#[tokio::test]
async fn a_run_waits_for_a_slot_like_any_session_and_gives_it_back_at_a_gate() {
    let mut flow = flow(&[("steps", TWO_STEPS)]).await;
    let settings = Settings {
        home_repo: Some(flow.home.to_str().unwrap().to_owned()),
        max_parallel: Some(1),
        ..Settings::default()
    };
    flow.manager.update_settings(settings).await.unwrap();
    let a = flow.session("steps", "a", &[]).await;
    let mut lead_a = Lead::default();
    lead_a.prompt(&mut flow.starts).await;

    // No slot: the run has not started, and nothing of it is on record.
    let b = flow.session("steps", "b", &[]).await;
    assert_eq!(
        labels(&all_events(&flow.manager, &b).await),
        ["session_status:queued", "user_message:b"]
    );
    let none = flow.manager.workflow_run(&b).await;
    assert!(matches!(none, Err(Error::Missing(_))), "{none:?}");

    // `a` waits at its gate, which frees its slot for `b`'s run.
    lead_a.answer("A is done.").await;
    let (question, seq) = flow.asked(&a, 0).await;
    let mut lead_b = Lead::default();
    assert_eq!(lead_b.prompt(&mut flow.starts).await, "b");
    let started = flow.until(&b, "workflow_node:work:started").await;
    assert_eq!(
        labels(&started[2..]),
        [
            "session_status:running",
            "workflow_started:steps",
            "workflow_node:start:started",
            "workflow_node:start:finished:next",
            "workflow_node:work:started",
        ]
    );

    // Approved, but the next node is work and the slot is taken.
    flow.manager
        .answer_question(&a, &question.id, "approve")
        .await
        .unwrap();
    let queued = is_status(SessionStatus::Queued);
    let events = events_until(&flow.manager, &a, seq, queued).await;
    assert_eq!(
        labels(&events),
        [
            "question_answered:approve:user",
            "workflow_node:gate:finished:approve",
            "session_status:queued",
        ]
    );
    nothing_for(lead_a.played());
    let run = flow.run(&a).await;
    assert_eq!(run.status, WorkflowStatus::Running);
    assert_eq!(node(&run, "more").state, NodeRunState::Idle);

    // `b` reaches its own gate, and `a` goes on.
    lead_b.answer("B is done.").await;
    assert_eq!(
        lead_a.prompt(&mut flow.starts).await,
        "Go on from: A is done."
    );
    let seq = events.last().unwrap().seq;
    let went_on = events_until(&flow.manager, &a, seq, |event| {
        label(event) == "workflow_node:more:started"
    })
    .await;
    assert_eq!(
        labels(&went_on),
        ["session_status:running", "workflow_node:more:started"]
    );
    assert_eq!(status(&flow.manager, &b).await, SessionStatus::Waiting);
}
