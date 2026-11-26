//! Claude Code's stream-json protocol, as far as Calin reads and writes it.
//!
//! [`Mapper`] turns the CLI's output lines into [`RunnerOutput`]s and builds
//! the lines the runner writes back. It does no I/O, so recorded transcripts
//! replay through it unchanged. What is handled here was recorded from CLI
//! 2.1.332 (see `fixtures/claude/`), except where a comment says otherwise.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;

use calin_proto::{
    EventBody, Item, ItemContent, ItemDelta, ItemStatus, PermissionDecision, PermissionRequest,
    ToolKind,
};
use serde_json::{Value, json};

use super::tools;
use crate::runner::RunnerOutput;

/// Told to the agent with a denial that carries no message of its own.
const DEFAULT_DENIAL: &str = "The user denied this request.";

/// An error shown in the transcript is a line or two, not a dump.
const ERROR_CHARS: usize = 500;

/// Translates one CLI process's output. Create one per process.
#[derive(Debug)]
pub(crate) struct Mapper {
    /// Where the agent works: relative tool paths are relative to it.
    cwd: PathBuf,
    /// Namespace of the item ids minted here, unique per start within a session.
    id_prefix: String,
    minted: u64,
    interrupts: u64,
    /// The conversation id and model last announced.
    native: Option<(String, Option<String>)>,
    /// Streaming state per `parent_tool_use_id`: the lead agent's stream and
    /// each subagent's interleave on the same pipe. Ordered, so that closing
    /// them all is deterministic.
    scopes: BTreeMap<Option<String>, Scope>,
    /// Tool items that wait for their result, oldest first.
    running: Vec<ToolCall>,
    /// Every tool-use id that became an item, so none becomes one twice.
    started: HashSet<String>,
    /// Permission requests that wait for a decision, by request id.
    pending: HashMap<String, PendingPermission>,
    /// Tool-use ids whose request was denied through this mapper.
    denied: HashSet<String>,
    totals: Totals,
    /// The error already shown in this turn, so the result does not repeat it.
    turn_error: Option<String>,
}

#[derive(Debug, Default)]
struct Scope {
    /// The API message whose blocks are arriving as stream events. Complete
    /// `assistant` messages with this id repeat what was streamed.
    message_id: Option<String>,
    block: Option<Block>,
}

/// A text or thinking block being streamed.
#[derive(Debug)]
struct Block {
    index: Option<u64>,
    prose: Prose,
    item_id: String,
    text: String,
    /// Reasoning starts with its first text: the CLI usually sends none.
    started: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Prose {
    Text,
    Reasoning,
}

impl Prose {
    fn content(self, text: String) -> ItemContent {
        match self {
            Self::Text => ItemContent::Text { text },
            Self::Reasoning => ItemContent::Reasoning { text },
        }
    }

    fn delta(self, text: String) -> ItemDelta {
        match self {
            Self::Text => ItemDelta::Text { text },
            Self::Reasoning => ItemDelta::Reasoning { text },
        }
    }
}

#[derive(Debug)]
struct ToolCall {
    id: String,
    name: String,
    kind: ToolKind,
    title: String,
    input: Value,
    locations: Vec<String>,
}

impl ToolCall {
    fn item(&self, status: ItemStatus, output: Option<String>) -> Item {
        Item {
            id: self.id.clone(),
            status,
            content: ItemContent::Tool {
                name: self.name.clone(),
                tool_kind: self.kind,
                title: self.title.clone(),
                input: self.input.clone(),
                output,
                locations: self.locations.clone(),
            },
        }
    }
}

#[derive(Debug)]
struct PendingPermission {
    tool_use_id: Option<String>,
    /// Sent back as `updatedInput` on allow: the CLI runs what it is handed.
    input: Value,
}

#[derive(Debug, Default)]
struct Totals {
    cost_usd: f64,
    tokens_in: u64,
    tokens_out: u64,
}

impl Mapper {
    /// `id_prefix` must differ for every process of a session.
    pub(crate) fn new(cwd: PathBuf, id_prefix: String) -> Self {
        Self {
            cwd,
            id_prefix,
            minted: 0,
            interrupts: 0,
            native: None,
            scopes: BTreeMap::new(),
            running: Vec::new(),
            started: HashSet::new(),
            pending: HashMap::new(),
            denied: HashSet::new(),
            totals: Totals::default(),
            turn_error: None,
        }
    }

    /// One line of the CLI's stdout. A line that is not JSON is skipped.
    pub(crate) fn line(&mut self, line: &str) -> Vec<RunnerOutput> {
        let line = line.trim();
        if line.is_empty() {
            return Vec::new();
        }
        match serde_json::from_str::<Value>(line) {
            Ok(message) => self.message(&message),
            Err(err) => {
                tracing::debug!(%err, "skipped a line from claude that is not JSON");
                Vec::new()
            }
        }
    }

    /// One parsed message of the CLI's stdout. Unknown types yield nothing.
    pub(crate) fn message(&mut self, message: &Value) -> Vec<RunnerOutput> {
        let mut out = Vec::new();
        match str_field(message, "type") {
            Some("system") => self.system(message, &mut out),
            Some("stream_event") => self.stream_event(message, &mut out),
            Some("assistant") => self.assistant(message, &mut out),
            Some("user") => self.user(message, &mut out),
            Some("control_request") => self.control_request(message, &mut out),
            // The CLI withdrawing a request of its own. Part of the SDK
            // protocol; no recording holds one.
            Some("control_cancel_request") => {
                if let Some(request_id) = str_field(message, "request_id") {
                    self.pending.remove(request_id);
                }
            }
            Some("result") => self.result(message, &mut out),
            _ => {}
        }
        out
    }

    /// Completes whatever is still running, for a stream that ended without a
    /// `result`: the process died or was killed.
    pub(crate) fn finish(&mut self) -> Vec<RunnerOutput> {
        let mut out = Vec::new();
        self.close_all(&mut out);
        out
    }

    /// The stdin line that starts a turn.
    pub(crate) fn user_message(text: &str) -> Value {
        json!({
            "type": "user",
            "message": { "role": "user", "content": [{ "type": "text", "text": text }] },
        })
    }

    /// The stdin line that answers a permission request, or `None` when no
    /// request with this id is waiting.
    pub(crate) fn permission_response(
        &mut self,
        request_id: &str,
        decision: PermissionDecision,
        message: Option<String>,
    ) -> Option<Value> {
        let pending = self.pending.remove(request_id)?;
        let response = match decision {
            PermissionDecision::Allow => {
                json!({ "behavior": "allow", "updatedInput": pending.input })
            }
            PermissionDecision::Deny => {
                self.denied.extend(pending.tool_use_id);
                let message = message.filter(|text| !text.trim().is_empty());
                json!({
                    "behavior": "deny",
                    "message": message.as_deref().unwrap_or(DEFAULT_DENIAL),
                })
            }
        };
        Some(json!({
            "type": "control_response",
            "response": { "subtype": "success", "request_id": request_id, "response": response },
        }))
    }

    /// The stdin line that ends the current turn. The CLI answers with a
    /// `result` whose `terminal_reason` starts with `aborted`.
    pub(crate) fn interrupt_request(&mut self) -> Value {
        self.interrupts += 1;
        json!({
            "type": "control_request",
            "request_id": format!("calin-interrupt-{}", self.interrupts),
            "request": { "subtype": "interrupt" },
        })
    }

    fn system(&mut self, message: &Value, out: &mut Vec<RunnerOutput>) {
        // `init` opens every turn; only a change is news.
        if str_field(message, "subtype") != Some("init") {
            return;
        }
        let Some(id) = str_field(message, "session_id") else {
            return;
        };
        let native = (
            id.to_owned(),
            str_field(message, "model").map(str::to_owned),
        );
        if self.native.as_ref() != Some(&native) {
            out.push(RunnerOutput::NativeSession {
                id: native.0.clone(),
                model: native.1.clone(),
            });
            self.native = Some(native);
        }
    }

    fn stream_event(&mut self, message: &Value, out: &mut Vec<RunnerOutput>) {
        let Some(event) = message.get("event") else {
            return;
        };
        let parent = parent_of(message);
        let index = event.get("index").and_then(Value::as_u64);
        let message_id = event
            .get("message")
            .and_then(|inner| str_field(inner, "id"))
            .or_else(|| str_field(message, "api_message_id"));
        if let Some(id) = message_id
            && self
                .scopes
                .get(&parent)
                .and_then(|s| s.message_id.as_deref())
                != Some(id)
        {
            // Another message: whatever the previous one left open is over.
            self.close_block(&parent, out);
            self.scopes.entry(parent.clone()).or_default().message_id = Some(id.to_owned());
        }
        match str_field(event, "type") {
            Some("message_start") => self.close_block(&parent, out),
            Some("content_block_start") => {
                self.close_block(&parent, out);
                let block = event.get("content_block").unwrap_or(&Value::Null);
                let (prose, initial) = match str_field(block, "type") {
                    Some("text") => (Prose::Text, str_field(block, "text")),
                    Some("thinking") => (Prose::Reasoning, str_field(block, "thinking")),
                    // Tool calls become items from their complete message, which has the input.
                    _ => return,
                };
                let item_id = self.mint_id();
                let block = self.scopes.entry(parent).or_default().block.insert(Block {
                    index,
                    prose,
                    item_id,
                    text: String::new(),
                    started: false,
                });
                if prose == Prose::Text {
                    block.start(out);
                }
                block.append(initial.unwrap_or_default(), out);
            }
            Some("content_block_delta") => {
                let delta = event.get("delta").unwrap_or(&Value::Null);
                let (prose, chunk) = match str_field(delta, "type") {
                    Some("text_delta") => (Prose::Text, str_field(delta, "text")),
                    Some("thinking_delta") => (Prose::Reasoning, str_field(delta, "thinking")),
                    _ => return,
                };
                let open = self.scopes.get_mut(&parent).and_then(|s| s.block.as_mut());
                if let Some(block) = open.filter(|b| b.prose == prose && b.index == index) {
                    block.append(chunk.unwrap_or_default(), out);
                }
            }
            Some("content_block_stop") => {
                let open = self.scopes.get(&parent).and_then(|s| s.block.as_ref());
                if open.is_some_and(|block| block.index == index) {
                    self.close_block(&parent, out);
                }
            }
            Some("message_stop") => self.close_block(&parent, out),
            _ => {}
        }
    }

    fn assistant(&mut self, message: &Value, out: &mut Vec<RunnerOutput>) {
        let Some(inner) = message.get("message") else {
            return;
        };
        let blocks = inner.get("content").and_then(Value::as_array);
        let blocks = blocks.map(Vec::as_slice).unwrap_or_default();

        // The CLI words API failures as an assistant message. It is not the
        // model speaking, so it becomes an error instead of text.
        let synthetic = message.get("is_api_error_message").and_then(Value::as_bool) == Some(true)
            || str_field(message, "error").is_some();
        if synthetic {
            let text: Vec<&str> = blocks
                .iter()
                .filter_map(|block| str_field(block, "text"))
                .collect();
            self.error(&text.join("\n"), out);
            return;
        }

        let parent = parent_of(message);
        let streamed = {
            let id = str_field(inner, "id");
            let streaming = self
                .scopes
                .get(&parent)
                .and_then(|s| s.message_id.as_deref());
            id.is_some() && id == streaming
        };
        for block in blocks {
            match str_field(block, "type") {
                Some("text") => {
                    let text = str_field(block, "text").unwrap_or_default();
                    self.prose(&parent, Prose::Text, text, streamed, out);
                }
                Some("thinking") => {
                    let text = str_field(block, "thinking").unwrap_or_default();
                    self.prose(&parent, Prose::Reasoning, text, streamed, out);
                }
                Some("tool_use") => {
                    let (Some(id), Some(name)) = (str_field(block, "id"), str_field(block, "name"))
                    else {
                        continue;
                    };
                    let input = block.get("input").cloned().unwrap_or_else(|| json!({}));
                    self.start_tool(id, name, input, out);
                }
                _ => {}
            }
        }
    }

    /// A complete text or thinking block. When its message was streamed, the
    /// block only finishes the item its deltas built; otherwise it is the item.
    fn prose(
        &mut self,
        parent: &Option<String>,
        prose: Prose,
        text: &str,
        streamed: bool,
        out: &mut Vec<RunnerOutput>,
    ) {
        if streamed {
            let Some(scope) = self.scopes.get_mut(parent) else {
                return;
            };
            if let Some(mut block) = scope.block.take_if(|block| block.prose == prose) {
                // The complete text wins over the sum of the deltas.
                if !text.is_empty() {
                    if !block.started {
                        block.start(out);
                    }
                    block.text = text.to_owned();
                }
                block.complete(out);
            }
            return;
        }
        if text.is_empty() && prose == Prose::Reasoning {
            return;
        }
        let mut block = Block {
            index: None,
            prose,
            item_id: self.mint_id(),
            text: String::new(),
            started: false,
        };
        block.start(out);
        block.text = text.to_owned();
        block.complete(out);
    }

    fn start_tool(&mut self, id: &str, name: &str, input: Value, out: &mut Vec<RunnerOutput>) {
        if !self.started.insert(id.to_owned()) {
            return;
        }
        let view = tools::describe(name, &input, &self.cwd);
        let call = ToolCall {
            id: id.to_owned(),
            name: name.to_owned(),
            kind: view.kind,
            title: view.title,
            input,
            locations: view.locations,
        };
        out.push(event(EventBody::ItemStarted {
            item: call.item(ItemStatus::Running, None),
        }));
        if name == tools::PLAN_TOOL
            && let Some(entries) = tools::plan_entries(&call.input)
        {
            out.push(event(EventBody::PlanUpdated { entries }));
        }
        self.running.push(call);
    }

    fn user(&mut self, message: &Value, out: &mut Vec<RunnerOutput>) {
        let blocks = message
            .get("message")
            .and_then(|inner| inner.get("content"))
            .and_then(Value::as_array);
        for block in blocks.map(Vec::as_slice).unwrap_or_default() {
            if str_field(block, "type") != Some("tool_result") {
                continue;
            }
            let Some(id) = str_field(block, "tool_use_id") else {
                continue;
            };
            let Some(position) = self.running.iter().position(|call| call.id == id) else {
                tracing::debug!(
                    tool_use_id = id,
                    "result for a tool call that is not running"
                );
                continue;
            };
            let call = self.running.remove(position);
            // What a subagent left open ends with the call that ran it.
            let scope = Some(id.to_owned());
            self.close_block(&scope, out);
            self.scopes.remove(&scope);

            let failed = block.get("is_error").and_then(Value::as_bool) == Some(true);
            let status = if self.denied.remove(id) || rejected(message, id) {
                ItemStatus::Declined
            } else if failed {
                ItemStatus::Failed
            } else {
                ItemStatus::Completed
            };
            let output = tools::result_text(block.get("content").unwrap_or(&Value::Null));
            out.push(event(EventBody::ItemCompleted {
                item: call.item(status, Some(tools::capped(output))),
            }));
        }
    }

    fn control_request(&mut self, message: &Value, out: &mut Vec<RunnerOutput>) {
        let (Some(request_id), Some(request)) =
            (str_field(message, "request_id"), message.get("request"))
        else {
            return;
        };
        if str_field(request, "subtype") != Some("can_use_tool") {
            tracing::debug!(
                subtype = str_field(request, "subtype"),
                "ignored a control request from claude"
            );
            return;
        }
        let Some(name) = str_field(request, "tool_name") else {
            return;
        };
        let input = request.get("input").cloned().unwrap_or_else(|| json!({}));
        let tool_use_id = str_field(request, "tool_use_id").map(str::to_owned);
        if let Some(id) = &tool_use_id {
            // The request normally follows the call's message. If it comes
            // first, the item starts here so that `item_id` points at one.
            self.start_tool(id, name, input.clone(), out);
        }
        let view = tools::describe(name, &input, &self.cwd);
        self.pending.insert(
            request_id.to_owned(),
            PendingPermission {
                tool_use_id: tool_use_id.clone(),
                input: input.clone(),
            },
        );
        out.push(RunnerOutput::PermissionRequest(PermissionRequest {
            id: request_id.to_owned(),
            tool_name: name.to_owned(),
            tool_kind: view.kind,
            title: view.title,
            input,
            item_id: tool_use_id,
        }));
    }

    /// The end of a turn: totals, the error if it failed, `TurnEnded`.
    fn result(&mut self, message: &Value, out: &mut Vec<RunnerOutput>) {
        self.close_all(out);
        self.count(message);
        out.push(event(EventBody::Usage {
            cost_usd: self.totals.cost_usd,
            tokens_in: self.totals.tokens_in,
            tokens_out: self.totals.tokens_out,
        }));

        let subtype = str_field(message, "subtype");
        let failed = message.get("is_error").and_then(Value::as_bool) == Some(true)
            || subtype.is_some_and(|subtype| subtype != "success");
        // An interrupted turn reports itself as an error, but it is what was asked for.
        let aborted = str_field(message, "terminal_reason")
            .is_some_and(|reason| reason.starts_with("aborted"));
        if failed && !aborted {
            self.error(&failure(message, subtype), out);
        }
        self.turn_error = None;
        out.push(RunnerOutput::TurnEnded);
    }

    /// Updates the running totals from a `result`.
    ///
    /// `total_cost_usd` and `modelUsage` are totals for the conversation, and
    /// a resumed process continues them. `modelUsage` is preferred over
    /// `usage`, which covers one turn and leaves out subagents.
    fn count(&mut self, message: &Value) {
        if let Some(cost) = message.get("total_cost_usd").and_then(Value::as_f64) {
            self.totals.cost_usd = self.totals.cost_usd.max(cost);
        }
        let tokens = |usage: &Value, key: &str| usage.get(key).and_then(Value::as_u64).unwrap_or(0);
        let models = message.get("modelUsage").and_then(Value::as_object);
        if let Some(models) = models.filter(|models| !models.is_empty()) {
            let (mut tokens_in, mut tokens_out) = (0, 0);
            for usage in models.values() {
                tokens_in += tokens(usage, "inputTokens")
                    + tokens(usage, "cacheReadInputTokens")
                    + tokens(usage, "cacheCreationInputTokens");
                tokens_out += tokens(usage, "outputTokens");
            }
            self.totals.tokens_in = self.totals.tokens_in.max(tokens_in);
            self.totals.tokens_out = self.totals.tokens_out.max(tokens_out);
        } else if let Some(usage) = message.get("usage") {
            self.totals.tokens_in += tokens(usage, "input_tokens")
                + tokens(usage, "cache_read_input_tokens")
                + tokens(usage, "cache_creation_input_tokens");
            self.totals.tokens_out += tokens(usage, "output_tokens");
        }
    }

    /// Shows an error once per turn, however many messages repeat it.
    fn error(&mut self, text: &str, out: &mut Vec<RunnerOutput>) {
        let text = text.trim();
        let mut message: String = text.chars().take(ERROR_CHARS).collect();
        if text.chars().count() > ERROR_CHARS {
            message.push('…');
        }
        if message.is_empty() || self.turn_error.as_deref() == Some(&message) {
            return;
        }
        out.push(event(EventBody::Error {
            message: message.clone(),
        }));
        self.turn_error = Some(message);
    }

    fn close_block(&mut self, parent: &Option<String>, out: &mut Vec<RunnerOutput>) {
        if let Some(mut block) = self.scopes.get_mut(parent).and_then(|s| s.block.take()) {
            block.complete(out);
        }
    }

    /// Completes every open item: prose with what arrived, tool calls as
    /// failed (or declined, when their request was denied). Also voids the
    /// permission requests nobody answered.
    fn close_all(&mut self, out: &mut Vec<RunnerOutput>) {
        for scope in std::mem::take(&mut self.scopes).into_values() {
            if let Some(mut block) = scope.block {
                block.complete(out);
            }
        }
        for call in std::mem::take(&mut self.running) {
            let status = if self.denied.contains(&call.id) {
                ItemStatus::Declined
            } else {
                ItemStatus::Failed
            };
            out.push(event(EventBody::ItemCompleted {
                item: call.item(status, None),
            }));
        }
        self.denied.clear();
        self.pending.clear();
    }

    fn mint_id(&mut self) -> String {
        self.minted += 1;
        format!("{}-{}", self.id_prefix, self.minted)
    }
}

impl Block {
    fn start(&mut self, out: &mut Vec<RunnerOutput>) {
        self.started = true;
        out.push(event(EventBody::ItemStarted {
            item: Item {
                id: self.item_id.clone(),
                status: ItemStatus::Running,
                content: self.prose.content(String::new()),
            },
        }));
    }

    fn append(&mut self, chunk: &str, out: &mut Vec<RunnerOutput>) {
        if chunk.is_empty() {
            return;
        }
        if !self.started {
            self.start(out);
        }
        self.text.push_str(chunk);
        out.push(event(EventBody::ItemDelta {
            item_id: self.item_id.clone(),
            delta: self.prose.delta(chunk.to_owned()),
        }));
    }

    /// Completes the item, if the block ever became one.
    fn complete(&mut self, out: &mut Vec<RunnerOutput>) {
        if !self.started {
            return;
        }
        self.started = false;
        out.push(event(EventBody::ItemCompleted {
            item: Item {
                id: self.item_id.clone(),
                status: ItemStatus::Completed,
                content: self.prose.content(std::mem::take(&mut self.text)),
            },
        }));
    }
}

fn event(body: EventBody) -> RunnerOutput {
    RunnerOutput::Event(body)
}

fn str_field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

/// The subagent a message belongs to: the tool-use id of the call that runs it.
fn parent_of(message: &Value) -> Option<String> {
    str_field(message, "parent_tool_use_id").map(str::to_owned)
}

/// True when the `user` message says the call was refused at a permission
/// prompt, by the user or by a rule, rather than failed or interrupted.
fn rejected(message: &Value, tool_use_id: &str) -> bool {
    let meta = message.get("tool_result_meta").and_then(Value::as_array);
    meta.map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter(|entry| str_field(entry, "id") == Some(tool_use_id))
        .filter_map(|entry| entry.get("permission_decision"))
        .any(|decision| str_field(decision, "decision") == Some("reject"))
}

/// A short account of why a `result` is an error.
fn failure(message: &Value, subtype: Option<&str>) -> String {
    if let Some(text) = str_field(message, "result").filter(|text| !text.trim().is_empty()) {
        return text.to_owned();
    }
    let errors: Vec<&str> = message
        .get("errors")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    if !errors.is_empty() {
        return errors.join("\n");
    }
    match subtype {
        Some("error_max_turns") => "The agent reached its turn limit.".to_owned(),
        Some("error_max_budget_usd") => "The agent reached its budget.".to_owned(),
        Some(subtype) if subtype != "success" => {
            format!("The turn ended with an error ({subtype}).")
        }
        _ => "The turn ended with an error.".to_owned(),
    }
}

#[cfg(test)]
mod tests;
