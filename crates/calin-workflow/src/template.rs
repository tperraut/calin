//! Templates (spec section 4).
//!
//! A template is split once, at load time, into literal text and placeholders. Rendering walks
//! the segments and appends values; it never looks at substituted text again, so an output that
//! itself contains `{{task}}` stays literal.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// What a session supplies to every template: `{{task}}`, `{{session.title}}`,
/// `{{session.branch}}`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Context {
    /// The session's prompt, `{{task}}`.
    pub task: String,
    /// `{{session.title}}`.
    pub session_title: String,
    /// `{{session.branch}}`.
    pub session_branch: String,
}

/// One piece of a parsed template.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Segment {
    /// Text copied as is.
    Literal {
        /// The text.
        text: String,
    },
    /// `{{task}}`.
    Task,
    /// `{{session.title}}`.
    SessionTitle,
    /// `{{session.branch}}`.
    SessionBranch,
    /// `{{nodes.<node>.<output>}}`.
    NodeOutput {
        /// The node id.
        node: String,
        /// The output name.
        output: String,
    },
}

/// A malformed template: a `{{` that does not open a known placeholder.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct TemplateError {
    /// What is wrong, quoting the offending text.
    pub message: String,
}

/// A parsed template. Serializes as its source text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Template {
    source: String,
    segments: Vec<Segment>,
}

impl Template {
    /// Parses `source`. Whitespace just inside the braces is ignored (`{{ task }}`); whitespace
    /// anywhere else in a placeholder makes it malformed.
    pub fn parse(source: &str) -> Result<Template, TemplateError> {
        let mut segments = Vec::new();
        let mut literal = String::new();
        let mut rest = source;
        while let Some(open) = rest.find("{{") {
            literal.push_str(&rest[..open]);
            let after = &rest[open + 2..];
            let Some(close) = after.find("}}") else {
                return Err(TemplateError {
                    message: format!("`{{{{` is never closed: `{}`", excerpt(&rest[open..])),
                });
            };
            let inner = &after[..close];
            let Some(segment) = placeholder(inner.trim()) else {
                return Err(TemplateError {
                    message: format!(
                        "`{{{{{inner}}}}}` is not a placeholder; expected `task`, `session.title`, \
                         `session.branch` or `nodes.<id>.<output>`"
                    ),
                });
            };
            if !literal.is_empty() {
                segments.push(Segment::Literal {
                    text: std::mem::take(&mut literal),
                });
            }
            segments.push(segment);
            rest = &after[close + 2..];
        }
        literal.push_str(rest);
        if !literal.is_empty() {
            segments.push(Segment::Literal { text: literal });
        }
        Ok(Template {
            source: source.to_owned(),
            segments,
        })
    }

    /// Builds a template whose parse is known to succeed, for built-in workflows.
    pub(crate) fn from_parts(source: &str, segments: Vec<Segment>) -> Template {
        Template {
            source: source.to_owned(),
            segments,
        }
    }

    /// The text as written in the file.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// The parsed pieces, in order.
    pub fn segments(&self) -> &[Segment] {
        &self.segments
    }

    /// Every `(node, output)` the template refers to, in order.
    pub fn references(&self) -> impl Iterator<Item = (&str, &str)> {
        self.segments.iter().filter_map(|s| match s {
            Segment::NodeOutput { node, output } => Some((node.as_str(), output.as_str())),
            _ => None,
        })
    }

    /// Renders with `context` and the outputs recorded so far. An output not recorded yet
    /// renders as the empty string.
    pub fn render(&self, context: &Context, outputs: &Outputs) -> String {
        let mut out = String::with_capacity(self.source.len());
        for segment in &self.segments {
            match segment {
                Segment::Literal { text } => out.push_str(text),
                Segment::Task => out.push_str(&context.task),
                Segment::SessionTitle => out.push_str(&context.session_title),
                Segment::SessionBranch => out.push_str(&context.session_branch),
                Segment::NodeOutput { node, output } => {
                    out.push_str(outputs.get(node, output).unwrap_or(""));
                }
            }
        }
        out
    }
}

impl fmt::Display for Template {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.source)
    }
}

impl Serialize for Template {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.source)
    }
}

impl<'de> Deserialize<'de> for Template {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let source = String::deserialize(d)?;
        Template::parse(&source).map_err(serde::de::Error::custom)
    }
}

fn placeholder(inner: &str) -> Option<Segment> {
    match inner {
        "task" => return Some(Segment::Task),
        "session.title" => return Some(Segment::SessionTitle),
        "session.branch" => return Some(Segment::SessionBranch),
        _ => {}
    }
    let mut parts = inner.split('.');
    let (Some("nodes"), Some(node), Some(output), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return None;
    };
    let word = |s: &str| !s.is_empty() && !s.contains(|c: char| c.is_whitespace() || c == '{');
    (word(node) && word(output)).then(|| Segment::NodeOutput {
        node: node.to_owned(),
        output: output.to_owned(),
    })
}

/// At most 40 characters of `s`, for quoting in a message.
fn excerpt(s: &str) -> String {
    let mut out: String = s.chars().take(40).collect();
    if out.len() < s.len() {
        out.push('…');
    }
    out
}

/// The outputs recorded so far in a run: per node, the values of its newest run.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Outputs(BTreeMap<String, BTreeMap<String, String>>);

impl Outputs {
    /// No outputs.
    pub fn new() -> Outputs {
        Outputs::default()
    }

    /// The newest value of `node`'s `output`, if that node has run and set it.
    pub fn get(&self, node: &str, output: &str) -> Option<&str> {
        self.0.get(node)?.get(output).map(String::as_str)
    }

    /// The outputs of `node`'s newest run.
    pub fn node(&self, node: &str) -> Option<&BTreeMap<String, String>> {
        self.0.get(node)
    }

    /// Records a run of `node`. Replaces everything the node's previous run recorded, so a
    /// value never comes from an older run than the others.
    pub fn record(&mut self, node: &str, values: BTreeMap<String, String>) {
        self.0.insert(node.to_owned(), values);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> Context {
        Context {
            task: "Fix the bug".into(),
            session_title: "Bug fix".into(),
            session_branch: "calin/bug-fix".into(),
        }
    }

    fn render(src: &str, outputs: &Outputs) -> String {
        Template::parse(src).unwrap().render(&ctx(), outputs)
    }

    #[test]
    fn each_placeholder_kind() {
        let mut outputs = Outputs::new();
        outputs.record(
            "tests",
            [("output".to_owned(), "1 failed".to_owned())].into(),
        );
        assert_eq!(render("{{task}}", &outputs), "Fix the bug");
        assert_eq!(render("t={{session.title}}", &outputs), "t=Bug fix");
        assert_eq!(render("{{session.branch}}!", &outputs), "calin/bug-fix!");
        assert_eq!(
            render("out: {{nodes.tests.output}}", &outputs),
            "out: 1 failed"
        );
        assert_eq!(render("no placeholders", &outputs), "no placeholders");
        assert_eq!(render("", &outputs), "");
        assert_eq!(
            render("{ single } braces }}", &outputs),
            "{ single } braces }}"
        );
    }

    #[test]
    fn whitespace_inside_braces() {
        let outputs = Outputs::new();
        assert_eq!(render("{{ task }}", &outputs), "Fix the bug");
        assert_eq!(render("{{\ttask\n}}", &outputs), "Fix the bug");
        assert_eq!(render("{{  session.branch}}", &outputs), "calin/bug-fix");
        let t = Template::parse("{{ nodes.a.b }}").unwrap();
        assert_eq!(t.references().collect::<Vec<_>>(), vec![("a", "b")]);
    }

    #[test]
    fn missing_output_is_empty() {
        assert_eq!(render("[{{nodes.tests.output}}]", &Outputs::new()), "[]");
    }

    #[test]
    fn malformed() {
        for src in [
            "{{",
            "hello {{task",
            "{{}}",
            "{{ }}",
            "{{tasks}}",
            "{{session}}",
            "{{session.id}}",
            "{{nodes.a}}",
            "{{nodes.a.b.c}}",
            "{{nodes..b}}",
            "{{nodes. a.b}}",
            "{{ta sk}}",
            "{{{task}}}",
            "{{ a {{task}}",
            "{{Task}}",
        ] {
            let err = Template::parse(src).unwrap_err();
            assert!(err.message.contains("{{"), "{src}: {err}");
        }
    }

    #[test]
    fn substituted_text_is_not_rescanned() {
        let mut outputs = Outputs::new();
        outputs.record(
            "a",
            [(
                "summary".to_owned(),
                "{{task}} and {{nodes.a.summary}}".to_owned(),
            )]
            .into(),
        );
        assert_eq!(
            render("{{nodes.a.summary}}", &outputs),
            "{{task}} and {{nodes.a.summary}}"
        );
        let c = Context {
            task: "{{session.title}}".into(),
            ..ctx()
        };
        let t = Template::parse("{{task}}").unwrap();
        assert_eq!(t.render(&c, &outputs), "{{session.title}}");
    }

    #[test]
    fn newest_value_wins() {
        let mut outputs = Outputs::new();
        outputs.record("a", [("summary".to_owned(), "first".to_owned())].into());
        outputs.record("a", [("summary".to_owned(), "second".to_owned())].into());
        assert_eq!(render("{{nodes.a.summary}}", &outputs), "second");
    }

    #[test]
    fn serializes_as_source() {
        let t = Template::parse("Hi {{ task }}").unwrap();
        let json = serde_json::to_string(&t).unwrap();
        assert_eq!(json, "\"Hi {{ task }}\"");
        let back: Template = serde_json::from_str(&json).unwrap();
        assert_eq!(back, t);
        assert!(serde_json::from_str::<Template>("\"{{oops}}\"").is_err());
    }
}
