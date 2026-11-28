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
    s.len() <= MAX_ID_LEN && is_valid_word(s)
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
    check_header(&d, c);
    // Ids first: everything after looks nodes up by id.
    let by_id = index_ids(&d, c);
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

/// `version`, `name` and `budget_usd`.
fn check_header(d: &Draft, c: &mut Collector) {
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
}

/// Each node's index by id, the first node for a repeated id. Reports invalid and repeated ids.
fn index_ids<'d>(d: &'d Draft, c: &mut Collector) -> HashMap<&'d str, usize> {
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
    by_id
}

fn check_fields(i: usize, n: &DraftNode, kind: &NodeKind, c: &mut Collector) {
    let (name, subject) = node_name(i, n);
    let mut f = FieldCheck {
        node: n,
        name,
        subject,
        c,
    };
    match kind {
        NodeKind::Start | NodeKind::End { .. } => {}
        NodeKind::Agent(agent) => f.agent(agent),
        NodeKind::Check(check) => f.check(check),
        NodeKind::Loop { max } => f.in_range("max", u64::from(*max), MAX_LOOP.into()),
        NodeKind::Gate { message } | NodeKind::Commit { message } => {
            f.non_empty("message", Some(message))
        }
        NodeKind::Ask { question, options } => {
            f.non_empty("question", Some(question));
            let name = f.name.clone();
            f.list(&name, "options", "option", options.as_deref());
        }
        NodeKind::Parallel { branches } => f.parallel(branches),
        NodeKind::Ship { title, .. } => f.non_empty("title", title.as_ref()),
    }
}

/// Reports the field problems of one node, under its name, each on its field's line. Fields
/// the reader already reported are skipped.
struct FieldCheck<'a> {
    node: &'a DraftNode,
    name: String,
    subject: Subject,
    c: &'a mut Collector,
}

impl FieldCheck<'_> {
    fn push(&mut self, key: &str, message: String) {
        let line = self.node.lines.get(key).copied().or(self.node.line);
        self.c.push(line, self.subject.clone(), message);
    }

    fn skip(&self, key: &str) -> bool {
        self.node.reported.contains(key)
    }

    /// A template, when present, must have text.
    fn non_empty(&mut self, key: &str, template: Option<&Template>) {
        let Some(template) = template else { return };
        if !self.skip(key) && template.source().trim().is_empty() {
            let message = format!("{}: `{key}` must not be empty", self.name);
            self.push(key, message);
        }
    }

    fn in_range(&mut self, key: &str, value: u64, max: u64) {
        if !self.skip(key) && !(1..=max).contains(&value) {
            let message = format!(
                "{}: `{key}` must be from 1 to {max}, found {value}",
                self.name
            );
            self.push(key, message);
        }
    }

    fn agent(&mut self, agent: &AgentNode) {
        self.non_empty("prompt", Some(&agent.prompt));
        self.verdicts(&agent.verdicts);
        for (key, value) in [("runner", &agent.runner), ("model", &agent.model)] {
            self.child_setting(key, value.as_deref(), agent.fresh);
        }
    }

    fn verdicts(&mut self, verdicts: &[String]) {
        let name = self.name.clone();
        let mut seen = HashSet::new();
        for v in verdicts {
            if !is_valid_word(v) {
                self.push(
                    "verdicts",
                    format!("{name}: verdict `{v}` must match `[a-z][a-z0-9-]*`"),
                );
            } else if v == "failed" {
                self.push(
                    "verdicts",
                    format!("{name}: verdict `failed` collides with the node's `failed` port"),
                );
            }
            if !seen.insert(v) {
                self.push("verdicts", format!("{name}: verdict `{v}` is listed twice"));
            }
        }
    }

    /// `runner` or `model`: set only for a fresh child session.
    fn child_setting(&mut self, key: &str, value: Option<&str>, fresh: bool) {
        let Some(value) = value else { return };
        if self.skip(key) {
            return;
        }
        let name = self.name.clone();
        if value.trim().is_empty() {
            self.push(key, format!("{name}: `{key}` must not be empty"));
        }
        if !fresh {
            self.push(
                key,
                format!("{name}: `{key}` applies only with `fresh: true`"),
            );
        }
    }

    fn check(&mut self, check: &CheckNode) {
        self.non_empty("command", check.command.as_ref());
        if let Some(repo) = &check.repo
            && !self.skip("repo")
        {
            let name = self.name.clone();
            if repo.trim().is_empty() {
                self.push("repo", format!("{name}: `repo` must not be empty"));
            }
            if check.command.is_none() && !self.skip("command") {
                self.push(
                    "repo",
                    format!("{name}: `repo` applies only with `command`"),
                );
            }
        }
        self.in_range(
            "timeout_seconds",
            check.timeout_seconds,
            MAX_TIMEOUT_SECONDS,
        );
    }

    fn parallel(&mut self, branches: &[Branch]) {
        if !self.skip("branches") && !PARALLEL_BRANCHES.contains(&branches.len()) {
            let message = format!(
                "{}: `branches` must have {} to {} entries, found {}",
                self.name,
                PARALLEL_BRANCHES.start(),
                PARALLEL_BRANCHES.end(),
                branches.len()
            );
            self.push("branches", message);
        }
        let mut seen = HashSet::new();
        for (b, branch) in branches.iter().enumerate() {
            let key = format!("branches.{b}");
            if self.skip(&format!("{key}.id")) {
                continue;
            }
            let id = &branch.id;
            if !is_valid_id(id) {
                self.push(
                    &key,
                    format!(
                        "{}: branch id `{id}` must match `[a-z][a-z0-9-]*` and be at most {MAX_ID_LEN} characters",
                        self.name
                    ),
                );
            }
            if !seen.insert(id) {
                self.push(
                    &key,
                    format!("{}: branch id `{id}` is used twice", self.name),
                );
            }
            self.branch_fields(&key, branch);
        }
    }

    /// A branch's prompt and repos; `key` is `branches.<index>`.
    fn branch_fields(&mut self, key: &str, Branch { id, prompt, repos }: &Branch) {
        let owner = format!("{}: branch `{id}`", self.name);
        let prompt_key = format!("{key}.prompt");
        if !self.skip(&prompt_key) && prompt.source().trim().is_empty() {
            self.push(&prompt_key, format!("{owner}: `prompt` must not be empty"));
        }
        self.list(&owner, &format!("{key}.repos"), "repo", repos.as_deref());
    }

    /// A list of names, when present: non-empty, no empty entry, no duplicate.
    fn list(&mut self, owner: &str, key: &str, what: &str, items: Option<&[String]>) {
        let Some(items) = items else { return };
        let shown = key.rsplit('.').next().unwrap_or(key);
        if items.is_empty() {
            self.push(key, format!("{owner}: `{shown}` must not be an empty list"));
        }
        let mut seen = HashSet::new();
        for item in items {
            if item.trim().is_empty() {
                self.push(key, format!("{owner}: `{shown}` has an empty {what}"));
            } else if !seen.insert(item) {
                self.push(key, format!("{owner}: {what} `{item}` is listed twice"));
            }
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
    let mut check = EdgeCheck {
        d,
        c,
        used: HashMap::new(),
        resolved: Vec::new(),
        loose: Vec::new(),
    };
    for (k, e) in d.edges.iter().enumerate() {
        if let Some(ends) = EdgeEnds::read(k, e, by_id) {
            check.edge(&ends);
        }
    }
    (check.resolved, check.loose)
}

/// An edge whose `from` and `to` were read, with its ends looked up.
struct EdgeEnds<'e> {
    index: usize,
    line: Option<u32>,
    /// `edge `<from> -> <to>``, for messages.
    name: String,
    from_id: &'e str,
    port: Option<&'e str>,
    to_id: &'e str,
    from: Option<usize>,
    to: Option<usize>,
}

impl<'e> EdgeEnds<'e> {
    fn read(index: usize, e: &'e DraftEdge, by_id: &HashMap<&str, usize>) -> Option<Self> {
        let (Some(from), Some(to)) = (&e.from, &e.to) else {
            return None;
        };
        let (from_id, port) = from
            .split_once('.')
            .map_or((from.as_str(), None), |(n, p)| (n, Some(p)));
        Some(EdgeEnds {
            index,
            line: e.line,
            name: format!("edge `{from} -> {to}`"),
            from_id,
            port,
            to_id: to,
            from: by_id.get(from_id).copied(),
            to: by_id.get(to.as_str()).copied(),
        })
    }
}

struct EdgeCheck<'d, 'c> {
    d: &'d Draft,
    c: &'c mut Collector,
    /// The edge that took each `(node, port)` first.
    used: HashMap<(usize, String), usize>,
    resolved: Vec<Resolved>,
    loose: Vec<(usize, usize)>,
}

impl EdgeCheck<'_, '_> {
    fn push(&mut self, ends: &EdgeEnds, message: String) {
        let subject = Subject::Edge { index: ends.index };
        self.c
            .push(ends.line, subject, format!("{}: {message}", ends.name));
    }

    fn edge(&mut self, ends: &EdgeEnds) {
        self.report_unknown_ends(ends);
        let Some(from) = ends.from else { return };
        let port = self
            .port(ends, from)
            .filter(|port| self.claim(ends, from, port));
        match (port, ends.to) {
            (Some(port), Some(to)) => self.resolved.push(Resolved { from, port, to }),
            // Still counts for reachability, so one bad port does not make the rest of the
            // graph "unreachable" too.
            (None, Some(to)) => self.loose.push((from, to)),
            (_, None) => {}
        }
    }

    fn report_unknown_ends(&mut self, ends: &EdgeEnds) {
        let (from_id, to_id) = (ends.from_id, ends.to_id);
        if ends.from.is_none() {
            self.push(ends, format!("`from` names unknown node `{from_id}`"));
        }
        match ends.to {
            None => {
                let hint = if to_id.contains('.') {
                    "; `to` is a node id, without a port"
                } else {
                    ""
                };
                self.push(ends, format!("`to` names unknown node `{to_id}`{hint}"));
            }
            Some(t) if matches!(self.d.nodes[t].kind, Some(NodeKind::Start)) => {
                self.push(
                    ends,
                    format!("no edge may lead into the `start` node `{to_id}`"),
                );
            }
            Some(_) => {}
        }
    }

    /// The port the edge leaves `from` through, when that node has it. A node of unknown type
    /// takes the port as written, so the graph checks still run.
    fn port(&mut self, ends: &EdgeEnds, from: usize) -> Option<String> {
        let d = self.d;
        let Some(kind) = &d.nodes[from].kind else {
            return ends.port.map(str::to_owned);
        };
        let from_id = ends.from_id;
        let ports: Vec<&str> = kind.port_names().collect();
        let problem = match (ends.port, &ports[..]) {
            (Some(p), _) if ports.contains(&p) => return Some(p.to_owned()),
            (None, [only]) => return Some((*only).to_owned()),
            (Some(p), _) => format!("node `{from_id}` has no port `{p}`"),
            (None, _) => format!(
                "node `{from_id}` has more than one port, so `from` must name one as `{from_id}.<port>`"
            ),
        };
        self.push(ends, format!("{problem}; {}", describe_ports(kind)));
        None
    }

    /// Takes `port` of `from` for this edge; false, reported, when an earlier edge took it.
    fn claim(&mut self, ends: &EdgeEnds, from: usize, port: &str) -> bool {
        let Some(&first) = self.used.get(&(from, port.to_owned())) else {
            self.used.insert((from, port.to_owned()), ends.index);
            return true;
        };
        let at = self.d.edges[first]
            .line
            .map(|l| format!(" on line {l}"))
            .unwrap_or_default();
        self.push(
            ends,
            format!(
                "port `{}.{port}` already has an outgoing edge{at}; a port has at most one",
                ends.from_id
            ),
        );
        false
    }
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
    check_start_and_end(d, &starts, c);
    if let Some(&start) = starts.first() {
        check_reachable(d, start, edges, loose, c);
    }
    check_cycles(d, edges, c);
}

/// Exactly one `start` node and at least one `end` node.
fn check_start_and_end(d: &Draft, starts: &[usize], c: &mut Collector) {
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
}

/// Every node with an id is reachable from `start`, through resolved or loose edges.
fn check_reachable(
    d: &Draft,
    start: usize,
    edges: &[Resolved],
    loose: &[(usize, usize)],
    c: &mut Collector,
) {
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); d.nodes.len()];
    let pairs = edges
        .iter()
        .map(|e| (e.from, e.to))
        .chain(loose.iter().copied());
    for (from, to) in pairs {
        adj[from].push(to);
    }
    let seen = reachable(&adj, start);
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

/// Which vertices a walk from `start` reaches.
fn reachable(adj: &[Vec<usize>], start: usize) -> Vec<bool> {
    let mut seen = vec![false; adj.len()];
    let mut stack = vec![start];
    seen[start] = true;
    while let Some(v) = stack.pop() {
        for &w in &adj[v] {
            if !seen[w] {
                seen[w] = true;
                stack.push(w);
            }
        }
    }
    seen
}

/// Spec 2 says every cycle passes through a loop node. A cycle that only passes a loop
/// through `exhausted` would run forever, since `exhausted` can be taken any number of
/// times; so the rule enforced is: without the `repeat` edges, the graph has no cycle.
fn check_cycles(d: &Draft, edges: &[Resolved], c: &mut Collector) {
    let unbounded = unbounded_adjacency(d, edges);
    let self_loops: HashSet<usize> = edges
        .iter()
        .filter(|e| e.from == e.to && unbounded[e.from].contains(&e.to))
        .map(|e| e.from)
        .collect();
    for scc in strongly_connected(&unbounded) {
        if scc.len() >= 2 || scc.first().is_some_and(|v| self_loops.contains(v)) {
            report_cycle(d, scc, c);
        }
    }
}

/// The edges that do not leave a loop through `repeat`. A node of unknown type might be a
/// loop, so its edges are left out too.
fn unbounded_adjacency(d: &Draft, edges: &[Resolved]) -> Vec<Vec<usize>> {
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); d.nodes.len()];
    for e in edges {
        let bounded = match &d.nodes[e.from].kind {
            None => true,
            Some(NodeKind::Loop { .. }) => e.port == "repeat",
            Some(_) => false,
        };
        if !bounded {
            adj[e.from].push(e.to);
        }
    }
    adj
}

fn report_cycle(d: &Draft, mut scc: Vec<usize>, c: &mut Collector) {
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

/// Tarjan's algorithm with an explicit stack, so a large file cannot overflow the call stack.
fn strongly_connected(adj: &[Vec<usize>]) -> Vec<Vec<usize>> {
    let n = adj.len();
    let mut t = Tarjan {
        index: vec![None; n],
        low: vec![0; n],
        on_stack: vec![false; n],
        stack: Vec::new(),
        next: 0,
        out: Vec::new(),
    };
    for s in 0..n {
        if t.index[s].is_none() {
            t.walk(adj, s);
        }
    }
    t.out
}

struct Tarjan {
    index: Vec<Option<usize>>,
    low: Vec<usize>,
    on_stack: Vec<bool>,
    stack: Vec<usize>,
    next: usize,
    out: Vec<Vec<usize>>,
}

impl Tarjan {
    /// Visits every vertex reachable from `s` not visited yet; `call` holds each open
    /// vertex with the index of its next edge.
    fn walk(&mut self, adj: &[Vec<usize>], s: usize) {
        let mut call: Vec<(usize, usize)> = vec![(s, 0)];
        self.open(s);
        while let Some(&mut (v, ref mut i)) = call.last_mut() {
            if let Some(&w) = adj[v].get(*i) {
                *i += 1;
                match self.index[w] {
                    None => {
                        self.open(w);
                        call.push((w, 0));
                    }
                    Some(iw) if self.on_stack[w] => self.low[v] = self.low[v].min(iw),
                    Some(_) => {}
                }
                continue;
            }
            call.pop();
            if let Some(&(u, _)) = call.last() {
                self.low[u] = self.low[u].min(self.low[v]);
            }
            self.close(v);
        }
    }

    fn open(&mut self, v: usize) {
        self.index[v] = Some(self.next);
        self.low[v] = self.next;
        self.next += 1;
        self.stack.push(v);
        self.on_stack[v] = true;
    }

    /// When `v` roots a component, pops it off the stack.
    fn close(&mut self, v: usize) {
        if Some(self.low[v]) != self.index[v] {
            return;
        }
        let mut scc = Vec::new();
        while let Some(w) = self.stack.pop() {
            self.on_stack[w] = false;
            scc.push(w);
            if w == v {
                break;
            }
        }
        self.out.push(scc);
    }
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
