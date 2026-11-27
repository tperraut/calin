use super::{doc, ok, spec_example};
use crate::{
    Action, CheckCommand, Context, EndStatus, EngineError, FinishReason, MAX_STEPS, NodeResult,
    NodeState, RunState, VERDICT_KEYWORD, Workflow, advance, extract_verdict, quick, start,
    verdict_instruction,
};

fn ctx() -> Context {
    Context {
        task: "Add a --verbose flag".into(),
        session_title: "Verbose flag".into(),
        session_branch: "calin/verbose-flag".into(),
    }
}

/// Applies `result`, asserting it is accepted, and returns the single action.
fn step(state: &mut RunState, result: NodeResult) -> Action {
    let (next, actions) = advance(state, result).unwrap_or_else(|e| panic!("rejected: {e}"));
    assert_eq!(actions.len(), 1);
    *state = next;
    actions.into_iter().next().unwrap()
}

fn begin(w: Workflow) -> (RunState, Action) {
    let (state, actions) = start(w, ctx());
    assert_eq!(actions.len(), 1);
    (state, actions.into_iter().next().unwrap())
}

fn finish_of(action: &Action) -> (EndStatus, FinishReason) {
    match action {
        Action::Finish(f) => (f.status, f.reason.clone()),
        other => panic!("expected finish, got {other:?}"),
    }
}

#[test]
fn quick_end_to_end() {
    let (mut s, a) = begin(quick());
    assert_eq!(
        a,
        Action::RunAgent {
            node: "agent".into(),
            prompt: "Add a --verbose flag".into(),
            fresh: false,
            verdicts: vec![],
            runner: None,
            model: None,
        }
    );
    let a = step(
        &mut s,
        NodeResult::new("agent", "done").with("summary", "Done."),
    );
    assert_eq!(
        finish_of(&a),
        (
            EndStatus::Success,
            FinishReason::EndNode { node: "end".into() }
        )
    );
    assert_eq!(s.finished().unwrap().status, EndStatus::Success);
    assert_eq!(
        advance(&s, NodeResult::new("agent", "done")),
        Err(EngineError::Finished)
    );

    // Failing the only turn leaves through the unwired `failed` port.
    let (mut s, _) = begin(quick());
    let a = step(&mut s, NodeResult::new("agent", "failed"));
    assert_eq!(
        finish_of(&a),
        (
            EndStatus::Failed,
            FinishReason::UnwiredPort {
                node: "agent".into(),
                port: "failed".into()
            }
        )
    );
}

#[test]
fn spec_example_success() {
    let (mut s, a) = begin(ok(spec_example()));
    assert_eq!(a.node(), Some("implement"));
    assert!(
        matches!(&a, Action::RunAgent { prompt, fresh: false, .. } if prompt == "Add a --verbose flag")
    );
    let a = step(
        &mut s,
        NodeResult::new("implement", "done").with("summary", "Added."),
    );
    assert_eq!(
        a,
        Action::RunCheck {
            node: "tests".into(),
            check: CheckCommand::RepoDefaults,
            timeout_seconds: 600,
        }
    );
    let a = step(
        &mut s,
        NodeResult::new("tests", "pass").with("exit_code", "0"),
    );
    let Action::RunAgent {
        node,
        prompt,
        fresh,
        verdicts,
        ..
    } = &a
    else {
        panic!("{a:?}")
    };
    assert_eq!(node, "review");
    assert!(*fresh);
    assert_eq!(verdicts, &["approve", "changes"]);
    assert_eq!(
        prompt,
        &format!(
            "Review the changes on this branch.\n\n{}",
            verdict_instruction(verdicts)
        )
    );
    let answer = format!("Looks right.\n{VERDICT_KEYWORD}: approve");
    let verdict = extract_verdict(&answer, verdicts);
    let a = step(
        &mut s,
        NodeResult::new("review", verdict.port())
            .with("summary", answer.as_str())
            .with("verdict", verdict.word()),
    );
    assert_eq!(
        a,
        Action::AskGate {
            node: "gate".into(),
            message: "Open pull requests?".into()
        }
    );
    let a = step(&mut s, NodeResult::new("gate", "approve"));
    assert_eq!(
        a,
        Action::Ship {
            node: "ship".into(),
            title: None,
            draft: true
        }
    );
    let a = step(
        &mut s,
        NodeResult::new("ship", "done").with("urls", "https://x/1"),
    );
    assert_eq!(
        finish_of(&a),
        (
            EndStatus::Success,
            FinishReason::EndNode { node: "ok".into() }
        )
    );

    let snap = s.snapshot();
    let taken: Vec<String> = snap
        .taken_edges
        .iter()
        .map(|e| format!("{}.{}>{}", e.from, e.port, e.to))
        .collect();
    assert_eq!(
        taken,
        [
            "start.next>implement",
            "implement.done>tests",
            "tests.pass>review",
            "review.approve>gate",
            "gate.approve>ship",
            "ship.done>ok"
        ]
    );
    let by_id = |id: &str| snap.nodes.iter().find(|n| n.id == id).unwrap().clone();
    assert_eq!(by_id("ok").state, NodeState::Finished { port: None });
    assert_eq!(by_id("fix").state, NodeState::Idle);
    assert_eq!(by_id("retry").iteration, Some(0));
    assert_eq!(
        by_id("review").state,
        NodeState::Finished {
            port: Some("approve".into())
        }
    );
    assert_eq!(snap.steps, 7);
}

#[test]
fn spec_example_retry_loop() {
    let (mut s, _) = begin(ok(spec_example()));
    step(&mut s, NodeResult::new("implement", "done"));
    let a = step(
        &mut s,
        NodeResult::new("tests", "fail")
            .with("output", "1 test failed: {{task}}")
            .with("exit_code", "101"),
    );
    // The loop is invisible: the next action is `fix`, rendered with the loop's outputs and
    // the check's output, which stays literal.
    assert_eq!(
        a,
        Action::RunAgent {
            node: "fix".into(),
            prompt: "The checks failed (attempt 1 of 3):\n1 test failed: {{task}}\n".into(),
            fresh: false,
            verdicts: vec![],
            runner: None,
            model: None,
        }
    );
    let snap = s.snapshot();
    let retry = snap.nodes.iter().find(|n| n.id == "retry").unwrap();
    assert_eq!(retry.iteration, Some(1));
    assert_eq!(retry.runs, 1);
    assert_eq!(
        retry.state,
        NodeState::Finished {
            port: Some("repeat".into())
        }
    );
    assert_eq!(
        snap.nodes.iter().find(|n| n.id == "fix").unwrap().state,
        NodeState::Active
    );

    let a = step(&mut s, NodeResult::new("fix", "done"));
    assert_eq!(a.node(), Some("tests"));
    let a = step(&mut s, NodeResult::new("tests", "pass"));
    assert_eq!(a.node(), Some("review"));
    // `changes` goes back through the same loop, whose counter did not reset.
    let a = step(
        &mut s,
        NodeResult::new("review", "changes").with("verdict", "changes"),
    );
    let Action::RunAgent { prompt, .. } = &a else {
        panic!("{a:?}")
    };
    // The newest `tests` run set no output, so it renders empty, not the older run's text.
    assert_eq!(prompt, "The checks failed (attempt 2 of 3):\n\n");
    let tests = s
        .snapshot()
        .nodes
        .into_iter()
        .find(|n| n.id == "tests")
        .unwrap();
    assert_eq!(tests.runs, 2);
}

#[test]
fn spec_example_loop_exhaustion() {
    let (mut s, _) = begin(ok(spec_example()));
    step(&mut s, NodeResult::new("implement", "done"));
    for attempt in 1..=3 {
        let a = step(
            &mut s,
            NodeResult::new("tests", "fail").with("output", "boom"),
        );
        let Action::RunAgent { node, prompt, .. } = &a else {
            panic!("{a:?}")
        };
        assert_eq!(node, "fix");
        assert!(prompt.starts_with(&format!("The checks failed (attempt {attempt} of 3)")));
        step(&mut s, NodeResult::new("fix", "done"));
    }
    let a = step(&mut s, NodeResult::new("tests", "fail"));
    assert_eq!(
        finish_of(&a),
        (
            EndStatus::Failed,
            FinishReason::EndNode {
                node: "gave-up".into()
            }
        )
    );
    let snap = s.snapshot();
    let retry = snap.nodes.iter().find(|n| n.id == "retry").unwrap();
    assert_eq!((retry.iteration, retry.runs), (Some(3), 4));
    assert_eq!(
        retry.state,
        NodeState::Finished {
            port: Some("exhausted".into())
        }
    );
    assert_eq!(s.outputs().get("retry", "iteration"), Some("3"));
}

#[test]
fn spec_example_rejected_gate() {
    let (mut s, _) = begin(ok(spec_example()));
    step(&mut s, NodeResult::new("implement", "done"));
    step(&mut s, NodeResult::new("tests", "pass"));
    step(&mut s, NodeResult::new("review", "approve"));
    let a = step(
        &mut s,
        NodeResult::new("gate", "reject").with("comment", "not yet"),
    );
    assert_eq!(
        finish_of(&a),
        (
            EndStatus::Failed,
            FinishReason::UnwiredPort {
                node: "gate".into(),
                port: "reject".into()
            }
        )
    );
    assert_eq!(s.outputs().get("gate", "comment"), Some("not yet"));
}

#[test]
fn unwired_ports() {
    let w = ok(&doc(
        "  - { id: start, type: start }
  - { id: q, type: ask, question: \"Ready for {{session.branch}}?\", options: [yes, no] }
  - { id: c, type: check, command: \"test {{nodes.q.answer}}\", repo: api, timeout_seconds: 5 }
  - { id: e, type: end }
",
        "  - { from: start, to: q }
  - { from: q, to: c }
  - { from: c.fail, to: e }
",
    ));
    let (mut s, a) = begin(w.clone());
    assert_eq!(
        a,
        Action::AskQuestion {
            node: "q".into(),
            question: "Ready for calin/verbose-flag?".into(),
            options: vec!["yes".into(), "no".into()],
        }
    );
    let a = step(
        &mut s,
        NodeResult::new("q", "answered").with("answer", "yes"),
    );
    assert_eq!(
        a,
        Action::RunCheck {
            node: "c".into(),
            check: CheckCommand::Command {
                command: "test yes".into(),
                repo: Some("api".into())
            },
            timeout_seconds: 5,
        }
    );
    // `pass` is unwired and not a failure port: success.
    let a = step(&mut s, NodeResult::new("c", "pass"));
    assert_eq!(
        finish_of(&a),
        (
            EndStatus::Success,
            FinishReason::UnwiredPort {
                node: "c".into(),
                port: "pass".into()
            }
        )
    );
    // An `end` reached through a failure port still finishes with the end's status.
    let (mut s, _) = begin(w);
    step(&mut s, NodeResult::new("q", "answered"));
    let a = step(&mut s, NodeResult::new("c", "fail"));
    assert_eq!(finish_of(&a).0, EndStatus::Success);
}

fn parallel_commit_ship() -> Workflow {
    ok(&doc(
        "  - { id: start, type: start }
  - id: par
    type: parallel
    branches:
      - { id: api, prompt: \"API part of {{task}}\", repos: [api] }
      - { id: web, prompt: \"Web part\" }
  - { id: save, type: commit, message: \"{{session.title}}: {{nodes.par.summary}}\" }
  - { id: ship, type: ship, title: \"{{session.title}}\", draft: false }
  - { id: ok, type: end }
  - { id: bad, type: end, status: failed }
",
        "  - { from: start, to: par }
  - { from: par.done, to: save }
  - { from: save.done, to: ship }
  - { from: save.nothing, to: ok }
  - { from: ship.done, to: ok }
  - { from: ship.failed, to: bad }
",
    ))
}

#[test]
fn parallel_commit_and_ship() {
    let (mut s, a) = begin(parallel_commit_ship());
    let Action::StartParallel { node, branches } = &a else {
        panic!("{a:?}")
    };
    assert_eq!(node, "par");
    assert_eq!(branches.len(), 2);
    assert_eq!(branches[0].prompt, "API part of Add a --verbose flag");
    assert_eq!(branches[0].repos.as_deref(), Some(&["api".to_owned()][..]));
    assert_eq!(branches[1].repos, None);
    let a = step(
        &mut s,
        NodeResult::new("par", "done").with("summary", "both done"),
    );
    assert_eq!(
        a,
        Action::Commit {
            node: "save".into(),
            message: "Verbose flag: both done".into()
        }
    );
    let a = step(&mut s, NodeResult::new("save", "done"));
    assert_eq!(
        a,
        Action::Ship {
            node: "ship".into(),
            title: Some("Verbose flag".into()),
            draft: false
        }
    );
    let a = step(&mut s, NodeResult::new("ship", "failed"));
    assert_eq!(
        finish_of(&a),
        (
            EndStatus::Failed,
            FinishReason::EndNode { node: "bad".into() }
        )
    );

    // `parallel.failed` is unwired: failed.
    let (mut s, _) = begin(parallel_commit_ship());
    let a = step(&mut s, NodeResult::new("par", "failed"));
    assert_eq!(finish_of(&a).0, EndStatus::Failed);

    // `commit.nothing` is wired to success; `commit.failed` is not.
    let (mut s, _) = begin(parallel_commit_ship());
    step(&mut s, NodeResult::new("par", "done"));
    let a = step(&mut s, NodeResult::new("save", "nothing"));
    assert_eq!(
        finish_of(&a),
        (
            EndStatus::Success,
            FinishReason::EndNode { node: "ok".into() }
        )
    );
    let (mut s, _) = begin(parallel_commit_ship());
    step(&mut s, NodeResult::new("par", "done"));
    let a = step(&mut s, NodeResult::new("save", "failed"));
    assert_eq!(finish_of(&a).0, EndStatus::Failed);
    // `ship.done`.
    let (mut s, _) = begin(parallel_commit_ship());
    step(&mut s, NodeResult::new("par", "done"));
    step(&mut s, NodeResult::new("save", "done"));
    let a = step(&mut s, NodeResult::new("ship", "done"));
    assert_eq!(finish_of(&a).0, EndStatus::Success);
}

#[test]
fn rejected_results_leave_state_unchanged() {
    let (mut s, _) = begin(ok(spec_example()));
    step(&mut s, NodeResult::new("implement", "done"));
    let before = s.clone();
    let cases = [
        (
            NodeResult::new("implement", "done"),
            EngineError::NotActive {
                node: "implement".into(),
                active: Some("tests".into()),
            },
        ),
        (
            NodeResult::new("retry", "repeat"),
            EngineError::NotActive {
                node: "retry".into(),
                active: Some("tests".into()),
            },
        ),
        (
            NodeResult::new("nope", "pass"),
            EngineError::NotActive {
                node: "nope".into(),
                active: Some("tests".into()),
            },
        ),
        (
            NodeResult::new("tests", "done"),
            EngineError::UnknownPort {
                node: "tests".into(),
                port: "done".into(),
                ports: vec!["pass".into(), "fail".into()],
            },
        ),
        (
            NodeResult::new("tests", "pass").with("summary", "x"),
            EngineError::UnknownOutput {
                node: "tests".into(),
                output: "summary".into(),
                outputs: vec!["output".into(), "exit_code".into()],
            },
        ),
    ];
    for (result, error) in cases {
        assert_eq!(advance(&s, result), Err(error));
        assert_eq!(s, before);
    }
    let msg = advance(&s, NodeResult::new("tests", "done"))
        .unwrap_err()
        .to_string();
    assert_eq!(
        msg,
        "node `tests` has no port `done`; its ports are pass, fail"
    );
    // A verdict node accepts only its verdict ports and `failed`.
    step(&mut s, NodeResult::new("tests", "pass"));
    assert!(matches!(
        advance(&s, NodeResult::new("review", "done")),
        Err(EngineError::UnknownPort { .. })
    ));
    // Commit declares no outputs at all.
    let (mut s, _) = begin(parallel_commit_ship());
    step(&mut s, NodeResult::new("par", "done"));
    let err = advance(&s, NodeResult::new("save", "done").with("summary", "x")).unwrap_err();
    assert_eq!(
        err.to_string(),
        "node `save` has no output `summary`; its outputs are none"
    );
}

/// A fixed script of results through the spec example, touching loops and verdicts.
fn script() -> Vec<NodeResult> {
    vec![
        NodeResult::new("implement", "done").with("summary", "first"),
        NodeResult::new("tests", "fail").with("output", "e1"),
        NodeResult::new("fix", "done"),
        NodeResult::new("tests", "pass"),
        NodeResult::new("review", "changes").with("verdict", "changes"),
        NodeResult::new("fix", "done").with("summary", "second"),
        NodeResult::new("tests", "pass"),
        NodeResult::new("review", "approve"),
        NodeResult::new("gate", "approve").with("comment", "go"),
        NodeResult::new("ship", "done"),
    ]
}

#[test]
fn json_round_trip_mid_run() {
    let (mut s, _) = begin(ok(spec_example()));
    let results = script();
    for r in &results[..5] {
        step(&mut s, r.clone());
    }
    let json = serde_json::to_string(&s).unwrap();
    let mut restored: RunState = serde_json::from_str(&json).unwrap();
    assert_eq!(restored, s);
    assert_eq!(restored.pending_action(), s.pending_action());
    assert_eq!(restored.pending_action().unwrap().node(), Some("fix"));
    for r in &results[5..] {
        let a = step(&mut s, r.clone());
        let b = step(&mut restored, r.clone());
        assert_eq!(a, b);
        assert_eq!(restored, s);
    }
    assert_eq!(restored.finished().unwrap().status, EndStatus::Success);
    // Actions and snapshots serialize too.
    let snap = serde_json::to_value(s.snapshot()).unwrap();
    assert_eq!(snap["nodes"][3]["iteration"], 2);
    assert_eq!(snap["nodes"][8]["state"]["state"], "finished");
}

#[test]
fn replay_is_deterministic() {
    let run = || {
        let (mut s, first) = begin(ok(spec_example()));
        let mut actions = vec![first];
        for r in script() {
            actions.push(step(&mut s, r));
        }
        (s, actions)
    };
    let (s1, a1) = run();
    let (s2, a2) = run();
    assert_eq!(a1, a2);
    assert_eq!(s1, s2);
    assert_eq!(
        serde_json::to_string(&s1).unwrap(),
        serde_json::to_string(&s2).unwrap()
    );
}

#[test]
fn step_ceiling() {
    // Valid: one loop of 20 around a chain of 50 agents is 21 passes of 51 activations.
    let mut nodes = String::from(
        "  - { id: start, type: start }\n  - { id: l, type: loop, max: 20 }\n  - { id: e, type: end }\n",
    );
    let mut edges = String::from(
        "  - { from: start, to: l }\n  - { from: l.repeat, to: a1 }\n  - { from: l.exhausted, to: e }\n",
    );
    for i in 1..=50 {
        nodes.push_str(&format!("  - {{ id: a{i}, type: agent, prompt: p }}\n"));
        let to = if i == 50 {
            "l".to_owned()
        } else {
            format!("a{}", i + 1)
        };
        edges.push_str(&format!("  - {{ from: a{i}.done, to: {to} }}\n"));
    }
    let (mut s, mut a) = begin(ok(&doc(&nodes, &edges)));
    let mut answered = 0;
    while let Some(node) = a.node().map(str::to_owned) {
        a = step(&mut s, NodeResult::new(node, "done"));
        answered += 1;
    }
    assert_eq!(
        finish_of(&a),
        (
            EndStatus::Failed,
            FinishReason::StepLimit { limit: MAX_STEPS }
        )
    );
    assert_eq!(s.steps(), MAX_STEPS);
    assert!(answered < MAX_STEPS);
    assert!(
        FinishReason::StepLimit { limit: MAX_STEPS }
            .to_string()
            .contains("1000 node activations")
    );
}
