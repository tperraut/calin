//! How Claude Code's tools look in Calin's vocabulary: what a call does, the
//! one line that describes it, and the files it touches.

use std::path::{Component, Path, PathBuf};

use calin_proto::{PlanEntry, PlanStatus, ToolKind};
use serde_json::Value;

/// The tool that carries the agent's plan as its input. CLI 2.1.332 does not
/// ship it, so its shape here is the documented one, not a recorded one.
pub(crate) const PLAN_TOOL: &str = "TodoWrite";

/// Tool output kept on an item. More than a transcript can usefully show.
pub(crate) const OUTPUT_CAP: usize = 64 * 1024;

/// A title is one line; a command can be a page.
const TITLE_CHARS: usize = 200;

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ToolView {
    pub kind: ToolKind,
    pub title: String,
    /// Absolute paths of the files the call reads or writes.
    pub locations: Vec<String>,
}

/// Describes a call of `name` with `input`, made by an agent working in `cwd`.
pub(crate) fn describe(name: &str, input: &Value, cwd: &Path) -> ToolView {
    let text = |key: &str| {
        input
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
    };
    let (kind, title, file) = match name {
        "Read" => (ToolKind::Read, None, text("file_path")),
        "Edit" | "Write" | "MultiEdit" => (ToolKind::Edit, None, text("file_path")),
        "NotebookEdit" => (
            ToolKind::Edit,
            None,
            text("notebook_path").or_else(|| text("file_path")),
        ),
        "Bash" => (ToolKind::Execute, text("command"), None),
        "Grep" | "Glob" => (ToolKind::Search, text("pattern"), None),
        "WebFetch" => (ToolKind::Fetch, text("url"), None),
        "WebSearch" => (ToolKind::Fetch, text("query"), None),
        "Task" | "Agent" => (
            ToolKind::Task,
            text("description").or_else(|| text("subagent_type")),
            None,
        ),
        _ => (ToolKind::Other, None, None),
    };
    let file = file.map(|path| absolute(path, cwd));
    let title = match (&file, title) {
        (Some(path), _) => relative(path, cwd),
        (None, Some(title)) => one_line(title),
        (None, None) => name.to_owned(),
    };
    ToolView {
        kind,
        title,
        locations: file
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect(),
    }
}

/// The plan a `TodoWrite` call carries, or `None` when its input has no list.
pub(crate) fn plan_entries(input: &Value) -> Option<Vec<PlanEntry>> {
    let todos = input.get("todos")?.as_array()?;
    Some(
        todos
            .iter()
            .filter_map(|todo| {
                let content = todo.get("content")?.as_str()?.to_owned();
                let status = match todo.get("status").and_then(Value::as_str) {
                    Some("in_progress") => PlanStatus::InProgress,
                    Some("completed") => PlanStatus::Completed,
                    _ => PlanStatus::Pending,
                };
                Some(PlanEntry { content, status })
            })
            .collect(),
    )
}

/// The text of a `tool_result`, whose content is a string or a list of blocks.
pub(crate) fn result_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|block| match block {
                Value::String(text) => Some(text.clone()),
                _ => match block.get("type").and_then(Value::as_str) {
                    Some("text") => block.get("text").and_then(Value::as_str).map(str::to_owned),
                    Some("image") => Some("[image]".to_owned()),
                    _ => None,
                },
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Cuts `text` to [`OUTPUT_CAP`] bytes and says so when it had to.
pub(crate) fn capped(text: String) -> String {
    if text.len() <= OUTPUT_CAP {
        return text;
    }
    let mut end = OUTPUT_CAP;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n[output truncated: first {end} of {} bytes]",
        &text[..end],
        text.len()
    )
}

fn one_line(text: &str) -> String {
    let line = text.trim().lines().next().unwrap_or_default().trim_end();
    let mut title: String = line.chars().take(TITLE_CHARS).collect();
    if line.chars().count() > TITLE_CHARS {
        title.push('…');
    }
    title
}

/// `path` as the agent meant it: relative paths are relative to `cwd`.
/// Resolved by name only, so a file that does not exist yet still has a location.
fn absolute(path: &str, cwd: &Path) -> PathBuf {
    let mut resolved = PathBuf::new();
    for component in cwd.join(path).components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                resolved.pop();
            }
            other => resolved.push(other),
        }
    }
    resolved
}

/// The path as a title: inside `cwd` without the prefix, elsewhere in full.
fn relative(path: &Path, cwd: &Path) -> String {
    match path.strip_prefix(cwd) {
        Ok(inside) if !inside.as_os_str().is_empty() => inside.to_string_lossy().into_owned(),
        _ => path.to_string_lossy().into_owned(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn view(name: &str, input: Value) -> (ToolKind, String, Vec<String>) {
        let view = describe(name, &input, Path::new("/work"));
        (view.kind, view.title, view.locations)
    }

    fn at(path: &str) -> Vec<String> {
        vec![path.to_owned()]
    }

    #[test]
    fn file_tools_are_titled_by_their_path_inside_the_working_directory() {
        assert_eq!(
            view("Read", json!({ "file_path": "/work/src/lib.rs" })),
            (ToolKind::Read, "src/lib.rs".into(), at("/work/src/lib.rs"))
        );
        assert_eq!(
            view(
                "Edit",
                json!({ "file_path": "/work/a.txt", "old_string": "a" })
            ),
            (ToolKind::Edit, "a.txt".into(), at("/work/a.txt"))
        );
        assert_eq!(
            view("Write", json!({ "file_path": "made.txt", "content": "x" })),
            (ToolKind::Edit, "made.txt".into(), at("/work/made.txt"))
        );
        assert_eq!(
            view("NotebookEdit", json!({ "notebook_path": "/work/n.ipynb" })),
            (ToolKind::Edit, "n.ipynb".into(), at("/work/n.ipynb"))
        );
    }

    #[test]
    fn paths_outside_the_working_directory_stay_absolute() {
        assert_eq!(
            view("Read", json!({ "file_path": "/etc/hosts" })),
            (ToolKind::Read, "/etc/hosts".into(), at("/etc/hosts"))
        );
        // A sibling whose name merely starts like the working directory is outside it.
        assert_eq!(
            view("Read", json!({ "file_path": "/workshop/a" })).1,
            "/workshop/a"
        );
        assert_eq!(
            view("Read", json!({ "file_path": "./sub/../../other/x" })),
            (ToolKind::Read, "/other/x".into(), at("/other/x"))
        );
    }

    #[test]
    fn commands_searches_fetches_and_tasks_are_titled_by_their_subject() {
        assert_eq!(
            view("Bash", json!({ "command": "cargo test -q\necho done" })),
            (ToolKind::Execute, "cargo test -q".into(), vec![])
        );
        assert_eq!(
            view("Grep", json!({ "pattern": "fn main", "path": "/work" })),
            (ToolKind::Search, "fn main".into(), vec![])
        );
        assert_eq!(view("Glob", json!({ "pattern": "**/*.rs" })).1, "**/*.rs");
        assert_eq!(
            view(
                "WebFetch",
                json!({ "url": "https://example.com", "prompt": "p" })
            ),
            (ToolKind::Fetch, "https://example.com".into(), vec![])
        );
        assert_eq!(
            view("WebSearch", json!({ "query": "rust 2024" })),
            (ToolKind::Fetch, "rust 2024".into(), vec![])
        );
        for name in ["Task", "Agent"] {
            assert_eq!(
                view(
                    name,
                    json!({ "description": "Review the diff", "prompt": "..." })
                ),
                (ToolKind::Task, "Review the diff".into(), vec![])
            );
        }
    }

    #[test]
    fn unknown_tools_and_missing_inputs_fall_back_to_the_tool_name() {
        assert_eq!(
            view("mcp__calin__dispatch", json!({ "brief": "x" })),
            (ToolKind::Other, "mcp__calin__dispatch".into(), vec![])
        );
        assert_eq!(view("TodoWrite", json!({})).0, ToolKind::Other);
        assert_eq!(
            view("Read", json!({})),
            (ToolKind::Read, "Read".into(), vec![])
        );
        assert_eq!(view("Bash", json!({ "command": "  " })).1, "Bash");
        assert_eq!(view("Bash", Value::Null).1, "Bash");
        assert_eq!(
            view("Edit", json!({ "file_path": 7 })).2,
            Vec::<String>::new()
        );
    }

    #[test]
    fn long_commands_are_cut_to_a_title() {
        let title = view("Bash", json!({ "command": "x".repeat(500) })).1;
        assert_eq!(title.chars().count(), TITLE_CHARS + 1);
        assert!(title.ends_with('…'));
    }

    #[test]
    fn plans_map_every_status() {
        let input = json!({ "todos": [
            { "content": "Read the notes", "status": "completed", "activeForm": "Reading" },
            { "content": "Fix the typo", "status": "in_progress", "activeForm": "Fixing" },
            { "content": "Summarize", "status": "pending", "activeForm": "Summarizing" },
            { "content": "Later", "status": "someday" },
            { "status": "pending" },
        ] });
        let entries = plan_entries(&input).unwrap();
        let seen: Vec<(&str, PlanStatus)> = entries
            .iter()
            .map(|entry| (entry.content.as_str(), entry.status))
            .collect();
        assert_eq!(
            seen,
            [
                ("Read the notes", PlanStatus::Completed),
                ("Fix the typo", PlanStatus::InProgress),
                ("Summarize", PlanStatus::Pending),
                ("Later", PlanStatus::Pending),
            ]
        );
        assert_eq!(plan_entries(&json!({ "todos": [] })), Some(vec![]));
        assert_eq!(plan_entries(&json!({ "todos": "none" })), None);
        assert_eq!(plan_entries(&json!({})), None);
    }

    #[test]
    fn results_are_read_from_strings_and_from_blocks() {
        assert_eq!(result_text(&json!("plain")), "plain");
        assert_eq!(
            result_text(&json!([
                { "type": "text", "text": "one" },
                { "type": "image", "source": { "data": "..." } },
                { "type": "text", "text": "two" },
                { "type": "tool_reference", "tool_name": "x" },
                "three",
            ])),
            "one\n[image]\ntwo\nthree"
        );
        assert_eq!(result_text(&Value::Null), "");
        assert_eq!(result_text(&json!({ "unexpected": true })), "");
    }

    #[test]
    fn long_output_is_capped_on_a_character_boundary() {
        assert_eq!(capped("short".into()), "short");
        let exact = "a".repeat(OUTPUT_CAP);
        assert_eq!(capped(exact.clone()), exact);

        // Two-byte characters, offset by one so the cap falls inside one of them.
        let long = format!("a{}", "é".repeat(OUTPUT_CAP));
        let cut = capped(long.clone());
        let (kept, note) = cut.split_once("\n[output truncated: ").unwrap();
        assert_eq!(kept.len(), OUTPUT_CAP - 1);
        assert!(long.starts_with(kept));
        assert_eq!(
            note,
            format!("first {} of {} bytes]", OUTPUT_CAP - 1, long.len())
        );
    }
}
