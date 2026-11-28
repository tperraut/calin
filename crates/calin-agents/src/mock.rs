//! A scripted agent with no network and no randomness, for tests, demos and
//! `CALIN_DRY_RUN=1`. Directives in a prompt (see `demo`) make it call
//! Calin's own tool endpoint on loopback, and nothing else.

mod demo;
mod tools;

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::Duration;

use calin_proto::{
    EventBody, Item, ItemContent, ItemDelta, ItemStatus, LimitWindow, PermissionDecision,
    PermissionMode, PermissionRequest, PlanEntry, PlanStatus, ToolKind,
};
use serde_json::json;
use tokio::sync::mpsc::error::TryRecvError;

use crate::prose::Prose;
use crate::runner::{
    AgentHandle, AgentIo, Exit, PermissionAsk, Runner, RunnerError, RunnerInput, RunnerOutput,
    StartParams, SubagentStart, ToolAccess,
};

pub const MOCK_RUNNER_ID: &str = "mock";

const MOCK_MODEL: &str = "mock-1";

/// The file the scripted edit appends to, relative to the working directory.
/// A real file, so a dry run has something to show as a change and a diff.
const EDITED_FILE: &str = "calin-mock-notes.md";

/// The scripted subagent: its type and what it is asked to do.
const SUBAGENT_ROLE: &str = "explorer";
const SUBAGENT_TASK: &str = "Read the project notes";
const SUBAGENT_PROMPT: &str = "Read calin-mock-notes.md and say what it holds.";

const DEFAULT_DELAY: Duration = Duration::from_millis(40);

/// How much of the user's text the scripted answer quotes back.
const QUOTE_CHARS: usize = 120;

/// The one limit window the mock reports, named like the CLI's.
const MOCK_WINDOW: &str = "five_hour";
const FIVE_HOURS_MS: i64 = 5 * 60 * 60 * 1000;

const PLAN: [&str; 3] = [
    "Run the test suite",
    "Make the change",
    "Summarize the result",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MockConfig {
    /// Pause before each chunk, so a turn looks like a live agent.
    pub delay: Duration,
}

impl Default for MockConfig {
    fn default() -> Self {
        Self {
            delay: DEFAULT_DELAY,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct MockRunner {
    config: MockConfig,
}

impl MockRunner {
    pub fn new(config: MockConfig) -> Self {
        Self { config }
    }
}

impl Runner for MockRunner {
    fn id(&self) -> &str {
        MOCK_RUNNER_ID
    }

    fn model(&self) -> Option<String> {
        Some(MOCK_MODEL.to_owned())
    }

    fn start(&self, params: StartParams) -> Result<AgentHandle, RunnerError> {
        let (handle, io) = AgentHandle::channel();
        self.spawn(params, io);
        Ok(handle)
    }
}

impl MockRunner {
    /// Runs the scripted agent on `io`, the runner's end of a handle.
    fn spawn(&self, params: StartParams, io: AgentIo) {
        let native_session = params
            .resume
            .unwrap_or_else(|| format!("mock-{}", params.agent_id));
        let agent = MockAgent {
            io,
            delay: self.config.delay,
            start_seq: params.start_seq,
            permission_mode: params.permission_mode,
            // `locations` are absolute by contract; without a real directory there are none.
            edited: Some(params.cwd.join(EDITED_FILE)).filter(|path| path.is_absolute()),
            items: 0,
            turns: 0,
            open: None,
            inside: None,
            calling: None,
            queued: VecDeque::new(),
            tools: match params.tool_endpoint.as_deref() {
                Some(url) => tools::Endpoint::parse(url),
                None => Err("none was given".to_owned()),
            },
            rpc_ids: 0,
            dispatched: Vec::new(),
            reported: Vec::new(),
        };
        tokio::spawn(agent.run(native_session, params.prompt));
    }
}

/// Why a turn ended early.
enum Cut {
    /// The orchestrator asked for the turn to end; the agent lives on.
    Interrupted,
    /// The orchestrator asked for a stop.
    Stopped,
    /// The orchestrator dropped the handle; nobody is listening.
    Detached,
}

struct MockAgent {
    io: AgentIo,
    delay: Duration,
    start_seq: u64,
    permission_mode: PermissionMode,
    /// The file the scripted edit appends to, when the working directory is known.
    edited: Option<PathBuf>,
    items: u64,
    turns: u64,
    /// The item being streamed, as far as it got. Completed when the turn is cut short.
    open: Option<Item>,
    /// The key of the subagent that is at work: what is emitted is its own.
    inside: Option<String>,
    /// The item of the call that runs that subagent. It stays open, and the
    /// agent's own, until the subagent has ended.
    calling: Option<Item>,
    /// Messages that arrived mid-turn; each starts a turn of its own.
    queued: VecDeque<String>,
    /// Calin's tool endpoint, or why the demo directives cannot use it.
    tools: Result<tools::Endpoint, String>,
    /// The last JSON-RPC id used.
    rpc_ids: u64,
    /// The titles of the children the last `demo: orchestrate` dispatched.
    dispatched: Vec<String>,
    /// The titles of the children that reported since.
    reported: Vec<String>,
}

impl MockAgent {
    async fn run(mut self, native_session: String, prompt: String) {
        let announced = RunnerOutput::NativeSession {
            id: native_session,
            model: Some(MOCK_MODEL.to_owned()),
        };
        if self.io.outputs.send(announced).await.is_err() {
            return;
        }
        let mut next = Some(prompt);
        while let Some(text) = next.take() {
            let cut = self.turn(&text).await.err();
            if matches!(cut, Some(Cut::Detached)) || !self.close_open_items().await {
                return;
            }
            if matches!(cut, Some(Cut::Stopped)) {
                break;
            }
            if self.io.outputs.send(RunnerOutput::TurnEnded).await.is_err() {
                return;
            }
            next = self.next_message().await;
        }
        let _ = self.io.outputs.send(RunnerOutput::Exited(Exit::Ok)).await;
    }

    /// The next user message, or `None` on a stop request or a dropped handle.
    async fn next_message(&mut self) -> Option<String> {
        loop {
            if let Some(text) = self.queued.pop_front() {
                return Some(text);
            }
            match self.io.inputs.recv().await? {
                RunnerInput::UserMessage(text) => return Some(text),
                RunnerInput::Stop => return None,
                // Nothing to interrupt and nothing waiting between turns.
                RunnerInput::Interrupt | RunnerInput::PermissionDecision { .. } => {}
            }
        }
    }

    /// Completes what a cut turn left running: a tool call as failed, prose
    /// with what was written. A subagent that was at work ends, cut short,
    /// between its own item and the call that ran it. False when nobody is
    /// listening.
    async fn close_open_items(&mut self) -> bool {
        if !self.close_open_item().await {
            return false;
        }
        let Some(key) = self.inside.take() else {
            return true;
        };
        let ended = RunnerOutput::SubagentEnded { key, ok: false };
        if self.io.outputs.send(ended).await.is_err() {
            return false;
        }
        self.open = self.calling.take();
        self.close_open_item().await
    }

    async fn close_open_item(&mut self) -> bool {
        let Some(mut item) = self.open.take() else {
            return true;
        };
        item.status = match item.content {
            ItemContent::Tool { .. } => ItemStatus::Failed,
            ItemContent::Text { .. } | ItemContent::Reasoning { .. } => ItemStatus::Completed,
        };
        let completed = self.attributed(EventBody::ItemCompleted { item });
        self.io.outputs.send(completed).await.is_ok()
    }

    async fn turn(&mut self, user_text: &str) -> Result<(), Cut> {
        self.turns += 1;
        if let Some(script) = demo::script(user_text) {
            return self.demo(script).await;
        }
        let quote = quote(user_text);
        self.think(&quote).await?;
        self.run_tests().await?;
        self.make_the_change(&quote).await?;
        self.summarize(&quote).await?;
        self.report_usage().await
    }

    /// The fixed turn's opening: what it makes of the request, then the plan.
    async fn think(&mut self, quote: &str) -> Result<(), Cut> {
        self.streamed(
            Prose::Reasoning,
            &[
                "The request is: ",
                quote,
                ". ",
                "I should look at the tests first, ",
                "then make the smallest change that satisfies it.",
            ],
        )
        .await?;
        self.streamed(
            Prose::Text,
            &[
                "I'll start by ",
                "running the test suite ",
                "to see where ",
                "things stand.",
            ],
        )
        .await?;
        self.plan(0).await
    }

    /// Plan step 1.
    async fn run_tests(&mut self) -> Result<(), Cut> {
        self.tool(
            "Bash",
            ToolKind::Execute,
            "cargo test -q",
            json!({ "command": "cargo test -q" }),
            Vec::new(),
            &[
                "running 3 tests\n",
                "...\n",
                "test result: ok. 3 passed; 0 failed\n",
            ],
            None,
        )
        .await?;
        self.plan(1).await
    }

    /// Plan step 2: a subagent reads the notes, then the agent adds one.
    async fn make_the_change(&mut self, quote: &str) -> Result<(), Cut> {
        self.subagent(self.edited_locations()).await?;
        self.edit_notes(quote).await?;
        self.plan(2).await
    }

    /// Plan step 3.
    async fn summarize(&mut self, quote: &str) -> Result<(), Cut> {
        self.streamed(
            Prose::Text,
            &[
                "Done. ",
                "You asked: \"",
                quote,
                "\". ",
                "The tests pass and the change is in calin-mock-notes.md.",
            ],
        )
        .await?;
        self.plan(3).await
    }

    async fn report_usage(&mut self) -> Result<(), Cut> {
        self.emit(EventBody::Usage {
            cost_usd: 0.0125 * self.turns as f64,
            tokens_in: 1200 * self.turns,
            tokens_out: 350 * self.turns,
        })
        .await?;
        // So that a dry run shows the plan limits too.
        self.send(RunnerOutput::Limits(vec![mock_window(
            self.turns,
            now_ms(),
        )]))
        .await
    }

    /// The edited file as a tool call's locations: none without a directory.
    fn edited_locations(&self) -> Vec<String> {
        self.edited
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect()
    }

    /// Appends a line naming the turn to the notes file: a real change.
    async fn edit_notes(&mut self, quote: &str) -> Result<(), Cut> {
        let locations = self.edited_locations();
        let note = format!("- turn {}: {quote}\n", self.turns);
        self.tool(
            "Edit",
            ToolKind::Edit,
            EDITED_FILE,
            json!({
                "file_path": EDITED_FILE,
                "old_string": "",
                "new_string": note,
            }),
            locations,
            &[],
            Some(note),
        )
        .await
    }

    /// A subagent that reads the notes file and says what it holds: a call
    /// of the agent's own, and inside it a tool call and some text that are
    /// the subagent's. It never asks: reading needs no permission.
    async fn subagent(&mut self, locations: Vec<String>) -> Result<(), Cut> {
        let key = self.next_item_id();
        self.start_item(Item {
            id: key.clone(),
            status: ItemStatus::Running,
            content: ItemContent::Tool {
                name: "Agent".to_owned(),
                tool_kind: ToolKind::Task,
                title: SUBAGENT_TASK.to_owned(),
                input: json!({
                    "description": SUBAGENT_TASK,
                    "subagent_type": SUBAGENT_ROLE,
                    "prompt": SUBAGENT_PROMPT,
                }),
                output: None,
                locations: Vec::new(),
            },
        })
        .await?;
        self.pause().await?;
        self.send(RunnerOutput::SubagentStarted(SubagentStart {
            key: key.clone(),
            parent: None,
            role: SUBAGENT_ROLE.to_owned(),
            description: SUBAGENT_TASK.to_owned(),
            item_id: Some(key.clone()),
        }))
        .await?;
        self.calling = self.open.take();
        self.inside = Some(key.clone());

        let notes = self.turns - 1;
        let found = format!("{notes} notes from earlier turns\n");
        self.tool(
            "Read",
            ToolKind::Read,
            EDITED_FILE,
            json!({ "file_path": EDITED_FILE }),
            locations,
            &[&found],
            None,
        )
        .await?;
        self.streamed(
            Prose::Text,
            &["The notes file holds ", "one line ", "per earlier turn."],
        )
        .await?;

        self.pause().await?;
        self.send(RunnerOutput::SubagentEnded { key, ok: true })
            .await?;
        self.inside = None;
        self.open = self.calling.take();
        self.delta(ItemDelta::Output {
            text: format!("The notes file holds {notes} lines.\n"),
        })
        .await?;
        self.complete_item(ItemStatus::Completed).await
    }

    /// A text or reasoning item streamed chunk by chunk.
    async fn streamed(&mut self, prose: Prose, chunks: &[&str]) -> Result<(), Cut> {
        let id = self.next_item_id();
        self.start_item(Item {
            id: id.clone(),
            status: ItemStatus::Running,
            content: prose.content(String::new()),
        })
        .await?;
        for chunk in chunks {
            self.delta(prose.delta((*chunk).to_owned())).await?;
        }
        self.complete_item(ItemStatus::Completed).await
    }

    /// A tool call whose output arrives in `output` chunks. Depending on the
    /// permission mode it first asks, and completes as declined on a denial.
    /// `note` is the line an allowed call appends to the edited file.
    #[allow(clippy::too_many_arguments)]
    async fn tool(
        &mut self,
        name: &str,
        tool_kind: ToolKind,
        title: &str,
        input: serde_json::Value,
        locations: Vec<String>,
        output: &[&str],
        note: Option<String>,
    ) -> Result<(), Cut> {
        let id = self.next_item_id();
        self.start_item(Item {
            id: id.clone(),
            status: ItemStatus::Running,
            content: ItemContent::Tool {
                name: name.to_owned(),
                tool_kind,
                title: title.to_owned(),
                input: input.clone(),
                output: None,
                locations,
            },
        })
        .await?;
        if self.asks_before(tool_kind)
            && let (PermissionDecision::Deny, message) = self
                .ask_about(id, name, tool_kind, title, input, self.access_of(tool_kind))
                .await?
        {
            self.set_tool_output(message);
            return self.complete_item(ItemStatus::Declined).await;
        }
        if let Err(reason) = self.append_note(note).await {
            self.set_tool_output(Some(reason));
            return self.complete_item(ItemStatus::Failed).await;
        }
        for chunk in output {
            self.delta(ItemDelta::Output {
                text: (*chunk).to_owned(),
            })
            .await?;
        }
        if let Some(output) = self.open_tool_output() {
            // A completed call always has an output, even an empty one.
            output.get_or_insert_default();
        }
        self.complete_item(ItemStatus::Completed).await
    }

    /// What a permission request for a scripted call of `tool_kind` is about.
    fn access_of(&self, tool_kind: ToolKind) -> ToolAccess {
        match (tool_kind, &self.edited) {
            (ToolKind::Edit, Some(path)) => ToolAccess::Files {
                paths: vec![path.clone()],
                write: true,
            },
            _ => ToolAccess::Other,
        }
    }

    /// Asks for the tool call `id`, as the agent or the subagent at work,
    /// and waits for the decision.
    async fn ask_about(
        &mut self,
        id: String,
        name: &str,
        tool_kind: ToolKind,
        title: &str,
        input: serde_json::Value,
        access: ToolAccess,
    ) -> Result<(PermissionDecision, Option<String>), Cut> {
        let request = PermissionAsk {
            request: PermissionRequest {
                id: format!("{id}-permission"),
                tool_name: name.to_owned(),
                tool_kind,
                title: title.to_owned(),
                input,
                item_id: Some(id),
            },
            access,
            subagent: self.inside.clone(),
        };
        self.ask(request).await
    }

    /// Appends `note` to the edited file, when there is a note and a file.
    async fn append_note(&self, note: Option<String>) -> Result<(), String> {
        let (Some(note), Some(path)) = (note, self.edited.clone()) else {
            return Ok(());
        };
        append_line(path, note).await
    }

    /// The output of the open item, when it is a tool call.
    fn open_tool_output(&mut self) -> Option<&mut Option<String>> {
        match &mut self.open {
            Some(Item {
                content: ItemContent::Tool { output, .. },
                ..
            }) => Some(output),
            _ => None,
        }
    }

    fn set_tool_output(&mut self, text: Option<String>) {
        if let Some(output) = self.open_tool_output() {
            *output = text;
        }
    }

    fn asks_before(&self, tool_kind: ToolKind) -> bool {
        match self.permission_mode {
            PermissionMode::Ask => matches!(tool_kind, ToolKind::Execute | ToolKind::Edit),
            PermissionMode::AcceptEdits => tool_kind == ToolKind::Execute,
            PermissionMode::Autonomous => false,
        }
    }

    /// Sends a permission request and waits for its decision, however long it takes.
    async fn ask(
        &mut self,
        request: PermissionAsk,
    ) -> Result<(PermissionDecision, Option<String>), Cut> {
        let id = request.request.id.clone();
        self.send(RunnerOutput::PermissionRequest(request)).await?;
        loop {
            match self.io.inputs.recv().await.ok_or(Cut::Detached)? {
                // The mock has no notion of directories: grants are ignored.
                RunnerInput::PermissionDecision {
                    request_id,
                    decision,
                    message,
                    ..
                } if request_id == id => return Ok((decision, message)),
                other => self.cut_by(other)?,
            }
        }
    }

    async fn start_item(&mut self, item: Item) -> Result<(), Cut> {
        self.emit(EventBody::ItemStarted { item: item.clone() })
            .await?;
        self.open = Some(item);
        Ok(())
    }

    /// Sends a chunk of the open item and appends it to what a cut turn would complete with.
    async fn delta(&mut self, delta: ItemDelta) -> Result<(), Cut> {
        let Some(item) = &mut self.open else {
            return Ok(());
        };
        let item_id = item.id.clone();
        self.emit(EventBody::ItemDelta {
            item_id,
            delta: delta.clone(),
        })
        .await?;
        if let Some(item) = &mut self.open {
            match (&mut item.content, delta) {
                (ItemContent::Text { text }, ItemDelta::Text { text: chunk })
                | (ItemContent::Reasoning { text }, ItemDelta::Reasoning { text: chunk }) => {
                    text.push_str(&chunk);
                }
                (ItemContent::Tool { output, .. }, ItemDelta::Output { text: chunk }) => {
                    output.get_or_insert_default().push_str(&chunk);
                }
                _ => {}
            }
        }
        Ok(())
    }

    async fn complete_item(&mut self, status: ItemStatus) -> Result<(), Cut> {
        let Some(mut item) = self.open.clone() else {
            return Ok(());
        };
        item.status = status;
        self.emit(EventBody::ItemCompleted { item }).await?;
        self.open = None;
        Ok(())
    }

    /// The full plan with the first `done` steps completed and the next one in progress.
    async fn plan(&mut self, done: usize) -> Result<(), Cut> {
        let entries = PLAN
            .iter()
            .enumerate()
            .map(|(index, content)| PlanEntry {
                content: (*content).to_owned(),
                status: match index {
                    i if i < done => PlanStatus::Completed,
                    i if i == done => PlanStatus::InProgress,
                    _ => PlanStatus::Pending,
                },
            })
            .collect();
        self.emit(EventBody::PlanUpdated { entries }).await
    }

    async fn emit(&mut self, body: EventBody) -> Result<(), Cut> {
        self.pause().await?;
        let output = self.attributed(body);
        self.send(output).await
    }

    /// An event of the subagent that is at work, or of the agent itself.
    fn attributed(&self, body: EventBody) -> RunnerOutput {
        match &self.inside {
            Some(key) => RunnerOutput::SubagentEvent {
                key: key.clone(),
                body,
            },
            None => RunnerOutput::Event(body),
        }
    }

    async fn send(&mut self, output: RunnerOutput) -> Result<(), Cut> {
        self.io
            .outputs
            .send(output)
            .await
            .map_err(|_| Cut::Detached)
    }

    /// Waits out the delay, ending early on an interrupt or a stop request.
    async fn pause(&mut self) -> Result<(), Cut> {
        if self.delay.is_zero() {
            loop {
                match self.io.inputs.try_recv() {
                    Ok(input) => self.cut_by(input)?,
                    Err(TryRecvError::Empty) => return Ok(()),
                    Err(TryRecvError::Disconnected) => return Err(Cut::Detached),
                }
            }
        }
        let sleep = tokio::time::sleep(self.delay);
        tokio::pin!(sleep);
        loop {
            tokio::select! {
                () = &mut sleep => return Ok(()),
                input = self.io.inputs.recv() => match input {
                    Some(input) => self.cut_by(input)?,
                    None => return Err(Cut::Detached),
                },
            }
        }
    }

    /// What an input received in the middle of a turn does to the turn.
    fn cut_by(&mut self, input: RunnerInput) -> Result<(), Cut> {
        match input {
            RunnerInput::UserMessage(text) => {
                self.queued.push_back(text);
                Ok(())
            }
            // Not the request the turn waits for, if it waits at all.
            RunnerInput::PermissionDecision { .. } => Ok(()),
            RunnerInput::Interrupt => Err(Cut::Interrupted),
            RunnerInput::Stop => Err(Cut::Stopped),
        }
    }

    /// Unique within the session: `start_seq` separates starts, the counter
    /// separates items of one start.
    fn next_item_id(&mut self) -> String {
        self.items += 1;
        format!("mock-{}-{}", self.start_seq, self.items)
    }
}

/// Appends `line` to the file at `path`, creating the file. The directory
/// must exist: it is the agent's working directory.
async fn append_line(path: PathBuf, line: String) -> Result<(), String> {
    let written = tokio::task::spawn_blocking(move || {
        use std::io::Write as _;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|err| format!("cannot open {}: {err}", path.display()))?;
        file.write_all(line.as_bytes())
            .map_err(|err| format!("cannot write {}: {err}", path.display()))
    })
    .await;
    written.unwrap_or_else(|err| Err(format!("the edit did not run: {err}")))
}

/// A plausible window: a little more used each turn, resetting at the end
/// of the current five-hour stretch of the clock.
fn mock_window(turns: u64, now_ms: i64) -> LimitWindow {
    LimitWindow {
        name: MOCK_WINDOW.to_owned(),
        // In whole points, as a person would read them.
        used: (10 + 2 * turns).min(95) as f64 / 100.0,
        resets_at: Some((now_ms.div_euclid(FIVE_HOURS_MS) + 1) * FIVE_HOURS_MS),
        refused: false,
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as i64)
}

/// The first line of the user's text, short enough to quote.
fn quote(text: &str) -> String {
    let line = text.trim().lines().next().unwrap_or_default().trim();
    let mut quote: String = line.chars().take(QUOTE_CHARS).collect();
    if line.chars().count() > QUOTE_CHARS {
        quote.push_str("...");
    }
    quote
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use super::*;

    mod demo;

    const TIMEOUT: Duration = Duration::from_secs(5);

    /// A running mock agent and the temporary directory it works in.
    struct Started {
        handle: AgentHandle,
        dir: std::sync::Arc<tempfile::TempDir>,
    }

    impl Started {
        fn notes_path(&self) -> PathBuf {
            self.dir.path().join(EDITED_FILE)
        }

        /// The notes file, or `None` when the agent never wrote it.
        fn notes(&self) -> Option<String> {
            std::fs::read_to_string(self.notes_path()).ok()
        }
    }

    impl std::ops::Deref for Started {
        type Target = AgentHandle;

        fn deref(&self) -> &AgentHandle {
            &self.handle
        }
    }

    impl std::ops::DerefMut for Started {
        fn deref_mut(&mut self) -> &mut AgentHandle {
            &mut self.handle
        }
    }

    fn start_at(
        dir: std::sync::Arc<tempfile::TempDir>,
        mode: PermissionMode,
        delay: Duration,
        prompt: &str,
    ) -> Started {
        let handle = MockRunner::new(MockConfig { delay })
            .start(StartParams {
                session_id: "s1".into(),
                agent_id: "a1".into(),
                prompt: prompt.into(),
                cwd: dir.path().to_owned(),
                permission_mode: mode,
                start_seq: 3,
                ..StartParams::default()
            })
            .unwrap();
        Started { handle, dir }
    }

    fn start_in(mode: PermissionMode, delay: Duration, prompt: &str) -> Started {
        let dir = std::sync::Arc::new(tempfile::tempdir().unwrap());
        start_at(dir, mode, delay, prompt)
    }

    /// An agent that never asks, so a turn runs to its end unattended.
    fn start(delay: Duration, prompt: &str) -> Started {
        start_in(PermissionMode::Autonomous, delay, prompt)
    }

    async fn next(handle: &mut AgentHandle) -> Option<RunnerOutput> {
        tokio::time::timeout(TIMEOUT, handle.next_output())
            .await
            .expect("the mock agent went quiet")
    }

    /// One turn, with every permission request answered by `answer`.
    struct Turn {
        /// Every event of the turn, the subagent's among the agent's own.
        events: Vec<EventBody>,
        /// The same events with whose they are, and the subagent's start and end.
        outputs: Vec<RunnerOutput>,
        requests: Vec<PermissionRequest>,
        /// What each request said it reaches for, in the order of `requests`.
        access: Vec<ToolAccess>,
        /// What the agent reported of the plan's limits.
        limits: Vec<Vec<LimitWindow>>,
    }

    async fn run_turn(
        handle: &mut AgentHandle,
        mut answer: impl FnMut(&PermissionRequest) -> (PermissionDecision, Option<String>),
    ) -> Turn {
        let mut turn = Turn {
            events: Vec::new(),
            outputs: Vec::new(),
            requests: Vec::new(),
            access: Vec::new(),
            limits: Vec::new(),
        };
        loop {
            match next(handle).await {
                Some(RunnerOutput::Event(body)) => {
                    turn.outputs.push(RunnerOutput::Event(body.clone()));
                    turn.events.push(body);
                }
                Some(RunnerOutput::SubagentEvent { key, body }) => {
                    let body = body.clone();
                    turn.events.push(body.clone());
                    turn.outputs.push(RunnerOutput::SubagentEvent { key, body });
                }
                Some(
                    output
                    @ (RunnerOutput::SubagentStarted(_) | RunnerOutput::SubagentEnded { .. }),
                ) => turn.outputs.push(output),
                Some(RunnerOutput::PermissionRequest(PermissionAsk {
                    request,
                    access,
                    subagent,
                })) => {
                    // The scripted subagent only reads, which never asks.
                    assert_eq!(subagent, None);
                    turn.access.push(access);
                    let (decision, message) = answer(&request);
                    handle
                        .resolve_permission(request.id.clone(), decision, message)
                        .unwrap();
                    turn.requests.push(request);
                }
                Some(RunnerOutput::NativeSession { .. }) => {}
                Some(RunnerOutput::Limits(windows)) => turn.limits.push(windows),
                Some(RunnerOutput::TurnEnded) => return turn,
                other => panic!("unexpected output {other:?}"),
            }
        }
    }

    /// Collects the events of one turn, up to `TurnEnded`. The turn must not ask.
    async fn collect_turn(handle: &mut AgentHandle) -> Vec<EventBody> {
        let turn = run_turn(handle, |request| panic!("unexpected request {request:?}")).await;
        turn.events
    }

    /// Checks the item lifecycle of a turn and returns its finished items in order.
    fn check_turn(events: &[EventBody]) -> Vec<Item> {
        let mut open: HashMap<String, String> = HashMap::new();
        let mut completed = Vec::new();
        for body in events {
            match body {
                EventBody::ItemStarted { item } => {
                    assert_eq!(item.status, ItemStatus::Running);
                    assert!(
                        open.insert(item.id.clone(), String::new()).is_none(),
                        "item {} started twice",
                        item.id
                    );
                }
                EventBody::ItemDelta { item_id, delta } => {
                    let text = open
                        .get_mut(item_id)
                        .unwrap_or_else(|| panic!("delta for {item_id}, which is not running"));
                    match delta {
                        ItemDelta::Text { text: chunk }
                        | ItemDelta::Reasoning { text: chunk }
                        | ItemDelta::Output { text: chunk } => text.push_str(chunk),
                    }
                }
                EventBody::ItemCompleted { item } => {
                    let streamed = open
                        .remove(&item.id)
                        .unwrap_or_else(|| panic!("item {} completed but never started", item.id));
                    assert_ne!(item.status, ItemStatus::Running);
                    if item.status == ItemStatus::Completed {
                        let final_text = match &item.content {
                            ItemContent::Text { text } | ItemContent::Reasoning { text } => {
                                text.clone()
                            }
                            ItemContent::Tool { output, .. } => output.clone().unwrap_or_default(),
                        };
                        assert_eq!(streamed, final_text, "deltas of {} do not add up", item.id);
                    }
                    completed.push(item.clone());
                }
                EventBody::PlanUpdated { entries } => assert_eq!(entries.len(), PLAN.len()),
                EventBody::Usage { .. } => {}
                other => panic!("the mock runner must not emit {other:?}"),
            }
        }
        assert!(open.is_empty(), "items never completed: {open:?}");
        completed
    }

    /// Who did what, one short line per output: item starts and ends
    /// (prefixed with the subagent's key when they are its own) and the
    /// subagent's start (`+`) and end (`-`). Also checks that what carries
    /// a key lies between that subagent's start and its end.
    fn trace(outputs: &[RunnerOutput]) -> Vec<String> {
        let line = |body: &EventBody| match body {
            EventBody::ItemStarted { item } => Some(format!("start {}", item.id)),
            EventBody::ItemCompleted { item } => {
                let status = format!("{:?}", item.status).to_lowercase();
                Some(format!("{status} {}", item.id))
            }
            _ => None,
        };
        let mut running: Option<String> = None;
        let mut lines = Vec::new();
        for output in outputs {
            match output {
                RunnerOutput::Event(body) => lines.extend(line(body)),
                RunnerOutput::SubagentEvent { key, body } => {
                    assert_eq!(running.as_ref(), Some(key), "{key} is not running");
                    lines.extend(line(body).map(|line| format!("{key}: {line}")));
                }
                RunnerOutput::SubagentStarted(start) => {
                    assert_eq!(running, None, "one subagent at a time");
                    assert_eq!(start.parent, None);
                    assert_eq!(start.item_id.as_ref(), Some(&start.key));
                    assert_eq!(start.role, SUBAGENT_ROLE);
                    assert_eq!(start.description, SUBAGENT_TASK);
                    running = Some(start.key.clone());
                    lines.push(format!("+{}", start.key));
                }
                RunnerOutput::SubagentEnded { key, ok } => {
                    assert_eq!(running.take().as_ref(), Some(key), "{key} is not running");
                    lines.push(format!("-{key} {}", if *ok { "ok" } else { "cut" }));
                }
                other => panic!("not part of a turn's work: {other:?}"),
            }
        }
        assert_eq!(running, None, "a subagent never ended");
        lines
    }

    /// The tool calls of the agent and of its subagent, in the order they ended.
    fn tool_items(items: &[Item]) -> Vec<(ToolKind, ItemStatus)> {
        items
            .iter()
            .filter_map(|item| match &item.content {
                ItemContent::Tool { tool_kind, .. } => Some((*tool_kind, item.status)),
                _ => None,
            })
            .collect()
    }

    fn usage(events: &[EventBody]) -> (f64, u64, u64) {
        match events.last() {
            Some(EventBody::Usage {
                cost_usd,
                tokens_in,
                tokens_out,
            }) => (*cost_usd, *tokens_in, *tokens_out),
            other => panic!("a turn must end with usage, got {other:?}"),
        }
    }

    fn allow(_: &PermissionRequest) -> (PermissionDecision, Option<String>) {
        (PermissionDecision::Allow, None)
    }

    #[tokio::test]
    async fn every_turn_reports_one_plausible_limit_window() {
        let mut handle = start(Duration::ZERO, "first");
        let first = run_turn(&mut handle, allow).await;
        handle.send_message("second").unwrap();
        let second = run_turn(&mut handle, allow).await;
        let [first] = first.limits.as_slice() else {
            panic!("one report per turn: {:?}", first.limits);
        };
        let [second] = second.limits.as_slice() else {
            panic!("one report per turn: {:?}", second.limits);
        };
        for windows in [first, second] {
            let [window] = windows.as_slice() else {
                panic!("one window: {windows:?}");
            };
            assert_eq!(window.name, MOCK_WINDOW);
            assert!(!window.refused);
            assert!((0.0..1.0).contains(&window.used), "{window:?}");
            let resets_at = window.resets_at.expect("a reset time");
            let now = now_ms();
            assert!(
                resets_at > now && resets_at <= now + FIVE_HOURS_MS,
                "{window:?}"
            );
        }
        // Used grows with the work done.
        assert!(second[0].used > first[0].used);
        assert_eq!(mock_window(100, 0).used, 0.95);
        assert_eq!(
            mock_window(1, FIVE_HOURS_MS - 1).resets_at,
            Some(FIVE_HOURS_MS)
        );
        assert_eq!(
            mock_window(1, FIVE_HOURS_MS).resets_at,
            Some(2 * FIVE_HOURS_MS)
        );
    }

    #[tokio::test]
    async fn a_turn_is_well_formed() {
        let mut handle = start(Duration::ZERO, "Fix the flaky login test\nsecond line");
        let events = collect_turn(&mut handle).await;
        let items = check_turn(&events);
        assert!(
            items
                .iter()
                .all(|item| item.status == ItemStatus::Completed)
        );

        let kinds: Vec<&str> = items
            .iter()
            .map(|item| match &item.content {
                ItemContent::Reasoning { .. } => "reasoning",
                ItemContent::Text { .. } => "text",
                ItemContent::Tool {
                    tool_kind: ToolKind::Execute,
                    ..
                } => "execute",
                ItemContent::Tool {
                    tool_kind: ToolKind::Edit,
                    ..
                } => "edit",
                ItemContent::Tool {
                    tool_kind: ToolKind::Read,
                    ..
                } => "read",
                ItemContent::Tool {
                    tool_kind: ToolKind::Task,
                    ..
                } => "task",
                ItemContent::Tool { .. } => "tool",
            })
            .collect();
        // In the order they ended: the subagent's read and text end inside its call.
        assert_eq!(
            kinds,
            [
                "reasoning",
                "text",
                "execute",
                "read",
                "text",
                "task",
                "edit",
                "text"
            ]
        );

        let text_deltas = events
            .iter()
            .filter(|body| {
                matches!(
                    body,
                    EventBody::ItemDelta {
                        delta: ItemDelta::Text { .. },
                        ..
                    }
                )
            })
            .count();
        assert!(text_deltas >= 4, "text must stream in several deltas");
        let output_deltas = events
            .iter()
            .filter(|body| {
                matches!(
                    body,
                    EventBody::ItemDelta {
                        delta: ItemDelta::Output { .. },
                        ..
                    }
                )
            })
            .count();
        assert!(output_deltas >= 2, "tool output must stream in deltas");
        assert!(
            events
                .iter()
                .any(|body| matches!(body, EventBody::PlanUpdated { .. }))
        );

        match &items.last().unwrap().content {
            ItemContent::Text { text } => {
                assert!(text.contains("Fix the flaky login test"), "{text}");
                assert!(!text.contains("second line"));
            }
            other => panic!("the turn must end on text, got {other:?}"),
        }
        assert_eq!(usage(&events), (0.0125, 1200, 350));
    }

    #[tokio::test]
    async fn the_edit_writes_the_file_it_names() {
        let mut handle = start(Duration::ZERO, "edit\nmore");
        assert_eq!(handle.notes(), None);
        let items = check_turn(&collect_turn(&mut handle).await);
        let locations: Vec<&Vec<String>> = items
            .iter()
            .filter_map(|item| match &item.content {
                ItemContent::Tool { locations, .. } => Some(locations),
                _ => None,
            })
            .collect();
        let edited = handle.notes_path().to_string_lossy().into_owned();
        // The command, the subagent's read, the call that ran it, the edit.
        assert_eq!(
            locations,
            [&vec![], &vec![edited.clone()], &vec![], &vec![edited]]
        );
        assert_eq!(handle.notes().as_deref(), Some("- turn 1: edit\n"));

        // Each turn appends its own line.
        handle.send_message("again").unwrap();
        collect_turn(&mut handle).await;
        assert_eq!(
            handle.notes().as_deref(),
            Some("- turn 1: edit\n- turn 2: again\n")
        );
    }

    #[tokio::test]
    async fn a_denied_edit_writes_nothing() {
        let mut handle = start_in(PermissionMode::Ask, Duration::ZERO, "deny the edit");
        let turn = run_turn(&mut handle, |request| match request.tool_kind {
            ToolKind::Edit => (PermissionDecision::Deny, None),
            _ => (PermissionDecision::Allow, None),
        })
        .await;
        assert_eq!(
            tool_items(&check_turn(&turn.events)),
            [
                (ToolKind::Execute, ItemStatus::Completed),
                (ToolKind::Read, ItemStatus::Completed),
                (ToolKind::Task, ItemStatus::Completed),
                (ToolKind::Edit, ItemStatus::Declined),
            ]
        );
        assert_eq!(handle.notes(), None);
    }

    #[tokio::test]
    async fn an_edit_that_cannot_be_written_fails_its_call_and_the_turn_goes_on() {
        let dir = std::sync::Arc::new(tempfile::tempdir().unwrap());
        // A directory where the file should be: the append cannot succeed.
        std::fs::create_dir(dir.path().join(EDITED_FILE)).unwrap();
        let mut handle = start_at(dir, PermissionMode::Autonomous, Duration::ZERO, "x");
        let items = check_turn(&collect_turn(&mut handle).await);
        assert_eq!(
            tool_items(&items),
            [
                (ToolKind::Execute, ItemStatus::Completed),
                (ToolKind::Read, ItemStatus::Completed),
                (ToolKind::Task, ItemStatus::Completed),
                (ToolKind::Edit, ItemStatus::Failed),
            ]
        );
        assert_eq!(items.len(), 8);
    }

    #[tokio::test]
    async fn without_a_real_directory_the_edit_names_and_writes_nothing() {
        let mut handle = MockRunner::new(MockConfig {
            delay: Duration::ZERO,
        })
        .start(StartParams {
            permission_mode: PermissionMode::Autonomous,
            ..StartParams::default()
        })
        .unwrap();
        let items = check_turn(&collect_turn(&mut handle).await);
        assert_eq!(
            tool_items(&items),
            [
                (ToolKind::Execute, ItemStatus::Completed),
                (ToolKind::Read, ItemStatus::Completed),
                (ToolKind::Task, ItemStatus::Completed),
                (ToolKind::Edit, ItemStatus::Completed),
            ]
        );
        assert!(items.iter().all(|item| !matches!(
            &item.content,
            ItemContent::Tool { locations, .. } if !locations.is_empty()
        )));
    }

    #[tokio::test]
    async fn the_conversation_id_is_announced_first_and_kept_on_resume() {
        let mut handle = start(Duration::ZERO, "hello");
        assert_eq!(
            next(&mut handle).await,
            Some(RunnerOutput::NativeSession {
                id: "mock-a1".into(),
                model: Some("mock-1".into()),
            })
        );

        let mut resumed = MockRunner::new(MockConfig {
            delay: Duration::ZERO,
        })
        .start(StartParams {
            agent_id: "a1".into(),
            resume: Some("earlier".into()),
            ..StartParams::default()
        })
        .unwrap();
        assert_eq!(
            next(&mut resumed).await,
            Some(RunnerOutput::NativeSession {
                id: "earlier".into(),
                model: Some("mock-1".into()),
            })
        );
    }

    #[tokio::test]
    async fn item_ids_are_unique_across_turns_and_usage_grows() {
        let mut handle = start(Duration::ZERO, "first");
        let first = collect_turn(&mut handle).await;
        handle.send_message("second").unwrap();
        let second = collect_turn(&mut handle).await;

        let mut ids = HashSet::new();
        for item in check_turn(&first).iter().chain(&check_turn(&second)) {
            assert!(ids.insert(item.id.clone()), "duplicate item id {}", item.id);
        }
        assert_eq!(ids.len(), 16);

        let (cost_1, in_1, out_1) = usage(&first);
        let (cost_2, in_2, out_2) = usage(&second);
        assert!(cost_2 > cost_1 && in_2 > in_1 && out_2 > out_1);

        match &check_turn(&second).last().unwrap().content {
            ItemContent::Text { text } => assert!(text.contains("second")),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[tokio::test]
    async fn two_agents_produce_the_same_script() {
        // The same directory, so the paths in the two scripts are the same too.
        let mut one = start(Duration::ZERO, "same");
        let first = collect_turn(&mut one).await;
        let mut two = start_at(
            one.dir.clone(),
            PermissionMode::Autonomous,
            Duration::ZERO,
            "same",
        );
        assert_eq!(first, collect_turn(&mut two).await);
    }

    #[tokio::test]
    async fn ask_mode_asks_before_the_command_and_before_the_edit() {
        let mut handle = start_in(PermissionMode::Ask, Duration::ZERO, "ask");
        let turn = run_turn(&mut handle, allow).await;
        let items = check_turn(&turn.events);

        let asked: Vec<(ToolKind, &str)> = turn
            .requests
            .iter()
            .map(|request| (request.tool_kind, request.title.as_str()))
            .collect();
        assert_eq!(
            asked,
            [
                (ToolKind::Execute, "cargo test -q"),
                (ToolKind::Edit, "calin-mock-notes.md")
            ]
        );
        // Each request points at its tool item, and its id is its own. The
        // subagent's call and its read are the two calls nobody asks about.
        let asks = |item: &&Item| {
            matches!(
                item.content,
                ItemContent::Tool {
                    tool_kind: ToolKind::Execute | ToolKind::Edit,
                    ..
                }
            )
        };
        let tool_ids: Vec<Option<String>> = items
            .iter()
            .filter(asks)
            .map(|item| Some(item.id.clone()))
            .collect();
        let pointed: Vec<Option<String>> =
            turn.requests.iter().map(|r| r.item_id.clone()).collect();
        assert_eq!(pointed, tool_ids);
        assert_ne!(turn.requests[0].id, turn.requests[1].id);
        assert_eq!(
            turn.requests[0].input,
            json!({ "command": "cargo test -q" })
        );
        // The edit says which file it writes; a command says nothing.
        assert_eq!(
            turn.access,
            [
                ToolAccess::Other,
                ToolAccess::Files {
                    paths: vec![handle.notes_path()],
                    write: true,
                },
            ]
        );

        // Allowed, the script is the one of an agent that never asks.
        let mut unattended = start_at(
            handle.dir.clone(),
            PermissionMode::Autonomous,
            Duration::ZERO,
            "ask",
        );
        assert_eq!(turn.events, collect_turn(&mut unattended).await);
    }

    #[tokio::test]
    async fn accept_edits_asks_only_before_the_command() {
        let mut handle = start_in(PermissionMode::AcceptEdits, Duration::ZERO, "edits");
        let turn = run_turn(&mut handle, allow).await;
        let kinds: Vec<ToolKind> = turn.requests.iter().map(|r| r.tool_kind).collect();
        assert_eq!(kinds, [ToolKind::Execute]);
        assert_eq!(
            tool_items(&check_turn(&turn.events)),
            [
                (ToolKind::Execute, ItemStatus::Completed),
                (ToolKind::Read, ItemStatus::Completed),
                (ToolKind::Task, ItemStatus::Completed),
                (ToolKind::Edit, ItemStatus::Completed),
            ]
        );
    }

    #[tokio::test]
    async fn a_denied_call_is_declined_and_the_turn_goes_on() {
        let mut handle = start_in(PermissionMode::Ask, Duration::ZERO, "deny the command");
        let turn = run_turn(&mut handle, |request| match request.tool_kind {
            ToolKind::Execute => (PermissionDecision::Deny, Some("not now".to_owned())),
            _ => (PermissionDecision::Allow, None),
        })
        .await;
        let items = check_turn(&turn.events);
        assert_eq!(
            tool_items(&items),
            [
                (ToolKind::Execute, ItemStatus::Declined),
                (ToolKind::Read, ItemStatus::Completed),
                (ToolKind::Task, ItemStatus::Completed),
                (ToolKind::Edit, ItemStatus::Completed),
            ]
        );
        let declined = items
            .iter()
            .find(|item| item.status == ItemStatus::Declined)
            .unwrap();
        assert!(matches!(
            &declined.content,
            ItemContent::Tool { output: Some(output), .. } if output == "not now"
        ));
        // A declined call produced no output chunks.
        assert!(!turn.events.iter().any(|body| matches!(
            body,
            EventBody::ItemDelta {
                item_id,
                delta: ItemDelta::Output { .. },
            } if *item_id == declined.id
        )));
        assert!(matches!(
            &items.last().unwrap().content,
            ItemContent::Text { text } if text.contains("deny the command")
        ));
        assert_eq!(usage(&turn.events), (0.0125, 1200, 350));
    }

    #[tokio::test]
    async fn a_decision_for_another_request_is_ignored() {
        let mut handle = start_in(PermissionMode::AcceptEdits, Duration::ZERO, "ids");
        let request = loop {
            match next(&mut handle).await {
                Some(RunnerOutput::PermissionRequest(ask)) => break ask.request,
                Some(_) => {}
                None => panic!("the agent ended before asking"),
            }
        };
        handle
            .resolve_permission("someone-else", PermissionDecision::Deny, None)
            .unwrap();
        handle
            .resolve_permission(request.id, PermissionDecision::Allow, None)
            .unwrap();
        let rest = collect_turn(&mut handle).await;
        assert!(!rest.iter().any(|body| matches!(
            body,
            EventBody::ItemCompleted { item } if item.status == ItemStatus::Declined
        )));
    }

    #[tokio::test]
    async fn interrupt_ends_the_turn_and_the_agent_takes_the_next_one() {
        let mut handle = start_in(PermissionMode::Ask, Duration::ZERO, "first");
        // The turn parks on its first request: a fixed point to interrupt at.
        let mut events = Vec::new();
        let request = loop {
            match next(&mut handle).await {
                Some(RunnerOutput::Event(body)) => events.push(body),
                Some(RunnerOutput::PermissionRequest(ask)) => break ask.request,
                Some(RunnerOutput::NativeSession { .. }) => {}
                other => panic!("unexpected output {other:?}"),
            }
        };
        handle.interrupt().unwrap();
        loop {
            match next(&mut handle).await {
                Some(RunnerOutput::Event(body)) => events.push(body),
                Some(RunnerOutput::TurnEnded) => break,
                other => panic!("unexpected output {other:?}"),
            }
        }
        // The call that was waiting is closed as failed; nothing ran after it.
        let items = check_turn(&events);
        assert_eq!(
            tool_items(&items),
            [(ToolKind::Execute, ItemStatus::Failed)]
        );
        assert_eq!(items.last().unwrap().id, request.item_id.unwrap());

        // The voided request's answer changes nothing, and the next turn is whole.
        handle
            .resolve_permission(request.id, PermissionDecision::Deny, None)
            .unwrap();
        handle.send_message("second").unwrap();
        let second = run_turn(&mut handle, allow).await;
        let items = check_turn(&second.events);
        assert_eq!(items.len(), 8);
        assert!(
            items
                .iter()
                .all(|item| item.status == ItemStatus::Completed)
        );
    }

    #[tokio::test]
    async fn interrupt_ends_a_slow_turn_promptly() {
        // An hour per chunk: only the interrupt can end this turn in time.
        let mut handle = start(Duration::from_secs(3600), "slow");
        handle.interrupt().unwrap();
        assert!(matches!(
            next(&mut handle).await,
            Some(RunnerOutput::NativeSession { .. })
        ));
        assert_eq!(next(&mut handle).await, Some(RunnerOutput::TurnEnded));
        handle.stop().unwrap();
        assert_eq!(
            next(&mut handle).await,
            Some(RunnerOutput::Exited(Exit::Ok))
        );
    }

    #[tokio::test]
    async fn stop_closes_the_waiting_call_and_exits() {
        let mut handle = start_in(PermissionMode::Ask, Duration::ZERO, "stop me");
        let mut events = Vec::new();
        loop {
            match next(&mut handle).await {
                Some(RunnerOutput::Event(body)) => events.push(body),
                Some(RunnerOutput::PermissionRequest(_)) => break,
                Some(RunnerOutput::NativeSession { .. }) => {}
                other => panic!("unexpected output {other:?}"),
            }
        }
        handle.stop().unwrap();
        loop {
            match next(&mut handle).await {
                Some(RunnerOutput::Event(body)) => events.push(body),
                Some(RunnerOutput::Exited(Exit::Ok)) => break,
                other => panic!("unexpected output {other:?}"),
            }
        }
        assert_eq!(
            tool_items(&check_turn(&events)),
            [(ToolKind::Execute, ItemStatus::Failed)]
        );
        assert_eq!(next(&mut handle).await, None);
    }

    #[tokio::test]
    async fn stop_ends_a_slow_turn_promptly() {
        // An hour per chunk: only the stop request can end this test in time.
        let mut handle = start(Duration::from_secs(3600), "slow");
        handle.stop().unwrap();
        assert!(matches!(
            next(&mut handle).await,
            Some(RunnerOutput::NativeSession { .. })
        ));
        assert_eq!(
            next(&mut handle).await,
            Some(RunnerOutput::Exited(Exit::Ok))
        );
        assert_eq!(next(&mut handle).await, None);
        assert!(matches!(
            handle.send_message("late"),
            Err(RunnerError::Gone)
        ));
    }

    #[tokio::test]
    async fn stop_between_turns_exits() {
        let mut handle = start(Duration::ZERO, "one turn");
        collect_turn(&mut handle).await;
        handle.stop().unwrap();
        assert_eq!(
            next(&mut handle).await,
            Some(RunnerOutput::Exited(Exit::Ok))
        );
    }

    #[tokio::test]
    async fn the_subagent_works_inside_the_call_that_started_it() {
        let mut handle = start(Duration::ZERO, "delegate");
        let turn = run_turn(&mut handle, allow).await;
        // Items are numbered in the order they start: 1 and 2 are the
        // opening prose, 3 the command, 4 the call that runs the subagent.
        assert_eq!(
            trace(&turn.outputs),
            [
                "start mock-3-1",
                "completed mock-3-1",
                "start mock-3-2",
                "completed mock-3-2",
                "start mock-3-3",
                "completed mock-3-3",
                "start mock-3-4",
                "+mock-3-4",
                "mock-3-4: start mock-3-5",
                "mock-3-4: completed mock-3-5",
                "mock-3-4: start mock-3-6",
                "mock-3-4: completed mock-3-6",
                "-mock-3-4 ok",
                "completed mock-3-4",
                "start mock-3-7",
                "completed mock-3-7",
                "start mock-3-8",
                "completed mock-3-8",
            ]
        );
        let items = check_turn(&turn.events);
        let call = items.iter().find(|item| item.id == "mock-3-4").unwrap();
        assert_eq!(
            call.content,
            ItemContent::Tool {
                name: "Agent".into(),
                tool_kind: ToolKind::Task,
                title: SUBAGENT_TASK.into(),
                input: json!({
                    "description": SUBAGENT_TASK,
                    "subagent_type": SUBAGENT_ROLE,
                    "prompt": SUBAGENT_PROMPT,
                }),
                output: Some("The notes file holds 0 lines.\n".into()),
                locations: vec![],
            }
        );
        // Totals are the agent's: the subagent reports none.
        assert!(!turn.outputs.iter().any(|output| matches!(
            output,
            RunnerOutput::SubagentEvent {
                body: EventBody::Usage { .. } | EventBody::PlanUpdated { .. },
                ..
            }
        )));

        // The next turn's subagent is another one, and finds the first note.
        handle.send_message("again").unwrap();
        let second = run_turn(&mut handle, allow).await;
        let lines = trace(&second.outputs);
        assert!(lines.contains(&"+mock-3-12".to_owned()), "{lines:?}");
        assert!(second.events.iter().any(|body| matches!(
            body,
            EventBody::ItemCompleted { item } if matches!(
                &item.content,
                ItemContent::Tool { name, output: Some(output), .. }
                    if name == "Read" && output == "1 notes from earlier turns\n"
            )
        )));
    }

    /// An agent on a channel that holds one output: it cannot run more than
    /// two outputs ahead of the test, so an input sent on seeing the
    /// subagent start reaches it while the subagent is still at work.
    struct Narrow {
        outputs: tokio::sync::mpsc::Receiver<RunnerOutput>,
        inputs: tokio::sync::mpsc::UnboundedSender<RunnerInput>,
        _dir: tempfile::TempDir,
    }

    impl Narrow {
        fn start(prompt: &str) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let (outputs_tx, outputs) = tokio::sync::mpsc::channel(1);
            let (inputs, inputs_rx) = tokio::sync::mpsc::unbounded_channel();
            let io = AgentIo {
                outputs: outputs_tx,
                inputs: inputs_rx,
            };
            let params = StartParams {
                prompt: prompt.into(),
                cwd: dir.path().to_owned(),
                permission_mode: PermissionMode::Autonomous,
                start_seq: 3,
                ..StartParams::default()
            };
            MockRunner::new(MockConfig {
                delay: Duration::ZERO,
            })
            .spawn(params, io);
            Self {
                outputs,
                inputs,
                _dir: dir,
            }
        }

        async fn next(&mut self) -> Option<RunnerOutput> {
            tokio::time::timeout(TIMEOUT, self.outputs.recv())
                .await
                .expect("the mock agent went quiet")
        }

        /// The turn's outputs up to the start of the subagent, then `input`
        /// is sent, then the rest up to `last`, which is left out.
        async fn cut_in_the_subagent(
            &mut self,
            input: RunnerInput,
            last: RunnerOutput,
        ) -> Vec<RunnerOutput> {
            let mut outputs = Vec::new();
            let mut sent = Some(input);
            loop {
                let output = self.next().await.expect("the agent ended early");
                if output == last {
                    return outputs;
                }
                let started = matches!(output, RunnerOutput::SubagentStarted(_));
                if !matches!(output, RunnerOutput::NativeSession { .. }) {
                    outputs.push(output);
                }
                if started && let Some(input) = sent.take() {
                    self.inputs.send(input).unwrap();
                }
            }
        }
    }

    /// What a turn cut inside the subagent must look like, whichever of the
    /// subagent's first outputs the cut came after.
    fn check_cut_subagent(outputs: &[RunnerOutput]) {
        let lines = trace(outputs);
        let from_call = lines
            .iter()
            .position(|line| line == "start mock-3-4")
            .unwrap();
        let tail: Vec<&str> = lines[from_call..].iter().map(String::as_str).collect();
        let cut_before_the_read = ["start mock-3-4", "+mock-3-4", "-mock-3-4 cut"];
        let cut_in_the_read = [
            "start mock-3-4",
            "+mock-3-4",
            "mock-3-4: start mock-3-5",
            "mock-3-4: failed mock-3-5",
            "-mock-3-4 cut",
        ];
        let subagent = &tail[..tail.len() - 1];
        assert!(
            subagent == cut_before_the_read || subagent == cut_in_the_read,
            "{tail:?}"
        );
        // The call that ran it fails last, and is the agent's own.
        assert_eq!(tail.last(), Some(&"failed mock-3-4"));
        let events: Vec<EventBody> = outputs
            .iter()
            .filter_map(|output| match output {
                RunnerOutput::Event(body) | RunnerOutput::SubagentEvent { body, .. } => {
                    Some(body.clone())
                }
                _ => None,
            })
            .collect();
        check_turn(&events);
    }

    #[tokio::test]
    async fn an_interrupt_inside_the_subagent_ends_it_then_its_call_then_the_turn() {
        let mut agent = Narrow::start("first");
        let outputs = agent
            .cut_in_the_subagent(RunnerInput::Interrupt, RunnerOutput::TurnEnded)
            .await;
        check_cut_subagent(&outputs);

        // The agent lives on: the next turn runs its subagent to the end.
        agent
            .inputs
            .send(RunnerInput::UserMessage("second".into()))
            .unwrap();
        let mut second = Vec::new();
        loop {
            match agent.next().await {
                Some(RunnerOutput::TurnEnded) => break,
                // Reported for the process, not part of the turn's work.
                Some(RunnerOutput::Limits(_)) => {}
                Some(output) => second.push(output),
                None => panic!("the agent ended early"),
            }
        }
        let lines = trace(&second);
        // The cut turn used the ids up to 4 or 5.
        let ended = lines.iter().filter(|line| line.ends_with(" ok")).count();
        assert_eq!(ended, 1, "{lines:?}");
        assert!(!lines.iter().any(|line| line.contains("failed")));
    }

    #[tokio::test]
    async fn a_stop_inside_the_subagent_ends_it_then_its_call_then_the_agent() {
        let mut agent = Narrow::start("stop me");
        let outputs = agent
            .cut_in_the_subagent(RunnerInput::Stop, RunnerOutput::Exited(Exit::Ok))
            .await;
        check_cut_subagent(&outputs);
        // No `TurnEnded` on the way out, and nothing after the exit.
        assert_eq!(agent.next().await, None);
    }

    #[test]
    fn long_requests_are_quoted_in_part() {
        let long = "x".repeat(QUOTE_CHARS + 50);
        let quoted = quote(&long);
        assert_eq!(quoted.chars().count(), QUOTE_CHARS + 3);
        assert_eq!(quote("  hello \n world"), "hello");
        assert_eq!(quote(""), "");
    }
}
