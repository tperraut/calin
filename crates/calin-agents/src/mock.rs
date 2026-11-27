//! A scripted agent with no network and no randomness, for tests, demos and
//! `CALIN_DRY_RUN=1`.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::Duration;

use calin_proto::{
    EventBody, Item, ItemContent, ItemDelta, ItemStatus, PermissionDecision, PermissionMode,
    PermissionRequest, PlanEntry, PlanStatus, ToolKind,
};
use serde_json::json;
use tokio::sync::mpsc::error::TryRecvError;

use crate::runner::{
    AgentHandle, AgentIo, Exit, Runner, RunnerError, RunnerInput, RunnerOutput, StartParams,
};

pub const MOCK_RUNNER_ID: &str = "mock";

const MOCK_MODEL: &str = "mock-1";

/// The file the scripted edit appends to, relative to the working directory.
/// A real file, so a dry run has something to show as a change and a diff.
const EDITED_FILE: &str = "calin-mock-notes.md";

const DEFAULT_DELAY: Duration = Duration::from_millis(40);

/// How much of the user's text the scripted answer quotes back.
const QUOTE_CHARS: usize = 120;

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
            queued: VecDeque::new(),
        };
        tokio::spawn(agent.run(native_session, params.prompt));
        Ok(handle)
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
    /// Messages that arrived mid-turn; each starts a turn of its own.
    queued: VecDeque<String>,
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
            if matches!(cut, Some(Cut::Detached)) || !self.close_open_item().await {
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

    /// Completes the item a cut turn left running: a tool call as failed,
    /// prose with what was written. False when nobody is listening.
    async fn close_open_item(&mut self) -> bool {
        let Some(mut item) = self.open.take() else {
            return true;
        };
        item.status = match item.content {
            ItemContent::Tool { .. } => ItemStatus::Failed,
            ItemContent::Text { .. } | ItemContent::Reasoning { .. } => ItemStatus::Completed,
        };
        let completed = RunnerOutput::Event(EventBody::ItemCompleted { item });
        self.io.outputs.send(completed).await.is_ok()
    }

    async fn turn(&mut self, user_text: &str) -> Result<(), Cut> {
        self.turns += 1;
        let quote = quote(user_text);

        self.streamed(
            Prose::Reasoning,
            &[
                "The request is: ",
                &quote,
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
        self.plan(0).await?;

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
        self.plan(1).await?;

        let locations: Vec<String> = self
            .edited
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect();
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
        .await?;
        self.plan(2).await?;

        self.streamed(
            Prose::Text,
            &[
                "Done. ",
                "You asked: \"",
                &quote,
                "\". ",
                "The tests pass and the change is in calin-mock-notes.md.",
            ],
        )
        .await?;
        self.plan(3).await?;

        self.emit(EventBody::Usage {
            cost_usd: 0.0125 * self.turns as f64,
            tokens_in: 1200 * self.turns,
            tokens_out: 350 * self.turns,
        })
        .await
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
        if self.asks_before(tool_kind) {
            let request = PermissionRequest {
                id: format!("{id}-permission"),
                tool_name: name.to_owned(),
                tool_kind,
                title: title.to_owned(),
                input,
                item_id: Some(id),
            };
            if let (PermissionDecision::Deny, message) = self.ask(request).await? {
                if let Some(Item {
                    content: ItemContent::Tool { output, .. },
                    ..
                }) = &mut self.open
                {
                    *output = message;
                }
                return self.complete_item(ItemStatus::Declined).await;
            }
        }
        if let (Some(note), Some(path)) = (note, self.edited.clone())
            && let Err(reason) = append_line(path, note).await
        {
            if let Some(Item {
                content: ItemContent::Tool { output, .. },
                ..
            }) = &mut self.open
            {
                *output = Some(reason);
            }
            return self.complete_item(ItemStatus::Failed).await;
        }
        for chunk in output {
            self.delta(ItemDelta::Output {
                text: (*chunk).to_owned(),
            })
            .await?;
        }
        if let Some(Item {
            content: ItemContent::Tool { output, .. },
            ..
        }) = &mut self.open
        {
            // A completed call always has an output, even an empty one.
            output.get_or_insert_default();
        }
        self.complete_item(ItemStatus::Completed).await
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
        request: PermissionRequest,
    ) -> Result<(PermissionDecision, Option<String>), Cut> {
        let id = request.id.clone();
        self.io
            .outputs
            .send(RunnerOutput::PermissionRequest(request))
            .await
            .map_err(|_| Cut::Detached)?;
        loop {
            match self.io.inputs.recv().await.ok_or(Cut::Detached)? {
                RunnerInput::PermissionDecision {
                    request_id,
                    decision,
                    message,
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
        self.io
            .outputs
            .send(RunnerOutput::Event(body))
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

#[derive(Clone, Copy)]
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
        events: Vec<EventBody>,
        requests: Vec<PermissionRequest>,
    }

    async fn run_turn(
        handle: &mut AgentHandle,
        mut answer: impl FnMut(&PermissionRequest) -> (PermissionDecision, Option<String>),
    ) -> Turn {
        let mut turn = Turn {
            events: Vec::new(),
            requests: Vec::new(),
        };
        loop {
            match next(handle).await {
                Some(RunnerOutput::Event(body)) => turn.events.push(body),
                Some(RunnerOutput::PermissionRequest(request)) => {
                    let (decision, message) = answer(&request);
                    handle
                        .resolve_permission(request.id.clone(), decision, message)
                        .unwrap();
                    turn.requests.push(request);
                }
                Some(RunnerOutput::NativeSession { .. }) => {}
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
                ItemContent::Tool { .. } => "tool",
            })
            .collect();
        assert_eq!(kinds, ["reasoning", "text", "execute", "edit", "text"]);

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
        assert_eq!(locations, [&vec![], &vec![edited]]);
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
                (ToolKind::Edit, ItemStatus::Declined)
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
                (ToolKind::Edit, ItemStatus::Failed)
            ]
        );
        assert_eq!(items.len(), 5);
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
                (ToolKind::Edit, ItemStatus::Completed)
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
        assert_eq!(ids.len(), 10);

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
        // Each request points at its tool item, and its id is its own.
        let tool_ids: Vec<Option<String>> = items
            .iter()
            .filter(|item| matches!(item.content, ItemContent::Tool { .. }))
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
                (ToolKind::Edit, ItemStatus::Completed)
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
                (ToolKind::Edit, ItemStatus::Completed)
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
                delta: ItemDelta::Output { .. },
                ..
            }
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
                Some(RunnerOutput::PermissionRequest(request)) => break request,
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
                Some(RunnerOutput::PermissionRequest(request)) => break request,
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
        assert_eq!(items.len(), 5);
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

    #[test]
    fn long_requests_are_quoted_in_part() {
        let long = "x".repeat(QUOTE_CHARS + 50);
        let quoted = quote(&long);
        assert_eq!(quoted.chars().count(), QUOTE_CHARS + 3);
        assert_eq!(quote("  hello \n world"), "hello");
        assert_eq!(quote(""), "");
    }
}
