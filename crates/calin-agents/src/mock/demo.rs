//! The mock's demo directives: words in a prompt (`demo: orchestrate`,
//! `demo: child`, `demo: note`, `demo: ask`) that make it use Calin's tool
//! endpoint the way a real agent would, so a dry run can show child
//! sessions, notes, reports and questions. Without a directive the mock
//! plays its fixed turn.

use calin_proto::{
    Item, ItemContent, ItemDelta, ItemStatus, PermissionDecision, PermissionMode,
    PermissionRequest, ToolKind,
};
use serde_json::{Value, json};

use super::tools::{self, Endpoint};
use super::{Cut, MockAgent, quote};
use crate::prose::Prose;
use crate::runner::{PermissionAsk, ToolAccess};

/// What Calin hands an agent, as `calin-core`'s briefing writes it.
const BRIEF: &str = "[Calin] Brief from the session that dispatched you";
const REPORT: &str = "[Calin] Report from child session \"";
const CHILD_NOTE: &str = "[Calin] Note from child session \"";
const ANSWER: &str = "[Calin] The user answered: ";

/// The prefix the real CLI gives the tools of the server named `calin`.
const TOOL_PREFIX: &str = "mcp__calin__";

/// How many words of the request name the children.
const TITLE_WORDS: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Directive {
    Orchestrate,
    Child,
    Note,
    Ask,
}

impl Directive {
    fn from_word(word: &str) -> Option<Self> {
        match word {
            "orchestrate" => Some(Self::Orchestrate),
            "child" => Some(Self::Child),
            "note" => Some(Self::Note),
            "ask" => Some(Self::Ask),
            _ => None,
        }
    }
}

/// What a turn plays instead of the fixed script.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Script {
    /// Dispatch two children and tell the user. With `note`, the second
    /// child is asked to send a note before it reports.
    Orchestrate {
        subject: String,
        note: bool,
    },
    /// Edit the notes file, optionally send a note to the parent, report.
    Child {
        subject: String,
        note: bool,
    },
    Ask {
        subject: String,
    },
    /// A child's report arrived.
    Report {
        title: String,
    },
    /// A child's note arrived.
    ChildNote {
        title: String,
    },
    /// The user answered the question.
    Answer {
        answer: String,
    },
}

/// One directive found in a text, and where.
struct Found {
    directive: Option<Directive>,
    start: usize,
    end: usize,
}

/// Every `demo:<word>` in `text`, matched case-insensitively and with any
/// spaces around the colon; `directive` is `None` for a word the mock does
/// not know.
fn scan(text: &str) -> Vec<Found> {
    // ASCII lowercasing keeps every byte where it was.
    let lower = text.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(at) = lower[from..].find("demo") {
        let start = from + at;
        from = start + "demo".len();
        // `demo` must start a word.
        if start > 0 && bytes[start - 1].is_ascii_alphanumeric() {
            continue;
        }
        let mut cursor = from;
        while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
            cursor += 1;
        }
        if bytes.get(cursor) != Some(&b':') {
            continue;
        }
        cursor += 1;
        while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
            cursor += 1;
        }
        let word_start = cursor;
        while bytes
            .get(cursor)
            .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b == b'-')
        {
            cursor += 1;
        }
        if cursor == word_start {
            continue;
        }
        found.push(Found {
            directive: Directive::from_word(&lower[word_start..cursor]),
            start,
            end: cursor,
        });
        from = cursor;
    }
    found
}

/// The directives in `text`, in order; unknown words are left out.
pub(super) fn directives(text: &str) -> Vec<Directive> {
    scan(text).into_iter().filter_map(|f| f.directive).collect()
}

/// `text` without its known directives, on one line.
fn without_directives(text: &str) -> String {
    let mut kept = String::new();
    let mut from = 0;
    for found in scan(text) {
        if found.directive.is_some() {
            kept.push_str(&text[from..found.start]);
            kept.push(' ');
            from = found.end;
        }
    }
    kept.push_str(&text[from..]);
    kept.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The first line of `text` that says something once the directives are
/// out, short enough to quote.
fn subject(text: &str) -> String {
    let line = text
        .lines()
        .map(without_directives)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    quote(&line)
}

/// What the turn on `text` plays, or `None` for the fixed turn.
///
/// What Calin hands the agent is recognized first: a report, a note or an
/// answer only ever reaches an agent that used the tools, and a brief's
/// header names the parent's title, which may hold directives of its own.
pub(super) fn script(text: &str) -> Option<Script> {
    if let Some(title) = quoted_after(text, REPORT) {
        return Some(Script::Report { title });
    }
    if let Some(title) = quoted_after(text, CHILD_NOTE) {
        return Some(Script::ChildNote { title });
    }
    if let Some(at) = text.find(ANSWER) {
        let answer = text[at + ANSWER.len()..].trim().to_owned();
        return Some(Script::Answer { answer });
    }
    let request = match text.find(BRIEF) {
        // The brief itself follows the header's paragraph.
        Some(at) => text[at..].split_once("\n\n").map_or("", |(_, brief)| brief),
        None => text,
    };
    let found = directives(request);
    let subject = subject(request);
    if found.contains(&Directive::Orchestrate) {
        let note = found.contains(&Directive::Note);
        return Some(Script::Orchestrate { subject, note });
    }
    found.first().map(|directive| match directive {
        Directive::Child | Directive::Orchestrate => Script::Child {
            subject,
            note: false,
        },
        Directive::Note => Script::Child {
            subject,
            note: true,
        },
        Directive::Ask => Script::Ask { subject },
    })
}

/// The text between the double quotes that follow `marker`.
fn quoted_after(text: &str, marker: &str) -> Option<String> {
    let at = text.find(marker)?;
    let rest = &text[at + marker.len()..];
    let end = rest.find("\" (").or_else(|| rest.find('"'))?;
    Some(rest[..end].to_owned())
}

/// The first few words of the request, to name the children by.
fn title_words(subject: &str) -> String {
    let words: Vec<&str> = subject.split_whitespace().take(TITLE_WORDS).collect();
    let words = words.join(" ");
    let words = words.trim_end_matches(|c: char| !c.is_alphanumeric());
    match words.is_empty() {
        true => "Demo".to_owned(),
        false => words.to_owned(),
    }
}

impl MockAgent {
    /// Plays a demo script. Each needs the tool endpoint, except to
    /// acknowledge what Calin delivered.
    pub(super) async fn demo(&mut self, script: Script) -> Result<(), Cut> {
        let needs_tools = matches!(
            script,
            Script::Orchestrate { .. } | Script::Child { .. } | Script::Ask { .. }
        );
        if needs_tools && let Err(reason) = &self.tools {
            let text = format!(
                "Calin's tool endpoint is unavailable ({reason}), so this demo cannot run."
            );
            return self.streamed(Prose::Text, &[&text]).await;
        }
        match script {
            Script::Orchestrate { subject, note } => self.orchestrate(&subject, note).await,
            Script::Child { subject, note } => self.child(&subject, note).await,
            Script::Ask { subject } => self.ask_user(&subject).await,
            Script::Report { title } => self.reported(title).await,
            Script::ChildNote { title } => {
                let text = format!("Noted the update from \"{title}\".");
                self.streamed(Prose::Text, &[&text]).await
            }
            Script::Answer { answer } => {
                self.streamed(
                    Prose::Text,
                    &["You answered: ", &answer, ". ", "I'll go on with that."],
                )
                .await
            }
        }
    }

    async fn orchestrate(&mut self, subject: &str, note: bool) -> Result<(), Cut> {
        self.streamed(
            Prose::Text,
            &[
                "I'll split \"",
                subject,
                "\" in two ",
                "and hand each part to a child session.",
            ],
        )
        .await?;
        let words = title_words(subject);
        let parts = [
            ("A", "demo: child"),
            ("B", if note { "demo: note" } else { "demo: child" }),
        ];
        let mut titles = Vec::new();
        for (part, directive) in parts {
            let title = format!("{words}: part {part}");
            let brief = format!(
                "Part {part} of \"{subject}\": add a line to the notes file, then report \
                 back.\n\n{directive}"
            );
            let arguments = json!({ "title": title, "brief": brief });
            let shown = format!("Dispatch {title}");
            if self
                .calin_tool("dispatch", shown, arguments)
                .await?
                .is_some()
            {
                titles.push(title);
            }
        }
        self.dispatched = titles.clone();
        self.reported.clear();
        let text = format!(
            "Both parts are under way: \"{}: part A\" and \"{}: part B\". Their reports will \
             come back here.",
            words, words
        );
        let arguments = json!({ "to": "user", "text": text });
        self.calin_tool("send_message", "Note to the user".to_owned(), arguments)
            .await?;
        Ok(())
    }

    async fn child(&mut self, subject: &str, note: bool) -> Result<(), Cut> {
        self.edit_notes(subject).await?;
        if note {
            let text = "Half way: the notes file has its new line. The report follows.";
            let arguments = json!({ "to": "parent", "text": text });
            self.calin_tool("send_message", "Note to the parent".to_owned(), arguments)
                .await?;
        }
        let summary = format!(
            "Added a line to calin-mock-notes.md; nothing is committed. The brief was: {subject}"
        );
        let arguments = json!({ "summary": summary });
        self.calin_tool("report", "Report to the parent".to_owned(), arguments)
            .await?;
        Ok(())
    }

    async fn ask_user(&mut self, subject: &str) -> Result<(), Cut> {
        self.streamed(
            Prose::Text,
            &["Before I go on, ", "I need a decision from you."],
        )
        .await?;
        let question = format!("Which way should I go for \"{subject}\"?");
        let arguments = json!({
            "question": question,
            "options": ["The simple way", "The thorough way"],
        });
        self.calin_tool("ask_user", "Ask the user".to_owned(), arguments)
            .await?;
        Ok(())
    }

    /// Acknowledges a report; the last one expected also gets a summary.
    async fn reported(&mut self, title: String) -> Result<(), Cut> {
        let text = format!("Got the report from \"{title}\".");
        self.streamed(Prose::Text, &[&text]).await?;
        self.reported.push(title);
        // After a restart the mock no longer knows what it dispatched; the
        // orchestrate script dispatches two.
        let expected = match self.dispatched.len() {
            0 => 2,
            count => count,
        };
        if self.reported.len() != expected {
            return Ok(());
        }
        let titles: Vec<String> = self
            .reported
            .iter()
            .map(|title| format!("\"{title}\""))
            .collect();
        let summary = format!(
            "All {} parts are done: {}. Each child's change is in its own session.",
            titles.len(),
            titles.join(" and ")
        );
        self.streamed(Prose::Text, &["Summary: ", &summary]).await
    }

    /// One call to a tool of Calin's, shown as a tool item the way the real
    /// CLI names it. `Some` with the result text when it succeeded; a call
    /// that failed or was declined completes as such and the script goes on.
    async fn calin_tool(
        &mut self,
        tool: &str,
        title: String,
        arguments: Value,
    ) -> Result<Option<String>, Cut> {
        let name = format!("{TOOL_PREFIX}{tool}");
        let id = self.next_item_id();
        self.start_item(Item {
            id: id.clone(),
            status: ItemStatus::Running,
            content: ItemContent::Tool {
                name: name.clone(),
                tool_kind: ToolKind::Other,
                title: title.clone(),
                input: arguments.clone(),
                output: None,
                locations: Vec::new(),
            },
        })
        .await?;
        // Like the CLI, it asks unless it may do anything; Calin's policy
        // allows its own tools at once.
        if self.permission_mode != PermissionMode::Autonomous {
            let request = PermissionAsk {
                request: PermissionRequest {
                    id: format!("{id}-permission"),
                    tool_name: name,
                    tool_kind: ToolKind::Other,
                    title,
                    input: arguments.clone(),
                    item_id: Some(id),
                },
                access: ToolAccess::Orchestrator,
                subagent: self.inside.clone(),
            };
            if let (PermissionDecision::Deny, message) = self.ask(request).await? {
                self.set_output(message);
                self.complete_item(ItemStatus::Declined).await?;
                return Ok(None);
            }
        }
        let outcome = match self.tools.clone() {
            Ok(endpoint) => self.request(endpoint, tool, arguments).await?,
            Err(reason) => Err(reason),
        };
        match outcome {
            Ok(text) => {
                self.delta(ItemDelta::Output { text: text.clone() }).await?;
                self.complete_item(ItemStatus::Completed).await?;
                Ok(Some(text))
            }
            Err(text) => {
                self.set_output(Some(text));
                self.complete_item(ItemStatus::Failed).await?;
                Ok(None)
            }
        }
    }

    /// The HTTP call, given up at once on an interrupt or a stop.
    async fn request(
        &mut self,
        endpoint: Endpoint,
        tool: &str,
        arguments: Value,
    ) -> Result<Result<String, String>, Cut> {
        self.rpc_ids += 1;
        let call = tools::call_tool(endpoint, self.rpc_ids, tool, arguments);
        tokio::pin!(call);
        loop {
            tokio::select! {
                outcome = &mut call => return Ok(outcome),
                input = self.io.inputs.recv() => match input {
                    Some(input) => self.cut_by(input)?,
                    None => return Err(Cut::Detached),
                },
            }
        }
    }

    /// Sets the output the open tool item completes with.
    fn set_output(&mut self, text: Option<String>) {
        if let Some(Item {
            content: ItemContent::Tool { output, .. },
            ..
        }) = &mut self.open
        {
            *output = text;
        }
    }
}
