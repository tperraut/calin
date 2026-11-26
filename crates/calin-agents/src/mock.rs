//! A scripted agent with no network and no randomness, for tests, demos and
//! `CALIN_DRY_RUN=1`.

use std::collections::VecDeque;
use std::time::Duration;

use calin_proto::{
    EventBody, Item, ItemContent, ItemDelta, ItemStatus, PlanEntry, PlanStatus, ToolKind,
};
use serde_json::json;
use tokio::sync::mpsc::error::TryRecvError;

use crate::runner::{
    AgentHandle, AgentIo, Exit, Runner, RunnerError, RunnerInput, RunnerOutput, StartParams,
};

pub const MOCK_RUNNER_ID: &str = "mock";

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
        Some("mock-1".to_owned())
    }

    fn start(&self, params: StartParams) -> Result<AgentHandle, RunnerError> {
        let (handle, io) = AgentHandle::channel();
        let agent = MockAgent {
            io,
            delay: self.config.delay,
            start_seq: params.start_seq,
            items: 0,
            turns: 0,
            queued: VecDeque::new(),
        };
        tokio::spawn(agent.run(params.prompt));
        Ok(handle)
    }
}

/// Why a turn ended early.
enum Interrupt {
    /// The orchestrator asked for a stop.
    Stopped,
    /// The orchestrator dropped the handle; nobody is listening.
    Detached,
}

struct MockAgent {
    io: AgentIo,
    delay: Duration,
    start_seq: u64,
    items: u64,
    turns: u64,
    /// Messages that arrived mid-turn. The orchestrator does not send any,
    /// but a queue costs less than a lost message.
    queued: VecDeque<String>,
}

impl MockAgent {
    async fn run(mut self, prompt: String) {
        let mut next = Some(prompt);
        while let Some(text) = next.take() {
            match self.turn(&text).await {
                Ok(()) => {
                    if self.io.outputs.send(RunnerOutput::TurnEnded).await.is_err() {
                        return;
                    }
                }
                Err(Interrupt::Stopped) => break,
                Err(Interrupt::Detached) => return,
            }
            next = self.next_message().await;
        }
        let _ = self.io.outputs.send(RunnerOutput::Exited(Exit::Ok)).await;
    }

    /// The next user message, or `None` on a stop request or a dropped handle.
    async fn next_message(&mut self) -> Option<String> {
        if let Some(text) = self.queued.pop_front() {
            return Some(text);
        }
        match self.io.inputs.recv().await? {
            RunnerInput::UserMessage(text) => Some(text),
            RunnerInput::Stop => None,
        }
    }

    async fn turn(&mut self, user_text: &str) -> Result<(), Interrupt> {
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
            &[
                "running 3 tests\n",
                "...\n",
                "test result: ok. 3 passed; 0 failed\n",
            ],
        )
        .await?;
        self.plan(1).await?;

        self.tool(
            "Edit",
            ToolKind::Edit,
            "src/lib.rs",
            json!({
                "file_path": "src/lib.rs",
                "old_string": "todo!()",
                "new_string": "Ok(())",
            }),
            &[],
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
                "The tests pass and the change is in src/lib.rs.",
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
    async fn streamed(&mut self, prose: Prose, chunks: &[&str]) -> Result<(), Interrupt> {
        let id = self.next_item_id();
        self.emit(EventBody::ItemStarted {
            item: Item {
                id: id.clone(),
                status: ItemStatus::Running,
                content: prose.content(String::new()),
            },
        })
        .await?;
        for chunk in chunks {
            self.emit(EventBody::ItemDelta {
                item_id: id.clone(),
                delta: prose.delta((*chunk).to_owned()),
            })
            .await?;
        }
        self.emit(EventBody::ItemCompleted {
            item: Item {
                id,
                status: ItemStatus::Completed,
                content: prose.content(chunks.concat()),
            },
        })
        .await
    }

    /// A tool call whose output arrives in `output` chunks.
    async fn tool(
        &mut self,
        name: &str,
        tool_kind: ToolKind,
        title: &str,
        input: serde_json::Value,
        output: &[&str],
    ) -> Result<(), Interrupt> {
        let id = self.next_item_id();
        let content = |output: Option<String>| ItemContent::Tool {
            name: name.to_owned(),
            tool_kind,
            title: title.to_owned(),
            input: input.clone(),
            output,
        };
        self.emit(EventBody::ItemStarted {
            item: Item {
                id: id.clone(),
                status: ItemStatus::Running,
                content: content(None),
            },
        })
        .await?;
        for chunk in output {
            self.emit(EventBody::ItemDelta {
                item_id: id.clone(),
                delta: ItemDelta::Output {
                    text: (*chunk).to_owned(),
                },
            })
            .await?;
        }
        self.emit(EventBody::ItemCompleted {
            item: Item {
                id,
                status: ItemStatus::Completed,
                content: content(Some(output.concat())),
            },
        })
        .await
    }

    /// The full plan with the first `done` steps completed and the next one in progress.
    async fn plan(&mut self, done: usize) -> Result<(), Interrupt> {
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

    async fn emit(&mut self, body: EventBody) -> Result<(), Interrupt> {
        self.pause().await?;
        self.io
            .outputs
            .send(RunnerOutput::Event(body))
            .await
            .map_err(|_| Interrupt::Detached)
    }

    /// Waits out the delay, ending early on a stop request.
    async fn pause(&mut self) -> Result<(), Interrupt> {
        if self.delay.is_zero() {
            loop {
                match self.io.inputs.try_recv() {
                    Ok(input) => self.interrupted_by(input)?,
                    Err(TryRecvError::Empty) => return Ok(()),
                    Err(TryRecvError::Disconnected) => return Err(Interrupt::Detached),
                }
            }
        }
        let sleep = tokio::time::sleep(self.delay);
        tokio::pin!(sleep);
        loop {
            tokio::select! {
                () = &mut sleep => return Ok(()),
                input = self.io.inputs.recv() => match input {
                    Some(input) => self.interrupted_by(input)?,
                    None => return Err(Interrupt::Detached),
                },
            }
        }
    }

    fn interrupted_by(&mut self, input: RunnerInput) -> Result<(), Interrupt> {
        match input {
            RunnerInput::UserMessage(text) => {
                self.queued.push_back(text);
                Ok(())
            }
            RunnerInput::Stop => Err(Interrupt::Stopped),
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

    fn start(delay: Duration, prompt: &str) -> AgentHandle {
        MockRunner::new(MockConfig { delay })
            .start(StartParams {
                session_id: "s1".into(),
                agent_id: "a1".into(),
                prompt: prompt.into(),
                resume: false,
                start_seq: 3,
            })
            .unwrap()
    }

    async fn next(handle: &mut AgentHandle) -> Option<RunnerOutput> {
        tokio::time::timeout(TIMEOUT, handle.next_output())
            .await
            .expect("the mock agent went quiet")
    }

    /// Collects the events of one turn, up to `TurnEnded`.
    async fn collect_turn(handle: &mut AgentHandle) -> Vec<EventBody> {
        let mut events = Vec::new();
        loop {
            match next(handle).await {
                Some(RunnerOutput::Event(body)) => events.push(body),
                Some(RunnerOutput::TurnEnded) => return events,
                other => panic!("unexpected output {other:?}"),
            }
        }
    }

    /// Checks the item lifecycle of a turn and returns its completed items in order.
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
                    assert_eq!(item.status, ItemStatus::Completed);
                    let final_text = match &item.content {
                        ItemContent::Text { text } | ItemContent::Reasoning { text } => {
                            text.clone()
                        }
                        ItemContent::Tool { output, .. } => output.clone().unwrap_or_default(),
                    };
                    assert_eq!(streamed, final_text, "deltas of {} do not add up", item.id);
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

    #[tokio::test]
    async fn a_turn_is_well_formed() {
        let mut handle = start(Duration::ZERO, "Fix the flaky login test\nsecond line");
        let events = collect_turn(&mut handle).await;
        let items = check_turn(&events);

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
        let mut one = start(Duration::ZERO, "same");
        let mut two = start(Duration::ZERO, "same");
        assert_eq!(collect_turn(&mut one).await, collect_turn(&mut two).await);
    }

    #[tokio::test]
    async fn stop_ends_a_slow_turn_promptly() {
        // An hour per chunk: only the stop request can end this test in time.
        let mut handle = start(Duration::from_secs(3600), "slow");
        handle.stop().unwrap();
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
