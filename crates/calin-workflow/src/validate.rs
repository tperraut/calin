//! Semantic validation (spec sections 2 to 4), shared by the YAML path ([`crate::parse`]) and
//! by deserializing a [`Workflow`] from its JSON form.
//!
//! The input is a [`Draft`]: the model with holes where the YAML reader already reported a
//! problem, so nothing is reported twice and one broken field does not hide the others.

use std::collections::{HashMap, HashSet};

use crate::model::{
    AgentNode, Branch, CheckNode, Edge, FORMAT_VERSION, MAX_ID_LEN, Node, NodeKind, Workflow,
    WorkflowRepr,
};
use crate::problem::{Collector, Problems, Subject};
use crate::template::Template;

/// Highest `loop.max` (spec section 3).
pub const MAX_LOOP: u32 = 20;
/// Fewest and most `parallel` branches (spec section 3).
pub const PARALLEL_BRANCHES: std::ops::RangeInclusive<usize> = 2..=4;
/// Highest `check.timeout_seconds`. The spec gives no bound; one day is this crate's choice.
pub const MAX_TIMEOUT_SECONDS: u64 = 86_400;

pub(crate) struct Draft {
    pub id: String,
    pub version: Option<i128>,
    pub name: Option<String>,
    pub description: Option<String>,
    pub budget_usd: Option<f64>,
    /// Lines of top-level keys.
    pub lines: HashMap<String, u32>,
    pub nodes: Vec<DraftNode>,
    pub edges: Vec<DraftEdge>,
}

pub(crate) struct DraftNode {
    /// `None` when missing or not a string (already reported).
    pub id: Option<String>,
    /// `None` when `type` is missing or unknown (already reported). Such a node accepts any
    /// port and may be a loop, so it causes no follow-on graph problems.
    pub kind: Option<NodeKind>,
    pub line: Option<u32>,
    /// Lines of fields, by key; nested keys as `branches.0.prompt`.
    pub lines: HashMap<String, u32>,
    /// Fields the reader already reported, so validation skips them.
    pub reported: HashSet<String>,
}

pub(crate) struct DraftEdge {
    /// `node` or `node.port`.
    pub from: Option<String>,
    pub to: Option<String>,
    pub line: Option<u32>,
}

/// Whether `s` is a valid node id: `[a-z][a-z0-9-]*`, at most 40 characters.
pub fn is_valid_id(s: &str) -> bool {
    let mut chars = s.chars();
    s.len() <= MAX_ID_LEN
        && chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Whether `s` is a valid verdict word: `[a-z][a-z0-9-]*`.
pub fn is_valid_word(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

pub(crate) fn validate_repr(r: WorkflowRepr) -> Result<Workflow, Problems> {
    let draft = Draft {
        id: r.id,
        version: Some(r.version.into()),
        name: Some(r.name),
        description: r.description,
        budget_usd: r.budget_usd,
        lines: HashMap::new(),
        nodes: r
            .nodes
            .into_iter()
            .map(|n| DraftNode {
                id: Some(n.id),
                kind: Some(n.kind),
                line: None,
                lines: HashMap::new(),
                reported: HashSet::new(),
            })
            .collect(),
        edges: r
            .edges
            .into_iter()
            .map(|e| DraftEdge {
                from: Some(format!("{}.{}", e.from, e.port)),
                to: Some(e.to),
                line: None,
            })
            .collect(),
    };
    let mut c = Collector::default();
    match validate(draft, &mut c) {
        Some(w) if c.is_empty() => Ok(w),
        _ => Err(c.finish()),
    }
}

/// Node `i` as named in messages and subjects.
fn node_name(i: usize, n: &DraftNode) -> (String, Subject) {
    match &n.id {
        Some(id) => (format!("node `{id}`"), Subject::Node { id: id.clone() }),
        None => (
            format!("node #{}", i + 1),
            Subject::Node {
                id: format!("#{}", i + 1),
            },
        ),
    }
}

/// Validates and, when there is no problem at all (including ones already in `c`), builds.
pub(crate) fn validate(d: Draft, c: &mut Collector) -> Option<Workflow> {
    let top_line = |k: &str| d.lines.get(k).copied();
    if let Some(v) = d.version
        && v != i128::from(FORMAT_VERSION)
    {
        c.push(
            top_line("version"),
            Subject::Workflow,
            format!("`version` must be {FORMAT_VERSION}, found {v}"),
        );
    }
    if let Some(name) = &d.name
        && name.trim().is_empty()
    {
        c.push(
            top_line("name"),
            Subject::Workflow,
            "`name` must not be empty",
        );
    }
    if let Some(b) = d.budget_usd
        && !(b.is_finite() && b > 0.0)
    {
        c.push(
            top_line("budget_usd"),
            Subject::Workflow,
            format!("`budget_usd` must be a positive number, found {b}"),
        );
    }

    // Ids first: everything after looks nodes up by id.
    let mut by_id: HashMap<&str, usize> = HashMap::new();
    for (i, n) in d.nodes.iter().enumerate() {
        let Some(id) = &n.id else { continue };
        let (name, subject) = node_name(i, n);
        let line = n.lines.get("id").copied().or(n.line);
        if !is_valid_id(id) {
            c.push(
                line,
                subject.clone(),
                format!(
                    "{name}: id must match `[a-z][a-z0-9-]*` and be at most {MAX_ID_LEN} characters"
                ),
            );
        }
        match by_id.get(id.as_str()) {
            Some(&first) => {
                let at = d.nodes[first]
                    .line
                    .map(|l| format!(" (line {l})"))
                    .unwrap_or_default();
                c.push(
                    line,
                    subject,
                    format!("{name}: id is already used by an earlier node{at}"),
                );
            }
            None => {
                by_id.insert(id, i);
            }
        }
    }

    for (i, n) in d.nodes.iter().enumerate() {
        if let Some(kind) = &n.kind {
            check_fields(i, n, kind, c);
            check_templates(i, n, kind, &d.nodes, &by_id, c);
        }
    }

    let (edges, loose) = check_edges(&d, &by_id, c);
    check_graph(&d, &edges, &loose, c);

    if !c.is_empty() {
        return None;
    }
    let built = build(d, edges);
    if built.is_none() {
        // Unreachable by construction; reported rather than panicking so a caller still gets
        // an error that says something.
        c.push(
            None,
            Subject::Workflow,
            "internal error: the workflow passed validation but could not be assembled",
        );
    }
    built
}

fn check_fields(i: usize, n: &DraftNode, kind: &NodeKind, c: &mut Collector) {
    let (name, subject) = node_name(i, n);
    let line = |k: &str| n.lines.get(k).copied().or(n.line);
    let skip = |k: &str| n.reported.contains(k);
    let mut push = |key: &str, msg: String| c.push(line(key), subject.clone(), msg);
    let non_empty = |key: &str, t: &Template, push: &mut dyn FnMut(&str, String)| {
        if !skip(key) && t.source().trim().is_empty() {
            push(key, format!("{name}: `{key}` must not be empty"));
        }
    };
    match kind {
        NodeKind::Start | NodeKind::End { .. } => {}
        NodeKind::Agent(AgentNode {
            prompt,
            fresh,
            verdicts,
            runner,
            model,
        }) => {
            non_empty("prompt", prompt, &mut push);
            let mut seen = HashSet::new();
            for v in verdicts {
                if !is_valid_word(v) {
                    push(
                        "verdicts",
                        format!("{name}: verdict `{v}` must match `[a-z][a-z0-9-]*`"),
                    );
                } else if v == "failed" {
                    push(
                        "verdicts",
                        format!("{name}: verdict `failed` collides with the node's `failed` port"),
                    );
                }
                if !seen.insert(v) {
                    push("verdicts", format!("{name}: verdict `{v}` is listed twice"));
                }
            }
            for (key, value) in [("runner", runner), ("model", model)] {
                let Some(value) = value else { continue };
                if skip(key) {
                    continue;
                }
                if value.trim().is_empty() {
                    push(key, format!("{name}: `{key}` must not be empty"));
                }
                if !fresh {
                    push(
                        key,
                        format!("{name}: `{key}` applies only with `fresh: true`"),
                    );
                }
            }
        }
        NodeKind::Check(CheckNode {
            command,
            repo,
            timeout_seconds,
        }) => {
            if let Some(cmd) = command {
                non_empty("command", cmd, &mut push);
            }
            if let Some(repo) = repo
                && !skip("repo")
            {
                if repo.trim().is_empty() {
                    push("repo", format!("{name}: `repo` must not be empty"));
                }
                if command.is_none() && !skip("command") {
                    push(
                        "repo",
                        format!("{name}: `repo` applies only with `command`"),
                    );
                }
            }
            if !skip("timeout_seconds") && !(1..=MAX_TIMEOUT_SECONDS).contains(timeout_seconds) {
                push(
                    "timeout_seconds",
                    format!(
                        "{name}: `timeout_seconds` must be from 1 to {MAX_TIMEOUT_SECONDS}, found {timeout_seconds}"
                    ),
                );
            }
        }
        NodeKind::Loop { max } => {
            if !skip("max") && !(1..=MAX_LOOP).contains(max) {
                push(
                    "max",
                    format!("{name}: `max` must be from 1 to {MAX_LOOP}, found {max}"),
                );
            }
        }
        NodeKind::Gate { message } | NodeKind::Commit { message } => {
            non_empty("message", message, &mut push);
        }
        NodeKind::Ask { question, options } => {
            non_empty("question", question, &mut push);
            if let Some(options) = options {
                check_list(&name, "options", "option", options, &mut push);
            }
        }
        NodeKind::Parallel { branches } => {
            if !skip("branches") && !PARALLEL_BRANCHES.contains(&branches.len()) {
                push(
                    "branches",
                    format!(
                        "{name}: `branches` must have {} to {} entries, found {}",
                        PARALLEL_BRANCHES.start(),
                        PARALLEL_BRANCHES.end(),
                        branches.len()
                    ),
                );
            }
            let mut seen = HashSet::new();
            for (b, Branch { id, prompt, repos }) in branches.iter().enumerate() {
                let key = format!("branches.{b}");
                if skip(&format!("{key}.id")) {
                    continue;
                }
                if !is_valid_id(id) {
                    push(
                        &key,
                        format!(
                            "{name}: branch id `{id}` must match `[a-z][a-z0-9-]*` and be at most {MAX_ID_LEN} characters"
                        ),
                    );
                }
                if !seen.insert(id) {
                    push(&key, format!("{name}: branch id `{id}` is used twice"));
                }
                let pkey = format!("{key}.prompt");
                if !skip(&pkey) && prompt.source().trim().is_empty() {
                    push(
                        &pkey,
                        format!("{name}: branch `{id}`: `prompt` must not be empty"),
                    );
                }
                if let Some(repos) = repos {
                    check_list(
                        &format!("{name}: branch `{id}`"),
                        &format!("{key}.repos"),
                        "repo",
                        repos,
                        &mut push,
                    );
                }
            }
        }
        NodeKind::Ship { title, .. } => {
            if let Some(t) = title {
                non_empty("title", t, &mut push);
            }
        }
    }
}

/// A list of names: non-empty, no empty entry, no duplicate.
fn check_list(
    name: &str,
    key: &str,
    what: &str,
    items: &[String],
    push: &mut dyn FnMut(&str, String),
) {
    let shown = key.rsplit('.').next().unwrap_or(key);
    if items.is_empty() {
        push(key, format!("{name}: `{shown}` must not be an empty list"));
    }
    let mut seen = HashSet::new();
    for item in items {
        if item.trim().is_empty() {
            push(key, format!("{name}: `{shown}` has an empty {what}"));
        } else if !seen.insert(item) {
            push(key, format!("{name}: {what} `{item}` is listed twice"));
        }
    }
}

fn check_templates(
    i: usize,
    n: &DraftNode,
    kind: &NodeKind,
    nodes: &[DraftNode],
    by_id: &HashMap<&str, usize>,
    c: &mut Collector,
) {
    let (name, subject) = node_name(i, n);
    let probe = Node {
        id: String::new(),
        kind: kind.clone(),
    };
    for (key, template) in probe.templates() {
        let line_key = key
            .strip_prefix("branches[")
            .and_then(|rest| rest.split_once("].prompt"))
            .and_then(|(bid, _)| {
                let NodeKind::Parallel { branches } = kind else {
                    return None;
                };
                let b = branches.iter().position(|b| b.id == bid)?;
                Some(format!("branches.{b}.prompt"))
            })
            .unwrap_or_else(|| key.clone());
        let line = n.lines.get(&line_key).copied().or(n.line);
        for (node, output) in template.references() {
            let Some(&target) = by_id.get(node) else {
                c.push(
                    line,
                    subject.clone(),
                    format!(
                        "{name}: `{key}` refers to `{{{{nodes.{node}.{output}}}}}`, but there is no node `{node}`"
                    ),
                );
                continue;
            };
            // A node whose type is unknown was already reported; its outputs are unknown too.
            let Some(target_kind) = &nodes[target].kind else {
                continue;
            };
            let t = target_kind.node_type();
            if !t.outputs().contains(&output) {
                let has = if t.outputs().is_empty() {
                    "it has no outputs".to_owned()
                } else {
                    format!("its outputs are {}", t.outputs().join(", "))
                };
                c.push(
                    line,
                    subject.clone(),
                    format!(
                        "{name}: `{key}` refers to `{{{{nodes.{node}.{output}}}}}`, but `{node}` (type `{}`) has no output `{output}`; {has}",
                        t.as_str()
                    ),
                );
            }
        }
    }
}

/// An edge whose endpoints resolved: `from` and `to` are node indexes.
struct Resolved {
    from: usize,
    port: String,
    to: usize,
}

/// Checks every edge. Returns the edges that resolved, and the `(from, to)` pairs of edges
/// whose port did not, which still count for reachability.
fn check_edges(
    d: &Draft,
    by_id: &HashMap<&str, usize>,
    c: &mut Collector,
) -> (Vec<Resolved>, Vec<(usize, usize)>) {
    let mut out = Vec::new();
    let mut loose = Vec::new();
    let mut used: HashMap<(usize, String), usize> = HashMap::new();
    for (k, e) in d.edges.iter().enumerate() {
        let subject = Subject::Edge { index: k };
        let (Some(from), Some(to)) = (&e.from, &e.to) else {
            continue;
        };
        let name = format!("edge `{from} -> {to}`");
        let (from_id, port) = match from.split_once('.') {
            Some((n, p)) => (n, Some(p)),
            None => (from.as_str(), None),
        };
        let from_ix = by_id.get(from_id).copied();
        let to_ix = by_id.get(to.as_str()).copied();
        if from_ix.is_none() {
            c.push(
                e.line,
                subject.clone(),
                format!("{name}: `from` names unknown node `{from_id}`"),
            );
        }
        match to_ix {
            None => {
                let hint = if to.contains('.') {
                    "; `to` is a node id, without a port"
                } else {
                    ""
                };
                c.push(
                    e.line,
                    subject.clone(),
                    format!("{name}: `to` names unknown node `{to}`{hint}"),
                );
            }
            Some(t) if matches!(d.nodes[t].kind, Some(NodeKind::Start)) => {
                c.push(
                    e.line,
                    subject.clone(),
                    format!("{name}: no edge may lead into the `start` node `{to}`"),
                );
            }
            Some(_) => {}
        }
        let Some(f) = from_ix else { continue };
        let port = match (&d.nodes[f].kind, port) {
            // Unknown type: accept the port as written so the graph checks still run.
            (None, Some(p)) => Some(p.to_owned()),
            (None, None) => None,
            (Some(kind), Some(p)) => {
                if kind.port_names().any(|q| q == p) {
                    Some(p.to_owned())
                } else {
                    c.push(
                        e.line,
                        subject.clone(),
                        format!(
                            "{name}: node `{from_id}` has no port `{p}`; {}",
                            describe_ports(kind)
                        ),
                    );
                    None
                }
            }
            (Some(kind), None) => {
                let ports: Vec<&str> = kind.port_names().collect();
                if let [only] = ports[..] {
                    Some(only.to_owned())
                } else {
                    c.push(
                        e.line,
                        subject.clone(),
                        format!(
                            "{name}: node `{from_id}` has more than one port, so `from` must name one as `{from_id}.<port>`; {}",
                            describe_ports(kind)
                        ),
                    );
                    None
                }
            }
        };
        let Some(port) = port else {
            // Still counts for reachability, so one bad port does not make the rest of the
            // graph "unreachable" too.
            if let Some(t) = to_ix {
                loose.push((f, t));
            }
            continue;
        };
        if let Some(&first) = used.get(&(f, port.clone())) {
            if let Some(t) = to_ix {
                loose.push((f, t));
            }
            let at = d.edges[first]
                .line
                .map(|l| format!(" on line {l}"))
                .unwrap_or_default();
            c.push(
                e.line,
                subject,
                format!(
                    "{name}: port `{from_id}.{port}` already has an outgoing edge{at}; a port has at most one"
                ),
            );
            continue;
        }
        used.insert((f, port.clone()), k);
        if let Some(t) = to_ix {
            out.push(Resolved {
                from: f,
                port,
                to: t,
            });
        }
    }
    (out, loose)
}

fn describe_ports(kind: &NodeKind) -> String {
    let ports: Vec<&str> = kind.port_names().collect();
    if ports.is_empty() {
        format!("an `{}` node has no ports", kind.node_type().as_str())
    } else {
        format!("its ports are {}", ports.join(", "))
    }
}

fn check_graph(d: &Draft, edges: &[Resolved], loose: &[(usize, usize)], c: &mut Collector) {
    let starts: Vec<usize> = (0..d.nodes.len())
        .filter(|&i| matches!(d.nodes[i].kind, Some(NodeKind::Start)))
        .collect();
    let has_end = d
        .nodes
        .iter()
        .any(|n| matches!(n.kind, Some(NodeKind::End { .. })));
    let nodes_line = d.lines.get("nodes").copied();
    match starts[..] {
        [] => c.push(
            nodes_line,
            Subject::Workflow,
            "there is no `start` node; there must be exactly one",
        ),
        [_] => {}
        [first, ..] => {
            for &s in &starts[1..] {
                let (name, subject) = node_name(s, &d.nodes[s]);
                let (first_name, _) = node_name(first, &d.nodes[first]);
                c.push(
                    d.nodes[s].line,
                    subject,
                    format!(
                        "{name}: a second `start` node; there must be exactly one (the first is {first_name})"
                    ),
                );
            }
        }
    }
    if !has_end {
        c.push(
            nodes_line,
            Subject::Workflow,
            "there is no `end` node; there must be at least one",
        );
    }

    let n = d.nodes.len();
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    for e in edges {
        adj[e.from].push(e.to);
    }
    for &(from, to) in loose {
        adj[from].push(to);
    }

    if let Some(&s) = starts.first() {
        let mut seen = vec![false; n];
        let mut stack = vec![s];
        seen[s] = true;
        while let Some(v) = stack.pop() {
            for &w in &adj[v] {
                if !seen[w] {
                    seen[w] = true;
                    stack.push(w);
                }
            }
        }
        for (i, node) in d.nodes.iter().enumerate() {
            // A node without an id was reported already and cannot be an edge target.
            if !seen[i] && node.id.is_some() {
                let (name, subject) = node_name(i, node);
                c.push(
                    node.line,
                    subject,
                    format!("{name} is not reachable from `start`"),
                );
            }
        }
    }

    // Spec 2 says every cycle passes through a loop node. A cycle that only passes a loop
    // through `exhausted` would run forever, since `exhausted` can be taken any number of
    // times; so the rule enforced is: without the `repeat` edges, the graph has no cycle.
    // A node of unknown type might be a loop, so its edges are left out too.
    let mut acyclic_adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    for e in edges {
        let bounded = match &d.nodes[e.from].kind {
            None => true,
            Some(NodeKind::Loop { .. }) => e.port == "repeat",
            Some(_) => false,
        };
        if !bounded {
            acyclic_adj[e.from].push(e.to);
        }
    }
    let self_loops: HashSet<usize> = edges
        .iter()
        .filter(|e| e.from == e.to && acyclic_adj[e.from].contains(&e.to))
        .map(|e| e.from)
        .collect();
    for mut scc in strongly_connected(&acyclic_adj) {
        if scc.len() < 2 && !scc.first().is_some_and(|v| self_loops.contains(v)) {
            continue;
        }
        scc.sort_unstable();
        let names: Vec<String> = scc
            .iter()
            .map(|&i| match &d.nodes[i].id {
                Some(id) => format!("`{id}`"),
                None => format!("#{}", i + 1),
            })
            .collect();
        let loops: Vec<String> = scc
            .iter()
            .filter(|&&i| matches!(d.nodes[i].kind, Some(NodeKind::Loop { .. })))
            .filter_map(|&i| d.nodes[i].id.as_ref().map(|id| format!("`{id}`")))
            .collect();
        let detail = if loops.is_empty() {
            "it passes through no `loop` node".to_owned()
        } else {
            format!(
                "it leaves loop {} only through `exhausted`, which does not bound it",
                loops.join(", ")
            )
        };
        let first = scc[0];
        let (_, subject) = node_name(first, &d.nodes[first]);
        c.push(
            d.nodes[first].line,
            subject,
            format!(
                "nodes {} form a cycle that is not bounded: {detail}; every cycle must go through a `loop` node's `repeat` port",
                names.join(", ")
            ),
        );
    }
}

/// Tarjan's algorithm with an explicit stack, so a large file cannot overflow the call stack.
fn strongly_connected(adj: &[Vec<usize>]) -> Vec<Vec<usize>> {
    let n = adj.len();
    let mut index: Vec<Option<usize>> = vec![None; n];
    let mut low = vec![0usize; n];
    let mut on_stack = vec![false; n];
    let mut stack = Vec::new();
    let mut next = 0usize;
    let mut out = Vec::new();
    for s in 0..n {
        if index[s].is_some() {
            continue;
        }
        let mut call: Vec<(usize, usize)> = vec![(s, 0)];
        index[s] = Some(next);
        low[s] = next;
        next += 1;
        stack.push(s);
        on_stack[s] = true;
        while let Some(&mut (v, ref mut i)) = call.last_mut() {
            if let Some(&w) = adj[v].get(*i) {
                *i += 1;
                match index[w] {
                    None => {
                        index[w] = Some(next);
                        low[w] = next;
                        next += 1;
                        stack.push(w);
                        on_stack[w] = true;
                        call.push((w, 0));
                    }
                    Some(iw) if on_stack[w] => low[v] = low[v].min(iw),
                    Some(_) => {}
                }
                continue;
            }
            call.pop();
            if let Some(&(u, _)) = call.last() {
                low[u] = low[u].min(low[v]);
            }
            if Some(low[v]) == index[v] {
                let mut scc = Vec::new();
                while let Some(w) = stack.pop() {
                    on_stack[w] = false;
                    scc.push(w);
                    if w == v {
                        break;
                    }
                }
                out.push(scc);
            }
        }
    }
    out
}

/// Assembles the workflow. Only called with no problem reported, when every hole is filled.
fn build(d: Draft, edges: Vec<Resolved>) -> Option<Workflow> {
    let mut nodes = Vec::with_capacity(d.nodes.len());
    for n in d.nodes {
        nodes.push(Node {
            id: n.id?,
            kind: n.kind?,
        });
    }
    // Every edge resolved when nothing was reported.
    if edges.len() != d.edges.len() {
        return None;
    }
    let edges = edges
        .into_iter()
        .map(|e| Edge {
            from: nodes[e.from].id.clone(),
            port: e.port,
            to: nodes[e.to].id.clone(),
        })
        .collect::<Vec<_>>();
    Some(Workflow {
        id: d.id,
        name: d.name?,
        description: d.description,
        budget_usd: d.budget_usd,
        nodes,
        edges,
    })
}
