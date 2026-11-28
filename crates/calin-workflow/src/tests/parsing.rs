use super::{doc, ok, problems, spec_example};
use crate::{
    EndStatus, NodeKind, NodeType, Problems, Subject, Workflow, is_failure_port, parse, quick,
};

const BASE_NODES: &str = "  - { id: start, type: start }
  - { id: work, type: agent, prompt: \"{{task}}\" }
  - { id: ok, type: end }
";
const BASE_EDGES: &str = "  - { from: start, to: work }
  - { from: work.done, to: ok }
";

/// The base workflow plus one node `id`, reached from `work.failed`.
fn with_node(node: &str, id: &str) -> String {
    doc(
        &format!("{BASE_NODES}  - {node}\n"),
        &format!("{BASE_EDGES}  - {{ from: work.failed, to: {id} }}\n"),
    )
}

fn assert_mentions(p: &Problems, fragments: &[&str], yaml: &str) {
    assert!(
        p.iter()
            .any(|q| fragments.iter().all(|f| q.message.contains(f))),
        "no problem mentions all of {fragments:?}; got:\n{p}\n---\n{yaml}"
    );
}

#[test]
fn spec_example_parses() {
    let w = ok(spec_example());
    assert_eq!(w.id(), "test");
    assert_eq!(w.name(), "Implement, test, review");
    assert_eq!(w.description(), Some("Optional one-liner."));
    assert_eq!(w.budget_usd(), Some(5.0));
    assert_eq!(w.nodes().len(), 10);
    assert_eq!(w.edges().len(), 11);
    // `from: start` resolved to its only port.
    assert_eq!(w.edges()[0].port, "next");
    let review = w.node("review").unwrap();
    let ports: Vec<_> = review.ports().into_iter().map(|p| p.name).collect();
    assert_eq!(ports, ["approve", "changes", "failed"]);
    let NodeKind::Check(check) = &w.node("tests").unwrap().kind else {
        panic!()
    };
    assert_eq!(check.timeout_seconds, 600);
    assert!(check.command.is_none());
    let NodeKind::Ship { title, draft } = &w.node("ship").unwrap().kind else {
        panic!()
    };
    assert!(title.is_none() && *draft);
    assert!(matches!(
        w.node("ok").unwrap().kind,
        NodeKind::End {
            status: EndStatus::Success
        }
    ));
}

#[test]
fn quick_validates() {
    let w = quick();
    let json = serde_json::to_string(&w).unwrap();
    // Deserializing runs the full validation again.
    let back: Workflow = serde_json::from_str(&json).unwrap();
    assert_eq!(back, w);
    assert_eq!(w.id(), "quick");
    let NodeKind::Agent(agent) = &w.node("agent").unwrap().kind else {
        panic!()
    };
    assert_eq!(agent.prompt.source(), "{{task}}");
    // The same graph written as a file validates too.
    let yaml = doc(
        "  - { id: start, type: start }\n  - { id: agent, type: agent, prompt: \"{{task}}\" }\n  - { id: end, type: end }\n",
        "  - { from: start, to: agent }\n  - { from: agent.done, to: end }\n",
    );
    assert_eq!(ok(&yaml).graph(), w.graph());
}

#[test]
fn json_form_is_validated_on_the_way_in() {
    let w = ok(spec_example());
    let mut json: serde_json::Value = serde_json::to_value(&w).unwrap();
    json["edges"][0]["to"] = "nowhere".into();
    let err = serde_json::from_value::<Workflow>(json).unwrap_err();
    assert!(err.to_string().contains("nowhere"), "{err}");
}

#[test]
fn graph_for_display() {
    let w = ok(spec_example());
    let g = w.graph();
    let ids: Vec<_> = g.nodes.iter().map(|n| n.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "start",
            "implement",
            "tests",
            "retry",
            "fix",
            "review",
            "gate",
            "ship",
            "ok",
            "gave-up"
        ]
    );
    assert_eq!(g.edges, w.edges());
    let fix = &g.nodes[4];
    assert_eq!(fix.node_type, NodeType::Agent);
    assert_eq!(
        fix.label,
        "The checks failed (attempt {{nodes.retry.iteration}} of {{n…"
    );
    assert_eq!(g.nodes[3].label, "Up to 3 times");
    assert_eq!(g.nodes[2].label, "Repository checks");
    let retry_ports: Vec<_> = g.nodes[3]
        .ports
        .iter()
        .map(|p| (p.name.as_str(), p.failure))
        .collect();
    assert_eq!(retry_ports, [("repeat", false), ("exhausted", true)]);
    assert!(g.nodes[8].ports.is_empty());
    let json = serde_json::to_value(&g).unwrap();
    assert_eq!(json["nodes"][1]["type"], "agent");
    assert_eq!(json["edges"][1]["port"], "done");
}

#[test]
fn labels_and_templates_of_every_type() {
    let w = ok(&doc(
        "  - { id: start, type: start }
  - { id: a, type: agent, prompt: \"\\n  Fix it  \\nthen test\" }
  - { id: c, type: check, command: make test }
  - { id: c2, type: check }
  - { id: l, type: loop, max: 2 }
  - { id: g, type: gate, message: Approve? }
  - { id: q, type: ask, question: Which one? }
  - { id: p, type: parallel, branches: [{ id: x, prompt: px }, { id: y, prompt: py }] }
  - { id: m, type: commit, message: Save work }
  - { id: s, type: ship, title: Add flag }
  - { id: s2, type: ship }
  - { id: ok, type: end }
  - { id: bad, type: end, status: failed }
",
        "  - { from: start, to: a }
  - { from: a.done, to: c }
  - { from: c.pass, to: c2 }
  - { from: c2.pass, to: l }
  - { from: l.repeat, to: g }
  - { from: l.exhausted, to: bad }
  - { from: g.approve, to: q }
  - { from: q, to: p }
  - { from: p.done, to: m }
  - { from: m.done, to: s }
  - { from: s.done, to: s2 }
  - { from: s2.done, to: ok }
",
    ));
    let labels: Vec<(String, String)> = w
        .graph()
        .nodes
        .into_iter()
        .map(|n| (n.id, n.label))
        .collect();
    let expected = [
        ("start", "Start"),
        ("a", "Fix it"),
        ("c", "make test"),
        ("c2", "Repository checks"),
        ("l", "Up to 2 times"),
        ("g", "Approve?"),
        ("q", "Which one?"),
        ("p", "2 branches"),
        ("m", "Save work"),
        ("s", "Add flag"),
        ("s2", "Open pull requests"),
        ("ok", "End: success"),
        ("bad", "End: failed"),
    ]
    .map(|(id, label)| (id.to_owned(), label.to_owned()));
    assert_eq!(labels, expected);

    let keys = |id: &str| -> Vec<String> {
        w.node(id)
            .unwrap()
            .templates()
            .into_iter()
            .map(|(key, t)| format!("{key}={}", t.source()))
            .collect()
    };
    assert_eq!(keys("a"), ["prompt=\n  Fix it  \nthen test"]);
    assert_eq!(keys("c"), ["command=make test"]);
    assert_eq!(keys("g"), ["message=Approve?"]);
    assert_eq!(keys("q"), ["question=Which one?"]);
    assert_eq!(
        keys("p"),
        ["branches[x].prompt=px", "branches[y].prompt=py"]
    );
    assert_eq!(keys("m"), ["message=Save work"]);
    assert_eq!(keys("s"), ["title=Add flag"]);
    for id in ["start", "c2", "l", "s2", "ok"] {
        assert!(keys(id).is_empty(), "{id}");
    }
}

#[test]
fn ports_and_outputs_per_type() {
    for t in NodeType::ALL {
        assert_eq!(NodeType::from_name(t.as_str()), Some(t));
    }
    assert_eq!(
        NodeType::Commit.default_ports(),
        ["done", "nothing", "failed"]
    );
    assert_eq!(NodeType::Check.outputs(), ["output", "exit_code"]);
    assert_eq!(NodeType::End.default_ports(), [] as [&str; 0]);
    for p in ["failed", "fail", "exhausted", "reject"] {
        assert!(is_failure_port(p));
    }
    for p in [
        "done", "pass", "repeat", "approve", "nothing", "answered", "next",
    ] {
        assert!(!is_failure_port(p));
    }
}

#[test]
fn defaults_and_optional_fields() {
    let w = ok(&doc(
        "  - { id: start, type: start }
  - { id: a, type: agent, prompt: p, fresh: true, runner: claude, model: opus, verdicts: [yes, no] }
  - { id: c, type: check, command: \"make {{session.branch}}\", repo: api, timeout_seconds: 30 }
  - { id: q, type: ask, question: Which?, options: [x, y] }
  - { id: p, type: parallel, branches: [{ id: one, prompt: a, repos: [api] }, { id: two, prompt: b }] }
  - { id: m, type: commit, message: wip }
  - { id: s, type: ship, title: \"{{session.title}}\", draft: false }
  - { id: f, type: end, status: failed }
",
        "  - { from: start, to: a }
  - { from: a.yes, to: c }
  - { from: a.no, to: q }
  - { from: c.pass, to: p }
  - { from: q, to: m }
  - { from: p.done, to: s }
  - { from: m.done, to: f }
",
    ));
    let NodeKind::Agent(a) = &w.node("a").unwrap().kind else {
        panic!()
    };
    // YAML 1.2: `yes` and `no` are words, not booleans.
    assert_eq!(a.verdicts, ["yes", "no"]);
    assert_eq!(a.runner.as_deref(), Some("claude"));
    assert!(matches!(
        w.node("f").unwrap().kind,
        NodeKind::End {
            status: EndStatus::Failed
        }
    ));
    assert_eq!(w.edges()[4].port, "answered");
}

/// One file per rule; each must report a problem mentioning every fragment.
#[test]
fn invalid_files() {
    let cases: Vec<(String, Vec<&str>)> = vec![
        // Section 2: version and keys.
        (
            "name: x\nnodes: []\nedges: []\n".into(),
            vec!["missing required key `version`"],
        ),
        (
            doc(BASE_NODES, BASE_EDGES).replace("version: 1", "version: 2"),
            vec!["`version` must be 1, found 2"],
        ),
        (
            doc(BASE_NODES, BASE_EDGES).replace("version: 1", "version: \"1\""),
            vec!["`version`", "integer", "string"],
        ),
        (
            format!("{}budget: 3\n", doc(BASE_NODES, BASE_EDGES)),
            vec!["unknown key `budget`", "the document"],
        ),
        (
            doc(BASE_NODES, BASE_EDGES).replace("prompt:", "promt:"),
            vec!["node `work`", "unknown key `promt` for type `agent`"],
        ),
        (
            with_node("{ id: c, type: check, fresh: true }", "c"),
            vec!["node `c`", "unknown key `fresh`", "`check`"],
        ),
        (
            doc(
                BASE_NODES,
                &BASE_EDGES.replace("to: ok }", "to: ok, label: x }"),
            ),
            vec!["edge `work.done -> ok`", "unknown key `label`"],
        ),
        (
            "version: 1\nnodes: []\nedges: []\n".into(),
            vec!["missing required key `name`"],
        ),
        (
            doc(BASE_NODES, BASE_EDGES).replace("name: Test", "name: \"  \""),
            vec!["`name` must not be empty"],
        ),
        (
            "version: 1\nname: x\nedges: []\n".into(),
            vec!["missing required key `nodes`"],
        ),
        (
            "version: 1\nname: x\nnodes: []\n".into(),
            vec!["missing required key `edges`"],
        ),
        (
            "version: 1\nname: x\nnodes: {}\nedges: []\n".into(),
            vec!["`nodes` must be a list"],
        ),
        (
            format!("{}budget_usd: -1\n", doc(BASE_NODES, BASE_EDGES)),
            vec!["`budget_usd` must be a positive number"],
        ),
        (
            format!("{}budget_usd: 0\n", doc(BASE_NODES, BASE_EDGES)),
            vec!["`budget_usd` must be a positive number, found 0"],
        ),
        (
            format!("{}budget_usd: five\n", doc(BASE_NODES, BASE_EDGES)),
            vec!["`budget_usd` must be a number"],
        ),
        // Node ids.
        (
            with_node("{ id: Fix, type: commit, message: m }", "Fix"),
            vec!["node `Fix`", "id must match"],
        ),
        (
            with_node("{ id: 9lives, type: commit, message: m }", "9lives"),
            vec!["node `9lives`", "id must match"],
        ),
        (
            with_node(
                &format!("{{ id: {}, type: commit, message: m }}", "a".repeat(41)),
                &"a".repeat(41),
            ),
            vec!["at most 40 characters"],
        ),
        (
            with_node("{ id: work, type: commit, message: m }", "work"),
            vec!["node `work`", "already used"],
        ),
        (
            with_node("{ type: commit, message: m }", "x"),
            vec!["node #4", "missing required key `id`"],
        ),
        (
            with_node("{ id: c, type: agnet, prompt: p }", "c"),
            vec!["node `c`", "unknown type `agnet`"],
        ),
        (
            with_node("{ id: c, prompt: p }", "c"),
            vec!["node `c`", "missing required key `type`"],
        ),
        (
            with_node("just a string", "x"),
            vec!["node #4 must be a mapping"],
        ),
        // Edges.
        (
            doc(
                BASE_NODES,
                &format!("{BASE_EDGES}  - {{ from: nope.done, to: ok }}\n"),
            ),
            vec!["edge `nope.done -> ok`", "unknown node `nope`"],
        ),
        (
            doc(
                BASE_NODES,
                &format!("{BASE_EDGES}  - {{ from: work.finished, to: ok }}\n"),
            ),
            vec![
                "edge `work.finished -> ok`",
                "no port `finished`",
                "done, failed",
            ],
        ),
        (
            doc(
                BASE_NODES,
                &format!("{BASE_EDGES}  - {{ from: work, to: ok }}\n"),
            ),
            vec!["edge `work -> ok`", "more than one port"],
        ),
        (
            doc(
                BASE_NODES,
                &format!("{BASE_EDGES}  - {{ from: ok, to: work }}\n"),
            ),
            vec!["edge `ok -> work`", "has more than one port"],
        ),
        (
            doc(
                BASE_NODES,
                &format!("{BASE_EDGES}  - {{ from: ok.done, to: work }}\n"),
            ),
            vec!["edge `ok.done -> work`", "no port `done`", "no ports"],
        ),
        (
            doc(
                BASE_NODES,
                &format!("{BASE_EDGES}  - {{ from: work.failed, to: nowhere }}\n"),
            ),
            vec!["edge `work.failed -> nowhere`", "unknown node `nowhere`"],
        ),
        (
            doc(
                BASE_NODES,
                &format!("{BASE_EDGES}  - {{ from: work.failed, to: ok.done }}\n"),
            ),
            vec!["`to` is a node id"],
        ),
        (
            doc(
                BASE_NODES,
                &format!("{BASE_EDGES}  - {{ from: work.done, to: work }}\n"),
            ),
            vec![
                "edge `work.done -> work`",
                "port `work.done` already has an outgoing edge",
            ],
        ),
        (
            doc(
                BASE_NODES,
                &format!("{BASE_EDGES}  - {{ from: work.failed, to: start }}\n"),
            ),
            vec!["edge `work.failed -> start`", "`start`"],
        ),
        (
            doc(
                BASE_NODES,
                &format!("{BASE_EDGES}  - {{ from: work.failed }}\n"),
            ),
            vec!["edge #3", "missing required key `to`"],
        ),
        // Graph.
        (
            with_node("{ id: start2, type: start }", "start2"),
            vec!["node `start2`", "second `start` node"],
        ),
        (
            doc(
                "  - { id: work, type: agent, prompt: p }\n  - { id: ok, type: end }\n",
                "  - { from: work.done, to: ok }\n",
            ),
            vec!["no `start` node"],
        ),
        (
            doc(
                "  - { id: start, type: start }\n  - { id: work, type: agent, prompt: p }\n",
                "  - { from: start, to: work }\n",
            ),
            vec!["no `end` node"],
        ),
        (
            doc(
                &format!("{BASE_NODES}  - {{ id: lost, type: commit, message: m }}\n"),
                BASE_EDGES,
            ),
            vec!["node `lost` is not reachable from `start`"],
        ),
        (
            doc(
                BASE_NODES,
                &format!("{BASE_EDGES}  - {{ from: work.failed, to: work }}\n"),
            ),
            vec!["`work`", "cycle", "no `loop` node"],
        ),
        // Section 3: fields.
        (
            with_node("{ id: e, type: end, status: done }", "e"),
            vec!["node `e`", "`status` must be `success` or `failed`"],
        ),
        (
            with_node("{ id: a, type: agent }", "a"),
            vec!["node `a`", "missing required key `prompt`"],
        ),
        (
            with_node("{ id: a, type: agent, prompt: \" \" }", "a"),
            vec!["node `a`", "`prompt` must not be empty"],
        ),
        (
            with_node("{ id: a, type: agent, prompt: 42 }", "a"),
            vec!["node `a`", "`prompt` must be a string"],
        ),
        (
            with_node("{ id: a, type: agent, prompt: p, fresh: yes }", "a"),
            vec!["node `a`", "`fresh` must be `true` or `false`"],
        ),
        (
            with_node("{ id: a, type: agent, prompt: p, verdicts: [] }", "a"),
            vec!["node `a`", "`verdicts` must not be an empty list"],
        ),
        (
            with_node(
                "{ id: a, type: agent, prompt: p, verdicts: [Approve] }",
                "a",
            ),
            vec!["node `a`", "verdict `Approve` must match"],
        ),
        (
            with_node(
                "{ id: a, type: agent, prompt: p, verdicts: [ok, failed] }",
                "a",
            ),
            vec!["node `a`", "verdict `failed` collides"],
        ),
        (
            with_node("{ id: a, type: agent, prompt: p, verdicts: [ok, ok] }", "a"),
            vec!["node `a`", "verdict `ok` is listed twice"],
        ),
        (
            with_node("{ id: a, type: agent, prompt: p, verdicts: [ok, 3] }", "a"),
            vec!["node `a`", "every entry of `verdicts` must be a string"],
        ),
        (
            with_node("{ id: a, type: agent, prompt: p, runner: claude }", "a"),
            vec!["node `a`", "`runner` applies only with `fresh: true`"],
        ),
        (
            with_node(
                "{ id: a, type: agent, prompt: p, fresh: false, model: opus }",
                "a",
            ),
            vec!["node `a`", "`model` applies only with `fresh: true`"],
        ),
        (
            with_node("{ id: c, type: check, repo: api }", "c"),
            vec!["node `c`", "`repo` applies only with `command`"],
        ),
        (
            with_node(
                "{ id: c, type: check, command: make, timeout_seconds: 0 }",
                "c",
            ),
            vec![
                "node `c`",
                "`timeout_seconds` must be from 1 to 86400, found 0",
            ],
        ),
        (
            with_node(
                "{ id: c, type: check, command: make, timeout_seconds: \"10\" }",
                "c",
            ),
            vec!["node `c`", "`timeout_seconds` must be an integer"],
        ),
        (
            with_node("{ id: l, type: loop }", "l"),
            vec!["node `l`", "missing required key `max`"],
        ),
        (
            with_node("{ id: l, type: loop, max: 0 }", "l"),
            vec!["node `l`", "`max` must be from 1 to 20, found 0"],
        ),
        (
            with_node("{ id: l, type: loop, max: 21 }", "l"),
            vec!["node `l`", "`max` must be from 1 to 20, found 21"],
        ),
        (
            with_node("{ id: l, type: loop, max: 2.5 }", "l"),
            vec!["node `l`", "`max` must be an integer from 1 to 20"],
        ),
        (
            with_node("{ id: g, type: gate }", "g"),
            vec!["node `g`", "missing required key `message`"],
        ),
        (
            with_node("{ id: q, type: ask }", "q"),
            vec!["node `q`", "missing required key `question`"],
        ),
        (
            with_node("{ id: q, type: ask, question: Q, options: [] }", "q"),
            vec!["node `q`", "`options` must not be an empty list"],
        ),
        (
            with_node("{ id: q, type: ask, question: Q, options: [a, a] }", "q"),
            vec!["node `q`", "option `a` is listed twice"],
        ),
        (
            with_node("{ id: m, type: commit }", "m"),
            vec!["node `m`", "missing required key `message`"],
        ),
        (
            with_node(
                "{ id: p, type: parallel, branches: [{ id: a, prompt: x }] }",
                "p",
            ),
            vec!["node `p`", "`branches` must have 2 to 4 entries, found 1"],
        ),
        (
            with_node(
                "{ id: p, type: parallel, branches: [{ id: a, prompt: x }, { id: b, prompt: x }, { id: c, prompt: x }, { id: d, prompt: x }, { id: e, prompt: x }] }",
                "p",
            ),
            vec!["node `p`", "found 5"],
        ),
        (
            with_node("{ id: p, type: parallel }", "p"),
            vec!["node `p`", "missing required key `branches`"],
        ),
        (
            with_node(
                "{ id: p, type: parallel, branches: [{ id: a, prompt: x }, { id: a, prompt: y }] }",
                "p",
            ),
            vec!["node `p`", "branch id `a` is used twice"],
        ),
        (
            with_node(
                "{ id: p, type: parallel, branches: [{ id: A, prompt: x }, { id: b, prompt: y }] }",
                "p",
            ),
            vec!["node `p`", "branch id `A` must match"],
        ),
        (
            with_node(
                "{ id: p, type: parallel, branches: [{ id: a }, { id: b, prompt: y }] }",
                "p",
            ),
            vec!["node `p`: branch `a`", "missing required key `prompt`"],
        ),
        (
            with_node(
                "{ id: p, type: parallel, branches: [{ id: a, prompt: x, repo: api }, { id: b, prompt: y }] }",
                "p",
            ),
            vec!["node `p`: branch `a`", "unknown key `repo`"],
        ),
        (
            with_node(
                "{ id: p, type: parallel, branches: [{ id: a, prompt: x, repos: [] }, { id: b, prompt: y }] }",
                "p",
            ),
            vec!["node `p`: branch `a`", "`repos` must not be an empty list"],
        ),
        (
            with_node("{ id: s, type: ship, draft: \"no\" }", "s"),
            vec!["node `s`", "`draft` must be `true` or `false`"],
        ),
        (
            with_node("{ id: s, type: ship, title: \"\" }", "s"),
            vec!["node `s`", "`title` must not be empty"],
        ),
        // Section 4: templates.
        (
            with_node(
                "{ id: g, type: gate, message: \"{{nodes.nope.summary}}\" }",
                "g",
            ),
            vec!["node `g`", "`message`", "no node `nope`"],
        ),
        (
            with_node(
                "{ id: g, type: gate, message: \"{{nodes.work.output}}\" }",
                "g",
            ),
            vec![
                "node `g`",
                "`work` (type `agent`)",
                "no output `output`",
                "summary, verdict",
            ],
        ),
        (
            with_node(
                "{ id: g, type: gate, message: \"{{nodes.ok.summary}}\" }",
                "g",
            ),
            vec!["node `g`", "no output `summary`", "no outputs"],
        ),
        (
            with_node("{ id: g, type: gate, message: \"hello {{ task\" }", "g"),
            vec!["node `g`", "`message`", "never closed"],
        ),
        (
            with_node("{ id: g, type: gate, message: \"{{tasks}}\" }", "g"),
            vec!["node `g`", "`message`", "not a placeholder"],
        ),
        (
            with_node(
                "{ id: c, type: check, command: \"{{ nodes.work.summary }} {{x}}\" }",
                "c",
            ),
            vec!["node `c`", "`command`", "`{{x}}`"],
        ),
        (
            with_node(
                "{ id: p, type: parallel, branches: [{ id: a, prompt: \"{{nodes.zz.summary}}\" }, { id: b, prompt: y }] }",
                "p",
            ),
            vec!["node `p`", "`branches[a].prompt`", "no node `zz`"],
        ),
        (
            with_node("{ id: s, type: ship, title: \"{{nodes.s.title}}\" }", "s"),
            vec!["node `s`", "`title`", "no output `title`", "urls"],
        ),
        // The document itself.
        ("".into(), vec!["the file is empty"]),
        ("- a\n- b\n".into(), vec!["the document must be a mapping"]),
        ("version: 1\nname: [unclosed\n".into(), vec!["invalid YAML"]),
        (
            "version: 1\nversion: 1\n".into(),
            vec!["invalid YAML", "duplicate"],
        ),
    ];
    for (yaml, fragments) in &cases {
        let p = problems(yaml);
        assert_mentions(&p, fragments, yaml);
    }
}

#[test]
fn problems_carry_subject_and_line() {
    let yaml = doc(BASE_NODES, BASE_EDGES).replace("prompt:", "promt:");
    let p = problems(&yaml);
    let q = p.iter().find(|q| q.message.contains("promt")).unwrap();
    assert_eq!(q.line, Some(5));
    assert_eq!(q.subject, Subject::Node { id: "work".into() });
    assert!(q.to_string().starts_with("line 5: node `work`"), "{q}");

    let yaml = doc(
        BASE_NODES,
        &format!("{BASE_EDGES}  - {{ from: work.failed, to: nowhere }}\n"),
    );
    let p = problems(&yaml);
    assert_eq!(p.0[0].subject, Subject::Edge { index: 2 });
    assert_eq!(p.0[0].line, Some(10));

    // A block-style node: the problem points at the key's line.
    let yaml = "version: 1
name: x
nodes:
  - id: start
    type: start
  - id: l
    type: loop
    max: 50
  - { id: e, type: end }
edges:
  - { from: start, to: l }
  - { from: l.repeat, to: e }
";
    let p = problems(yaml);
    assert_eq!(p.len(), 1, "{p}");
    assert_eq!(p.0[0].line, Some(8));
    assert!(p.0[0].message.contains("node `l`"));

    let p = problems("version: 1\nname: [unclosed\n");
    assert!(p.0[0].line.is_some(), "{p}");
}

#[test]
fn several_problems_at_once() {
    let yaml = "version: 3
name: x
colour: blue
nodes:
  - { id: start, type: start }
  - { id: Work, type: agent, promt: hi }
  - { id: l, type: loop, max: 99 }
  - { id: g, type: gate, message: \"{{nodes.ghost.comment}}\" }
edges:
  - { from: start, to: Work }
  - { from: Work.done, to: l }
  - { from: l.repeat, to: g }
  - { from: g.maybe, to: l }
";
    let p = problems(yaml);
    for fragments in [
        &["`version` must be 1"][..],
        &["unknown key `colour`"],
        &["node `Work`", "id must match"],
        &["node `Work`", "unknown key `promt`"],
        &["node `Work`", "missing required key `prompt`"],
        &["node `l`", "`max` must be from 1 to 20"],
        &["node `g`", "no node `ghost`"],
        &["edge `g.maybe -> l`", "no port `maybe`"],
        &["no `end` node"],
    ] {
        assert_mentions(&p, fragments, yaml);
    }
    assert!(p.len() >= 9, "{p}");
    // Sorted by line.
    let lines: Vec<u32> = p.iter().filter_map(|q| q.line).collect();
    assert!(lines.windows(2).all(|w| w[0] <= w[1]), "{lines:?}");
    // Not reported twice: the missing prompt is not also "must not be empty".
    assert!(
        !p.iter()
            .any(|q| q.message.contains("`prompt` must not be empty")),
        "{p}"
    );
}

#[test]
fn unknown_keys_at_each_level() {
    let yaml = "version: 1
name: x
extra: 1
nodes:
  - { id: start, type: start, label: s }
  - { id: p, type: parallel, branches: [{ id: a, prompt: x, model: m }, { id: b, prompt: y }], fresh: true }
  - { id: e, type: end, status: success, colour: red }
edges:
  - { from: start, to: p, why: because }
  - { from: p.done, to: e }
";
    let p = problems(yaml);
    for (key, who) in [
        ("extra", "the document"),
        ("label", "node `start`"),
        ("model", "branch `a`"),
        ("fresh", "node `p`"),
        ("colour", "node `e`"),
        ("why", "edge `start -> p`"),
    ] {
        assert_mentions(&p, &[&format!("unknown key `{key}`"), who], yaml);
    }
    assert_eq!(p.len(), 6, "{p}");
}

#[test]
fn graph_rules() {
    // A cycle through a loop's `repeat` is fine.
    ok(&doc(
        "  - { id: start, type: start }
  - { id: l, type: loop, max: 2 }
  - { id: a, type: agent, prompt: p }
  - { id: e, type: end }
",
        "  - { from: start, to: l }
  - { from: l.repeat, to: a }
  - { from: a.done, to: l }
  - { from: l.exhausted, to: e }
",
    ));
    // A cycle that only passes a loop through `exhausted` never ends.
    let p = problems(&doc(
        "  - { id: start, type: start }
  - { id: l, type: loop, max: 2 }
  - { id: a, type: agent, prompt: p }
  - { id: e, type: end }
",
        "  - { from: start, to: l }
  - { from: l.repeat, to: e }
  - { from: l.exhausted, to: a }
  - { from: a.done, to: l }
",
    ));
    assert_mentions(&p, &["`l`", "`a`", "cycle", "only through `exhausted`"], "");
    // A cycle without any loop, two nodes long; and a self-edge.
    let p = problems(&doc(
        "  - { id: start, type: start }
  - { id: a, type: agent, prompt: p }
  - { id: b, type: check }
  - { id: e, type: end }
",
        "  - { from: start, to: a }
  - { from: a.done, to: b }
  - { from: b.fail, to: a }
  - { from: b.pass, to: e }
  - { from: a.failed, to: a }
",
    ));
    assert_mentions(&p, &["nodes `a`, `b` form a cycle", "no `loop` node"], "");
    // A self-edge on the first node listed, which nothing else leads to.
    let p = problems(&doc(
        "  - { id: a, type: agent, prompt: p }
  - { id: start, type: start }
  - { id: e, type: end }
",
        "  - { from: start, to: e }
  - { from: a.done, to: a }
",
    ));
    assert_mentions(&p, &["node `a` is not reachable"], "");
    assert_mentions(&p, &["nodes `a` form a cycle", "no `loop` node"], "");
    // An unreachable island that is also a cycle reports both.
    let p = problems(&doc(
        "  - { id: start, type: start }
  - { id: e, type: end }
  - { id: x, type: agent, prompt: p }
  - { id: y, type: agent, prompt: p }
",
        "  - { from: start, to: e }
  - { from: x.done, to: y }
  - { from: y.done, to: x }
",
    ));
    assert_mentions(&p, &["node `x` is not reachable"], "");
    assert_mentions(&p, &["node `y` is not reachable"], "");
    assert_mentions(&p, &["cycle"], "");
    // Two starts and no end together.
    let p = problems(&doc(
        "  - { id: s1, type: start }\n  - { id: s2, type: start }\n",
        "",
    ));
    assert_mentions(&p, &["node `s2`", "second `start`", "`s1`"], "");
    // Only the second one is reported, not the first as well.
    let seconds = p
        .iter()
        .filter(|q| q.message.contains("second `start`"))
        .count();
    assert_eq!(seconds, 1, "{p}");
    assert_mentions(&p, &["no `end` node"], "");
    assert_mentions(&p, &["node `s2` is not reachable"], "");
    // An unknown type does not cascade into edge, reachability or cycle problems.
    let p = problems(&doc(
        "  - { id: start, type: start }
  - { id: l, type: lop, max: 2 }
  - { id: a, type: agent, prompt: p }
  - { id: e, type: end }
",
        "  - { from: start, to: l }
  - { from: l.repeat, to: a }
  - { from: a.done, to: l }
  - { from: l.exhausted, to: e }
",
    ));
    assert_eq!(p.len(), 1, "{p}");
}

#[test]
fn blank_fields_are_reported() {
    for (node, id, fragments) in [
        (
            "{ id: a, type: agent, prompt: p, fresh: true, runner: \" \" }",
            "a",
            ["node `a`", "`runner` must not be empty"],
        ),
        (
            "{ id: c, type: check, command: make, repo: \" \" }",
            "c",
            ["node `c`", "`repo` must not be empty"],
        ),
        (
            "{ id: q, type: ask, question: q, options: [a, \" \"] }",
            "q",
            ["node `q`", "`options` has an empty option"],
        ),
        (
            "{ id: p, type: parallel, branches: [{ id: a, prompt: \" \" }, { id: b, prompt: y }] }",
            "p",
            ["node `p`: branch `a`", "`prompt` must not be empty"],
        ),
    ] {
        let yaml = with_node(node, id);
        let p = problems(&yaml);
        assert_eq!(p.len(), 1, "{p}");
        assert_mentions(&p, &fragments, &yaml);
    }
}

#[test]
fn a_branch_whose_id_was_reported_is_not_checked_again() {
    let yaml = with_node(
        "{ id: p, type: parallel, branches: [{ id: 5, prompt: \" \" }, { id: b, prompt: y }] }",
        "p",
    );
    let p = problems(&yaml);
    assert_eq!(p.len(), 1, "{p}");
    assert_mentions(&p, &["branch #1", "`id` must be a string"], &yaml);
}

/// Ranges the YAML reader checks itself are checked again on the JSON path.
#[test]
fn json_form_ranges() {
    for (node, key, value, fragment) in [
        (
            "{ id: c, type: check }",
            "timeout_seconds",
            0,
            "node `c`: `timeout_seconds` must be from 1 to 86400, found 0",
        ),
        (
            "{ id: c, type: loop, max: 2 }",
            "max",
            21,
            "node `c`: `max` must be from 1 to 20, found 21",
        ),
    ] {
        let w = ok(&with_node(node, "c"));
        let mut json: serde_json::Value = serde_json::to_value(&w).unwrap();
        json["nodes"][3][key] = value.into();
        let err = serde_json::from_value::<Workflow>(json).unwrap_err();
        assert!(err.to_string().contains(fragment), "{err}");
    }
}

#[test]
fn edges_that_fail_still_count_for_reachability() {
    let more = format!("{BASE_NODES}  - {{ id: more, type: commit, message: m }}\n");
    // A second edge from one port.
    let p = problems(&doc(
        &more,
        &format!("{BASE_EDGES}  - {{ from: work.done, to: more }}\n"),
    ));
    assert_eq!(p.len(), 1, "{p}");
    assert_mentions(&p, &["port `work.done` already has an outgoing edge"], "");
    // An edge without a port from a node with two.
    let p = problems(&doc(
        &more,
        &format!("{BASE_EDGES}  - {{ from: work, to: more }}\n"),
    ));
    assert_eq!(p.len(), 1, "{p}");
    assert_mentions(&p, &["more than one port"], "");
}

#[test]
fn a_node_of_unknown_type_raises_nothing_else() {
    // An edge without a port from it, and a template naming its output.
    let p = problems(&doc(
        "  - { id: start, type: start }
  - { id: x, type: bogus }
  - { id: a, type: agent, prompt: \"{{nodes.x.anything}}\" }
  - { id: e, type: end }
",
        "  - { from: start, to: x }
  - { from: x, to: a }
  - { from: a.done, to: e }
",
    ));
    assert_eq!(p.len(), 1, "{p}");
    assert_mentions(&p, &["unknown type `bogus`"], "");
}

#[test]
fn shapes_the_reader_refuses() {
    for (node, id, fragments) in [
        (
            "{ id: p, type: parallel, branches: x }",
            "p",
            vec!["node `p`", "`branches` must be a list, found a string"],
        ),
        (
            "{ id: a, type: agent, prompt: p, verdicts: yes }",
            "a",
            vec!["node `a`", "`verdicts` must be a list of strings"],
        ),
        // A list with no string entry is not then also "an empty list".
        (
            "{ id: a, type: agent, prompt: p, verdicts: [3] }",
            "a",
            vec!["node `a`", "every entry of `verdicts` must be a string"],
        ),
        (
            "{ id: q, type: ask, question: Q, options: [3] }",
            "q",
            vec!["node `q`", "every entry of `options` must be a string"],
        ),
        (
            "{ id: p, type: parallel, branches: [x, { id: b, prompt: y }] }",
            "p",
            vec!["node `p`: branch #1 must be a mapping, found a string"],
        ),
        (
            "{ id: p, type: parallel, branches: [{ prompt: x }, { id: b, prompt: y }] }",
            "p",
            vec!["node `p`: branch #1", "missing required key `id`"],
        ),
    ] {
        let yaml = with_node(node, id);
        let p = problems(&yaml);
        assert_eq!(p.len(), 1, "{p}");
        assert_mentions(&p, &fragments, &yaml);
    }
    let yaml = doc(BASE_NODES, &format!("{BASE_EDGES}  - x\n"));
    let p = problems(&yaml);
    assert_mentions(&p, &["edge #3 must be a mapping, found a string"], &yaml);
    let w = ok(&format!("{}budget_usd: 2.5\n", doc(BASE_NODES, BASE_EDGES)));
    assert_eq!(w.budget_usd(), Some(2.5));
}

#[test]
fn a_second_edge_from_one_port_does_not_count_for_cycles() {
    // The refused edge would close a self-loop; only the port problem is reported.
    let yaml = doc(
        BASE_NODES,
        &format!("{BASE_EDGES}  - {{ from: work.done, to: work }}\n"),
    );
    let p = problems(&yaml);
    assert_eq!(p.len(), 1, "{p}");
    assert_mentions(
        &p,
        &["port `work.done` already has an outgoing edge"],
        &yaml,
    );
}

#[test]
fn a_yaml_error_on_the_first_line_keeps_its_line() {
    let p = problems("version: 1: 2\nname: x\n");
    assert_eq!(p.len(), 1, "{p}");
    assert_mentions(&p, &["invalid YAML"], "");
    assert_eq!(p.0[0].line, Some(1), "{p}");
}

#[test]
fn problems_without_a_line_come_last() {
    // The file name problem has no line and is found first; it is still listed last.
    let yaml = doc(BASE_NODES, BASE_EDGES).replace("version: 1", "version: 2");
    let p = parse("Bad Name", &yaml).unwrap_err();
    let mut lines = Vec::new();
    for q in &p {
        lines.push(q.line);
    }
    assert_eq!(lines, [Some(1), None], "{p}");
    // Shown one per line, in that order.
    let text = p.to_string();
    let shown: Vec<&str> = text.split('\n').collect();
    assert_eq!(shown.len(), 2, "{text:?}");
    assert!(shown[0].starts_with("line 1: "), "{text:?}");
    assert!(shown[1].starts_with("workflow id `Bad Name`"), "{text:?}");
}

#[test]
fn a_label_keeps_sixty_characters_and_shortens_longer_ones() {
    for (len, label) in [(60, "x".repeat(60)), (61, format!("{}…", "x".repeat(59)))] {
        let message = "x".repeat(len);
        let w = ok(&with_node(
            &format!("{{ id: g, type: gate, message: {message} }}"),
            "g",
        ));
        assert_eq!(w.node("g").unwrap().label(), label, "{len}");
    }
}

#[test]
fn an_id_of_forty_characters_is_valid() {
    let id = "a".repeat(40);
    let w = ok(&with_node(
        &format!("{{ id: {id}, type: commit, message: m }}"),
        &id,
    ));
    assert!(w.node(&id).is_some());
}

#[test]
fn a_node_without_an_id_is_named_by_its_position() {
    let yaml = doc(
        &format!("{BASE_NODES}  - {{ type: commit, message: \" \" }}\n"),
        BASE_EDGES,
    );
    let p = problems(&yaml);
    let blank = p
        .iter()
        .find(|q| q.message.contains("`message` must not be empty"))
        .unwrap_or_else(|| panic!("no blank message problem:\n{p}"));
    assert!(blank.message.starts_with("node #4: "), "{blank}");
    assert_eq!(blank.subject, Subject::Node { id: "#4".into() });

    // The reader names a node that is not a mapping the same way.
    let yaml = doc(&format!("{BASE_NODES}  - just a string\n"), BASE_EDGES);
    let p = problems(&yaml);
    assert_eq!(p.len(), 1, "{p}");
    assert_mentions(&p, &["node #4 must be a mapping"], &yaml);
    assert_eq!(p.0[0].subject, Subject::Node { id: "#4".into() });
}

#[test]
fn a_branch_prompt_problem_points_at_that_branch() {
    let yaml = "version: 1
name: x
nodes:
  - { id: start, type: start }
  - id: p
    type: parallel
    branches:
      - id: a
        prompt: fine
      - id: b
        prompt: \"{{nodes.zz.summary}}\"
  - { id: e, type: end }
edges:
  - { from: start, to: p }
  - { from: p.done, to: e }
";
    let p = problems(yaml);
    assert_eq!(p.len(), 1, "{p}");
    assert_mentions(&p, &["`branches[b].prompt`", "no node `zz`"], yaml);
    assert_eq!(p.0[0].line, Some(11), "{p}");
}

#[test]
fn bad_port_does_not_cascade() {
    let p = problems(&doc(
        &format!("{BASE_NODES}  - {{ id: more, type: commit, message: m }}\n"),
        &format!("{BASE_EDGES}  - {{ from: work.finish, to: more }}\n"),
    ));
    assert_eq!(p.len(), 1, "{p}");
    assert_mentions(&p, &["no port `finish`"], "");
}

#[test]
fn workflow_id_rules() {
    let yaml = doc(BASE_NODES, BASE_EDGES);
    assert!(parse("good-id", &yaml).is_ok());
    let p = parse("quick", &yaml).unwrap_err();
    assert_mentions(&p, &["`quick` is reserved"], "");
    let p = parse("Bad Name", &yaml).unwrap_err();
    assert_mentions(&p, &["workflow id `Bad Name`"], "");
}
