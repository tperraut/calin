//! YAML to [`Draft`]: shapes, types, required and unknown keys (spec sections 2 and 3).
//! Value rules (ranges, ids, the graph) are in [`crate::validate`], shared with the JSON path.

use std::collections::{HashMap, HashSet};

use crate::model::{AgentNode, Branch, CheckNode, EndStatus, NodeKind, NodeType, Workflow};
use crate::problem::{Collector, Problems, Subject};
use crate::raw::{self, Raw, RawValue};
use crate::template::Template;
use crate::validate::{
    Draft, DraftEdge, DraftNode, MAX_LOOP, MAX_TIMEOUT_SECONDS, is_valid_id, validate,
};

/// Default `check.timeout_seconds` (spec section 3).
pub const DEFAULT_TIMEOUT_SECONDS: u64 = 600;

const TOP_KEYS: [&str; 6] = [
    "version",
    "name",
    "description",
    "budget_usd",
    "nodes",
    "edges",
];

/// Parses and validates a workflow file. `id` is the file name without extension.
///
/// Every problem is collected in one pass and returned together, sorted by line. Besides the
/// spec's rules, the id itself must follow the node id rule and must not be `quick`, which is
/// the built-in workflow's.
pub fn parse(id: &str, yaml: &str) -> Result<Workflow, Problems> {
    let mut c = Collector::default();
    if !is_valid_id(id) {
        c.push(
            None,
            Subject::Workflow,
            format!(
                "workflow id `{id}` (the file name) must match `[a-z][a-z0-9-]*` and be at most 40 characters"
            ),
        );
    } else if id == "quick" {
        c.push(
            None,
            Subject::Workflow,
            "workflow id `quick` is reserved for the built-in workflow; rename the file",
        );
    }
    let doc = match raw::read(yaml) {
        Ok(doc) => doc,
        Err((message, line)) => {
            c.push(line, Subject::Workflow, message);
            return Err(c.finish());
        }
    };
    let draft = read_document(id, &doc, &mut c);
    let workflow = draft.and_then(|d| validate(d, &mut c));
    match workflow {
        Some(w) if c.is_empty() => Ok(w),
        _ => Err(c.finish()),
    }
}

/// A mapping's string-keyed entries, with the line of each key.
struct MapView<'a> {
    entries: Vec<(&'a str, Option<u32>, &'a Raw)>,
}

impl<'a> MapView<'a> {
    fn new(raw: &'a Raw, what: &str, subject: &Subject, c: &mut Collector) -> Option<Self> {
        let RawValue::Map(map) = &raw.value else {
            c.push(
                raw.line,
                subject.clone(),
                format!("{what} must be a mapping, found {}", raw.value.kind()),
            );
            return None;
        };
        let mut entries = Vec::with_capacity(map.len());
        for (k, v) in map {
            match &k.value {
                RawValue::Str(s) => entries.push((s.as_str(), k.line.or(v.line), v)),
                other => c.push(
                    k.line,
                    subject.clone(),
                    format!("{what}: keys must be strings, found {}", other.kind()),
                ),
            }
        }
        Some(MapView { entries })
    }

    fn get(&self, key: &str) -> Option<&'a Raw> {
        self.entries.iter().find(|e| e.0 == key).map(|e| e.2)
    }

    fn line(&self, key: &str) -> Option<u32> {
        self.entries.iter().find(|e| e.0 == key).and_then(|e| e.1)
    }

    fn reject_unknown(
        &self,
        allowed: &[&str],
        what: &str,
        context: &str,
        subject: &Subject,
        c: &mut Collector,
    ) {
        for (key, line, _) in &self.entries {
            if !allowed.contains(key) {
                c.push(
                    *line,
                    subject.clone(),
                    format!(
                        "{what}: unknown key `{key}`{context}; allowed keys are {}",
                        allowed.join(", ")
                    ),
                );
            }
        }
    }
}

/// Reads typed values out of one mapping, reporting under one name and subject.
struct Reader<'a, 'c> {
    view: MapView<'a>,
    what: String,
    subject: Subject,
    c: &'c mut Collector,
    /// Keys whose problem was reported here, so validation does not report them again.
    reported: HashSet<String>,
    /// Key prefix for `reported` and line keys (`branches.0.`).
    prefix: String,
    /// The mapping's own line, for problems about a missing key.
    line: Option<u32>,
    /// Lines of nested entries (branches), merged into [`Reader::lines`].
    extra_lines: HashMap<String, u32>,
}

impl<'a> Reader<'a, '_> {
    fn fail(&mut self, key: &str, line: Option<u32>, message: String) {
        self.reported.insert(format!("{}{key}", self.prefix));
        self.c.push(line, self.subject.clone(), message);
    }

    fn missing(&mut self, key: &str) {
        let line = self.line;
        let msg = format!("{}: missing required key `{key}`", self.what);
        self.fail(key, line, msg);
    }

    fn wrong(&mut self, key: &str, expected: &str, raw: &Raw) {
        let line = self.view.line(key).or(raw.line);
        let msg = format!(
            "{}: `{key}` must be {expected}, found {}",
            self.what,
            raw.value.kind()
        );
        self.fail(key, line, msg);
    }

    fn string(&mut self, key: &str) -> Option<String> {
        let raw = self.view.get(key)?;
        match &raw.value {
            RawValue::Str(s) => Some(s.clone()),
            _ => {
                self.wrong(key, "a string", raw);
                None
            }
        }
    }

    fn required_string(&mut self, key: &str) -> Option<String> {
        if self.view.get(key).is_none() {
            self.missing(key);
        }
        self.string(key)
    }

    fn template(&mut self, key: &str) -> Option<Template> {
        let source = self.string(key)?;
        match Template::parse(&source) {
            Ok(t) => Some(t),
            Err(e) => {
                let line = self.view.line(key);
                let msg = format!("{}: `{key}`: {e}", self.what);
                self.fail(key, line, msg);
                None
            }
        }
    }

    /// A required template; a placeholder when missing or broken (already reported).
    fn required_template(&mut self, key: &str) -> Template {
        if self.view.get(key).is_none() {
            self.missing(key);
        }
        self.template(key).unwrap_or_else(placeholder)
    }

    fn boolean(&mut self, key: &str) -> Option<bool> {
        let raw = self.view.get(key)?;
        match raw.value {
            RawValue::Bool(b) => Some(b),
            _ => {
                self.wrong(key, "`true` or `false`", raw);
                None
            }
        }
    }

    /// An integer in `range`; out-of-range values are reported here with the range.
    fn integer(&mut self, key: &str, lo: i128, hi: i128) -> Option<i128> {
        let raw = self.view.get(key)?;
        let range = format!("an integer from {lo} to {hi}");
        match raw.value {
            RawValue::Int(v) if (lo..=hi).contains(&v) => Some(v),
            RawValue::Int(v) => {
                let line = self.view.line(key).or(raw.line);
                let msg = format!(
                    "{}: `{key}` must be from {lo} to {hi}, found {v}",
                    self.what
                );
                self.fail(key, line, msg);
                None
            }
            _ => {
                self.wrong(key, &range, raw);
                None
            }
        }
    }

    fn number(&mut self, key: &str) -> Option<f64> {
        let raw = self.view.get(key)?;
        match raw.value {
            // Precision loss only beyond 2^53 dollars.
            RawValue::Int(v) => Some(v as f64),
            RawValue::Float(v) => Some(v),
            _ => {
                self.wrong(key, "a number", raw);
                None
            }
        }
    }

    fn string_list(&mut self, key: &str) -> Option<Vec<String>> {
        let raw = self.view.get(key)?;
        let RawValue::Seq(items) = &raw.value else {
            self.wrong(key, "a list of strings", raw);
            return None;
        };
        let mut out = Vec::with_capacity(items.len());
        let mut ok = true;
        for item in items {
            match &item.value {
                RawValue::Str(s) => out.push(s.clone()),
                other => {
                    ok = false;
                    let msg = format!(
                        "{}: every entry of `{key}` must be a string, found {}",
                        self.what,
                        other.kind()
                    );
                    self.fail(key, item.line, msg);
                }
            }
        }
        ok.then_some(out)
    }

    /// Lines of every key, prefixed, for validation's messages.
    fn lines(&self, into: &mut HashMap<String, u32>) {
        into.extend(self.extra_lines.iter().map(|(k, v)| (k.clone(), *v)));
        for (key, line, _) in &self.view.entries {
            if let Some(line) = line {
                into.insert(format!("{}{key}", self.prefix), *line);
            }
        }
    }
}

fn placeholder() -> Template {
    Template::from_parts("", Vec::new())
}

fn read_document(id: &str, doc: &Raw, c: &mut Collector) -> Option<Draft> {
    if doc.value == RawValue::Null {
        c.push(
            None,
            Subject::Workflow,
            "the file is empty; expected a mapping with `version`, `name`, `nodes` and `edges`",
        );
        return None;
    }
    let view = MapView::new(doc, "the document", &Subject::Workflow, c)?;
    view.reject_unknown(&TOP_KEYS, "the document", "", &Subject::Workflow, c);
    let mut r = Reader {
        view,
        what: "the document".to_owned(),
        subject: Subject::Workflow,
        c,
        reported: HashSet::new(),
        prefix: String::new(),
        line: doc.line,
        extra_lines: HashMap::new(),
    };
    if r.view.get("version").is_none() {
        r.missing("version");
    }
    let version = r.integer("version", i128::MIN, i128::MAX);
    let name = r.required_string("name");
    let description = r.string("description");
    let budget_usd = r.number("budget_usd");
    let mut lines = HashMap::new();
    r.lines(&mut lines);

    let nodes = match r.view.get("nodes") {
        None => {
            r.missing("nodes");
            Vec::new()
        }
        Some(Raw {
            value: RawValue::Seq(items),
            ..
        }) => items
            .iter()
            .enumerate()
            .map(|(i, item)| read_node(i, item, r.c))
            .collect(),
        Some(other) => {
            r.wrong("nodes", "a list", other);
            Vec::new()
        }
    };
    let edges = match r.view.get("edges") {
        None => {
            r.missing("edges");
            Vec::new()
        }
        Some(Raw {
            value: RawValue::Seq(items),
            ..
        }) => items
            .iter()
            .enumerate()
            .map(|(k, item)| read_edge(k, item, r.c))
            .collect(),
        Some(other) => {
            r.wrong("edges", "a list", other);
            Vec::new()
        }
    };
    Some(Draft {
        id: id.to_owned(),
        version,
        name,
        description,
        budget_usd,
        lines,
        nodes,
        edges,
    })
}

fn read_node(i: usize, raw: &Raw, c: &mut Collector) -> DraftNode {
    let fallback = format!("node #{}", i + 1);
    let mut node = DraftNode {
        id: None,
        kind: None,
        line: raw.line,
        lines: HashMap::new(),
        reported: HashSet::new(),
    };
    let subject = Subject::Node {
        id: format!("#{}", i + 1),
    };
    let Some(view) = MapView::new(raw, &fallback, &subject, c) else {
        return node;
    };
    let id = match view.get("id").map(|r| &r.value) {
        Some(RawValue::Str(s)) => Some(s.clone()),
        _ => None,
    };
    let (what, subject) = match &id {
        Some(id) => (format!("node `{id}`"), Subject::Node { id: id.clone() }),
        None => (fallback, subject),
    };
    let mut r = Reader {
        view,
        what,
        subject,
        c,
        reported: HashSet::new(),
        prefix: String::new(),
        line: raw.line,
        extra_lines: HashMap::new(),
    };
    if id.is_none() {
        // Reports "missing" or "must be a string".
        let _ = r.required_string("id");
    }
    node.id = id;

    let type_name = r.required_string("type");
    let node_type = type_name.as_deref().and_then(|t| {
        let found = NodeType::from_name(t);
        if found.is_none() {
            let line = r.view.line("type");
            let all: Vec<&str> = NodeType::ALL.iter().map(|t| t.as_str()).collect();
            let msg = format!(
                "{}: unknown type `{t}`; expected one of {}",
                r.what,
                all.join(", ")
            );
            r.fail("type", line, msg);
        }
        found
    });
    if let Some(t) = node_type {
        let mut allowed = vec!["id", "type"];
        allowed.extend_from_slice(t.fields());
        let context = format!(" for type `{}`", t.as_str());
        r.view
            .reject_unknown(&allowed, &r.what, &context, &r.subject, r.c);
        node.kind = Some(read_kind(t, &mut r));
    }
    r.lines(&mut node.lines);
    node.reported = r.reported;
    node
}

fn read_kind(t: NodeType, r: &mut Reader) -> NodeKind {
    match t {
        NodeType::Start => NodeKind::Start,
        NodeType::End => {
            let status = match r.string("status").as_deref() {
                None => EndStatus::Success,
                Some("success") => EndStatus::Success,
                Some("failed") => EndStatus::Failed,
                Some(other) => {
                    let line = r.view.line("status");
                    let msg = format!(
                        "{}: `status` must be `success` or `failed`, found `{other}`",
                        r.what
                    );
                    r.fail("status", line, msg);
                    EndStatus::Success
                }
            };
            NodeKind::End { status }
        }
        NodeType::Agent => {
            let prompt = r.required_template("prompt");
            let fresh = r.boolean("fresh").unwrap_or(false);
            let verdicts = r.string_list("verdicts");
            if verdicts.as_ref().is_some_and(Vec::is_empty) {
                let line = r.view.line("verdicts");
                let msg = format!(
                    "{}: `verdicts` must not be an empty list; leave it out for `done`/`failed` ports",
                    r.what
                );
                r.fail("verdicts", line, msg);
            }
            NodeKind::Agent(AgentNode {
                prompt,
                fresh,
                verdicts: verdicts.unwrap_or_default(),
                runner: r.string("runner"),
                model: r.string("model"),
            })
        }
        NodeType::Check => {
            let command = r.template("command");
            let repo = r.string("repo");
            let timeout = r
                .integer("timeout_seconds", 1, MAX_TIMEOUT_SECONDS.into())
                .and_then(|v| u64::try_from(v).ok())
                .unwrap_or(DEFAULT_TIMEOUT_SECONDS);
            NodeKind::Check(CheckNode {
                command,
                repo,
                timeout_seconds: timeout,
            })
        }
        NodeType::Loop => {
            if r.view.get("max").is_none() {
                r.missing("max");
            }
            let max = r
                .integer("max", 1, MAX_LOOP.into())
                .and_then(|v| u32::try_from(v).ok())
                .unwrap_or(1);
            NodeKind::Loop { max }
        }
        NodeType::Gate => NodeKind::Gate {
            message: r.required_template("message"),
        },
        NodeType::Commit => NodeKind::Commit {
            message: r.required_template("message"),
        },
        NodeType::Ask => NodeKind::Ask {
            question: r.required_template("question"),
            options: r.string_list("options"),
        },
        NodeType::Ship => NodeKind::Ship {
            title: r.template("title"),
            draft: r.boolean("draft").unwrap_or(true),
        },
        NodeType::Parallel => {
            let branches = match r.view.get("branches") {
                None => {
                    r.missing("branches");
                    Vec::new()
                }
                Some(Raw {
                    value: RawValue::Seq(items),
                    ..
                }) => {
                    let mut out = Vec::with_capacity(items.len());
                    for (j, item) in items.iter().enumerate() {
                        out.push(read_branch(j, item, r));
                    }
                    out
                }
                Some(other) => {
                    r.wrong("branches", "a list", other);
                    Vec::new()
                }
            };
            NodeKind::Parallel { branches }
        }
    }
}

fn read_branch(j: usize, raw: &Raw, parent: &mut Reader) -> Branch {
    let prefix = format!("branches.{j}.");
    if let Some(line) = raw.line {
        parent.extra_lines.insert(format!("branches.{j}"), line);
    }
    let fallback = format!("{}: branch #{}", parent.what, j + 1);
    let Some(view) = MapView::new(raw, &fallback, &parent.subject, parent.c) else {
        parent.reported.insert(format!("{prefix}id"));
        return Branch {
            id: String::new(),
            prompt: placeholder(),
            repos: None,
        };
    };
    let id = match view.get("id").map(|r| &r.value) {
        Some(RawValue::Str(s)) => Some(s.clone()),
        _ => None,
    };
    let what = match &id {
        Some(id) => format!("{}: branch `{id}`", parent.what),
        None => fallback,
    };
    view.reject_unknown(
        &["id", "prompt", "repos"],
        &what,
        "",
        &parent.subject,
        parent.c,
    );
    let mut r = Reader {
        view,
        what,
        subject: parent.subject.clone(),
        c: parent.c,
        reported: HashSet::new(),
        prefix,
        line: raw.line,
        extra_lines: HashMap::new(),
    };
    if id.is_none() {
        let _ = r.required_string("id");
    }
    let prompt = r.required_template("prompt");
    let repos = r.string_list("repos");
    let mut lines = HashMap::new();
    r.lines(&mut lines);
    let reported = std::mem::take(&mut r.reported);
    parent.reported.extend(reported);
    parent.extra_lines.extend(lines);
    Branch {
        id: id.unwrap_or_default(),
        prompt,
        repos,
    }
}

fn read_edge(k: usize, raw: &Raw, c: &mut Collector) -> DraftEdge {
    let subject = Subject::Edge { index: k };
    let fallback = format!("edge #{}", k + 1);
    let mut edge = DraftEdge {
        from: None,
        to: None,
        line: raw.line,
    };
    let Some(view) = MapView::new(raw, &fallback, &subject, c) else {
        return edge;
    };
    let text = |key: &str| match view.get(key).map(|r| &r.value) {
        Some(RawValue::Str(s)) => Some(s.clone()),
        _ => None,
    };
    let what = match (text("from"), text("to")) {
        (Some(f), Some(t)) => format!("edge `{f} -> {t}`"),
        _ => fallback,
    };
    view.reject_unknown(&["from", "to"], &what, "", &subject, c);
    let mut r = Reader {
        view,
        what,
        subject,
        c,
        reported: HashSet::new(),
        prefix: String::new(),
        line: raw.line,
        extra_lines: HashMap::new(),
    };
    edge.from = r.required_string("from");
    edge.to = r.required_string("to");
    edge
}
