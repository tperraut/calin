# Workflows

Status: accepted · 2026-11-27 · milestone M4

A workflow is a small graph that decides what a session does after it starts: which prompts the
agent gets, which commands check its work, when to loop, when to stop and ask a person, and when
to open pull requests. Without a workflow a session behaves as it does today: one agent turn on
the prompt, then review.

This document defines the file format and the engine. How the server drives a session with it
(events, routes, UI) is in section 6.

## 1. Where workflows live

- `<home repo>/.calin/workflows/*.yaml`. The file name without extension is the workflow's id.
- One workflow is built in and needs no file: `quick`, the behavior without a workflow.
- A file that fails validation is listed with its problem and cannot be selected. It never
  breaks the list.

## 2. File format

```yaml
version: 1
name: Implement, test, review          # shown in the UI; the id is the file name
description: Optional one-liner.
budget_usd: 5                          # optional; the run stops when the session's cost passes it
nodes:
  - { id: start, type: start }
  - { id: implement, type: agent, prompt: "{{task}}" }
  - { id: tests, type: check }                              # each changed repo's own checks
  - { id: retry, type: loop, max: 3 }
  - id: fix
    type: agent
    prompt: |
      The checks failed (attempt {{nodes.retry.iteration}} of {{nodes.retry.max}}):
      {{nodes.tests.output}}
  - { id: review, type: agent, fresh: true, prompt: "Review the changes on this branch.", verdicts: [approve, changes] }
  - { id: gate, type: gate, message: "Open pull requests?" }
  - { id: ship, type: ship }
  - { id: ok, type: end, status: success }
  - { id: gave-up, type: end, status: failed }
edges:
  - { from: start, to: implement }
  - { from: implement.done, to: tests }
  - { from: tests.pass, to: review }
  - { from: tests.fail, to: retry }
  - { from: retry.repeat, to: fix }
  - { from: retry.exhausted, to: gave-up }
  - { from: fix.done, to: tests }
  - { from: review.approve, to: gate }
  - { from: review.changes, to: retry }
  - { from: gate.approve, to: ship }
  - { from: ship.done, to: ok }
```

Rules that hold for every file:

- `version` must be `1`. Unknown top-level keys and unknown keys on a node are errors, so a typo
  never silently changes behavior.
- Node ids match `[a-z][a-z0-9-]*`, are unique, and are at most 40 characters.
- An edge's `from` is `node` (meaning that node's only output port) or `node.port`; `to` is a node
  id. A port has at most one outgoing edge. A node may have any number of incoming edges.
- There is exactly one `start` node and at least one `end` node. Every node is reachable from
  `start`.
- Every cycle leaves a `loop` node through its `repeat` port. Loop counters never reset during a
  run, so every run is finite. (A cycle that only passes a loop through `exhausted` would not be.)
- A port with no outgoing edge ends the run when it is taken: as `failed` for the failure ports
  listed below, as `success` otherwise.

## 3. Node types

| Type | Fields | Ports | Outputs |
| --- | --- | --- | --- |
| `start` | | `next` | |
| `end` | `status`: `success` (default) or `failed` | none | |
| `agent` | `prompt` (required), `fresh`, `verdicts`, `runner`, `model` | `done`, `failed`; or one port per verdict, plus `failed` | `summary`, `verdict` |
| `check` | `command`, `repo`, `timeout_seconds` (default 600) | `pass`, `fail` | `output`, `exit_code` |
| `loop` | `max` (required, 1 to 20) | `repeat`, `exhausted` | `iteration`, `max` |
| `gate` | `message` (required) | `approve`, `reject` | `comment` |
| `ask` | `question` (required), `options` | `answered` | `answer` |
| `parallel` | `branches`: a list of `{ id, prompt, repos? }` (2 to 4) | `done`, `failed` | `summary` |
| `commit` | `message` (required) | `done`, `nothing`, `failed` | |
| `ship` | `title`, `draft` (default true) | `done`, `failed` | `urls` |

Failure ports are `failed`, `fail`, `exhausted` and `reject`, by name: a verdict called `reject`
is a failure port too. All outputs are strings. `timeout_seconds` is 1 to 86,400.

- **`agent`** gives the session's lead agent a turn with `prompt`. It leaves through `done` when
  the turn ends and through `failed` when the agent fails. `summary` is the agent's last text.
  - With `fresh: true` the prompt goes to a new child session instead, which starts from the
    session's current commits and reports back. `summary` is the child's report. Its work stays on
    its own branch; the next lead turn is told the branch names, as with any dispatched child.
  - With `verdicts`, the agent is told to end its answer with one of them on its own line as
    `VERDICT: <word>`, and the node leaves through the port of that name. An answer without a
    recognizable verdict leaves through `failed`. `verdict` holds the word.
  - `runner` and `model` apply only with `fresh: true`.
- **`check`** runs shell commands in worktrees. With `command`, it runs that command in `repo`'s
  worktree (the primary repo when `repo` is absent). Without `command`, it runs each changed
  `write` repo's own `checks` from the home repo's `repos.yaml`, in the order of the session's repo
  set, stopping at the first failure; a session with no changed repo that declares checks passes.
  `output` is the combined output of the last command that ran, capped at 16 KiB from the end.
- **`loop`** counts. Each time it is entered it adds one to `iteration` and leaves through
  `repeat`, until `iteration` would exceed `max`, when it leaves through `exhausted`.
- **`gate`** asks the user to approve or reject, with an optional comment. The session waits.
- **`ask`** asks the user a question. The session waits.
- **`parallel`** dispatches one child session per branch and waits for all of them. It leaves
  through `done` when every child reaches review and through `failed` when any child fails.
  `summary` is the reports joined, each under its branch id.
- **`commit`** commits uncommitted work in every `write` repo of the session with `message`.
  `nothing` when no repo had anything to commit.
- **`ship`** opens or updates the session's pull requests, as the Ship action does.

## 4. Templates

A string field marked as a template (`prompt`, `command`, `message`, `question`, `title`, and each
branch's `prompt`) may contain:

- `{{task}}`: the session's prompt.
- `{{session.title}}`, `{{session.branch}}`.
- `{{nodes.<id>.<output>}}`: an output of a node that has already run, from its newest run. An
  output that does not exist yet, or that the newest run did not produce, is the empty string.

Whitespace inside the braces is ignored. A `{{` that does not form one of these is a validation
error. Substituted text is never re-scanned for templates. Templates are checked at load time:
`<id>` must be a node of the file and `<output>` an output of its type.

## 5. The engine

The engine is a pure state machine with no I/O. Given a validated workflow and the session's task
it produces **actions** for the orchestrator and consumes **results**.

```
start(workflow, context)         -> (state, [action])
advance(state, result)           -> (state, [action])
```

- Actions: run an agent turn (node, rendered prompt, fresh or not, verdicts, runner, model), run a
  check (node, command and repo, or "repo defaults", timeout), ask a gate, ask a question, start
  parallel branches, commit (rendered message), ship (rendered title, draft), and finish (status).
- A result names the node it answers, the port it leaves through and its outputs.
- At most one node is active at a time; `parallel` is one node whose branches the orchestrator runs
  together.
- The state is serializable. Replaying the same results from `start` yields the same state and
  actions, which is how a run resumes after a restart.
- A result for a node that is not active, or through a port the node does not have, is an error
  and leaves the state unchanged.
- The budget is not the engine's concern: the orchestrator stops a run that passes it.

## 6. A workflow run in a session

- `CreateSessionRequest` gains an optional `workflow` id. Absent means `quick`.
- The run is recorded in the session's log: a `workflow_started` event with the workflow's id,
  name and its graph (nodes and edges, so the picture is right even if the file changes later),
  then a `workflow_node` event each time a node starts and each time it finishes (node id, port,
  a short outcome), then `workflow_finished` with the status. Agent turns, checks, questions and
  pull requests keep producing their usual events; a check appears as a tool item of kind
  `execute` with no agent.
- While a run is active the session is `running`, or `waiting` at a gate or question. When the
  run finishes the session goes to `review` (or to `shipping` if a `ship` node opened pull
  requests). A failed run still ends in `review`: the user decides what to do with the work.
- A user message during a run is delivered to the lead agent at the next turn boundary and does
  not change the run's position. Interrupt ends the current agent turn and fails that node. Stop
  ends the run and the session.
- After the run, the session is an ordinary session: further messages start plain turns.
- Budget: when the session tree's cost passes `budget_usd`, the current node is interrupted, the
  run finishes as `failed` with the reason, and the session goes to `review`.
- Routes: `GET /workflows` lists the built-in and the home repo's workflows with their graphs and
  problems; `GET /sessions/{id}/workflow` returns the run: the graph and each node's state.
- UI: a workflow select in the composer; a Workflow view beside Graph and Timeline showing the
  graph with the active node, taken edges, loop counters and each finished node's outcome. An
  editor for workflow files is a later step.
