//! A seeded property test: random valid workflows, driven with random valid results, always
//! finish without panicking. No dependency: a small xorshift generator is enough.

use super::ok;
use crate::{Action, Context, EngineError, NodeResult, NodeType, RunState, advance, start};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*: deterministic and good enough to vary shapes.
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}

const MIDDLE: [NodeType; 8] = [
    NodeType::Agent,
    NodeType::Check,
    NodeType::Loop,
    NodeType::Gate,
    NodeType::Ask,
    NodeType::Parallel,
    NodeType::Commit,
    NodeType::Ship,
];

struct GenNode {
    id: String,
    t: NodeType,
}

/// A template referring to random outputs of earlier nodes (any node may be referred to; an
/// output not set yet renders empty).
fn template(rng: &mut Rng, nodes: &[GenNode]) -> String {
    let mut s = String::from("Do {{ task }} on {{session.branch}}");
    for _ in 0..rng.below(3) {
        let n = rng.pick(nodes);
        if let Some(out) = n.t.outputs().get(rng.below(n.t.outputs().len().max(1))) {
            s.push_str(&format!(" [{{{{nodes.{}.{out}}}}}]", n.id));
        }
    }
    s
}

/// Builds a random valid workflow as YAML. Non-`repeat` edges only go forward in node order,
/// so every cycle goes through a loop's `repeat`; each node is wired from an earlier free port,
/// so every node is reachable.
fn generate(rng: &mut Rng) -> String {
    let mut nodes = vec![GenNode {
        id: "start".into(),
        t: NodeType::Start,
    }];
    let mut free: Vec<(usize, String)> = vec![(0, "next".into())];
    let mut edges: Vec<(usize, String, usize)> = Vec::new();
    let mut lines = Vec::new();
    let middle = 1 + rng.below(12);
    for i in 1..=middle + 1 {
        let last = i == middle + 1;
        let t = if last || (free.len() >= 2 && rng.chance(10)) {
            NodeType::End
        } else {
            *rng.pick(&MIDDLE)
        };
        let id = format!("n{i}");
        let mut verdicts = Vec::new();
        let line = match t {
            NodeType::End => {
                let status = if rng.chance(30) { "failed" } else { "success" };
                format!("{{ id: {id}, type: end, status: {status} }}")
            }
            NodeType::Agent => {
                let mut extra = String::new();
                if rng.chance(40) {
                    let all = ["approve", "changes", "reject", "maybe"];
                    let n = 1 + rng.below(3);
                    verdicts = all[..n].iter().map(|s| (*s).to_owned()).collect();
                    extra.push_str(&format!(", verdicts: [{}]", verdicts.join(", ")));
                }
                if rng.chance(30) {
                    extra.push_str(", fresh: true, runner: mock, model: m");
                }
                format!(
                    "{{ id: {id}, type: agent, prompt: \"{}\"{extra} }}",
                    template(rng, &nodes)
                )
            }
            NodeType::Check => {
                if rng.chance(50) {
                    format!(
                        "{{ id: {id}, type: check, command: \"{}\", repo: api, timeout_seconds: {} }}",
                        template(rng, &nodes),
                        1 + rng.below(100)
                    )
                } else {
                    format!("{{ id: {id}, type: check }}")
                }
            }
            NodeType::Loop => format!("{{ id: {id}, type: loop, max: {} }}", 1 + rng.below(20)),
            NodeType::Gate => format!(
                "{{ id: {id}, type: gate, message: \"{}\" }}",
                template(rng, &nodes)
            ),
            NodeType::Ask => format!(
                "{{ id: {id}, type: ask, question: \"{}\", options: [a, b] }}",
                template(rng, &nodes)
            ),
            NodeType::Parallel => {
                let n = 2 + rng.below(3);
                let branches: Vec<String> = (0..n)
                    .map(|b| format!("{{ id: b{b}, prompt: \"{}\" }}", template(rng, &nodes)))
                    .collect();
                format!(
                    "{{ id: {id}, type: parallel, branches: [{}] }}",
                    branches.join(", ")
                )
            }
            NodeType::Commit => format!(
                "{{ id: {id}, type: commit, message: \"{}\" }}",
                template(rng, &nodes)
            ),
            NodeType::Ship => format!(
                "{{ id: {id}, type: ship, title: \"{}\", draft: {} }}",
                template(rng, &nodes),
                rng.chance(50)
            ),
            NodeType::Start => unreachable!(),
        };
        lines.push(line);
        // Wire the new node from a random free port of an earlier node.
        let (from, port) = free.swap_remove(rng.below(free.len()));
        edges.push((from, port, i));
        let ports: Vec<String> = if verdicts.is_empty() {
            t.default_ports().iter().map(|p| (*p).to_owned()).collect()
        } else {
            verdicts
                .iter()
                .cloned()
                .chain(["failed".to_owned()])
                .collect()
        };
        free.extend(ports.into_iter().map(|p| (i, p)));
        nodes.push(GenNode { id, t });
    }
    // Wire some remaining ports: `repeat` anywhere but start, others forward only.
    let n = nodes.len();
    for (from, port) in free {
        if !rng.chance(60) {
            continue;
        }
        let to = if port == "repeat" {
            1 + rng.below(n - 1)
        } else if from + 1 < n {
            from + 1 + rng.below(n - from - 1)
        } else {
            continue;
        };
        edges.push((from, port, to));
    }

    let mut yaml =
        String::from("version: 1\nname: Generated\nnodes:\n  - { id: start, type: start }\n");
    for l in &lines {
        yaml.push_str(&format!("  - {l}\n"));
    }
    yaml.push_str("edges:\n");
    for (from, port, to) in &edges {
        yaml.push_str(&format!(
            "  - {{ from: {}.{port}, to: {} }}\n",
            nodes[*from].id, nodes[*to].id
        ));
    }
    yaml
}

/// A random valid result for the action's node.
fn result_for(rng: &mut Rng, state: &RunState, node: &str) -> NodeResult {
    let n = state.workflow().node(node).expect("active node exists");
    let ports = n.ports();
    let port = ports[rng.below(ports.len())].name.clone();
    let mut r = NodeResult::new(node, port);
    for out in n.outputs() {
        if rng.chance(50) {
            r = r.with(*out, format!("{{{{task}}}} {}", rng.below(1000)));
        }
    }
    r
}

#[test]
fn random_workflows_always_finish() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut finished_success = 0;
    let mut finished_failed = 0;
    for case in 0..400 {
        let yaml = generate(&mut rng);
        let w = ok(&yaml);
        for _ in 0..3 {
            let (mut state, actions) = start(w.clone(), Context::default());
            let mut action = actions.into_iter().next().expect("one action");
            let mut answered = 0;
            while let Some(node) = action.node().map(str::to_owned) {
                // Now and then, an invalid result must be rejected without effect.
                if rng.chance(10) {
                    let before = state.clone();
                    let bad = NodeResult::new(node.clone(), "no-such-port");
                    assert!(matches!(
                        advance(&state, bad),
                        Err(EngineError::UnknownPort { .. })
                    ));
                    assert_eq!(state, before);
                }
                let result = result_for(&mut rng, &state, &node);
                let (next, actions) = advance(&state, result)
                    .unwrap_or_else(|e| panic!("case {case}: rejected: {e}\n{yaml}"));
                assert_eq!(actions.len(), 1);
                state = next;
                action = actions.into_iter().next().expect("one action");
                answered += 1;
                assert!(
                    answered <= crate::MAX_STEPS,
                    "case {case} never finished\n{yaml}"
                );
            }
            let Action::Finish(finish) = &action else {
                unreachable!()
            };
            assert_eq!(state.finished(), Some(finish));
            assert!(state.active_node().is_none());
            match finish.status {
                crate::EndStatus::Success => finished_success += 1,
                crate::EndStatus::Failed => finished_failed += 1,
            }
            // The snapshot agrees with the run.
            let snap = state.snapshot();
            assert_eq!(snap.nodes.len(), w.nodes().len());
            assert_eq!(snap.taken_edges.len(), state.taken_edges().len());
        }
    }
    // The generator exercises both outcomes.
    assert!(finished_success > 50 && finished_failed > 50);
}
