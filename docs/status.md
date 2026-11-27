# Status

Milestones are defined in [the design spec](specs/2026-11-26-calin-design.md#11-milestones).
Update this file in the same commit that finishes an item.

## M0: Skeleton (done 2026-11-27)

- [x] Rust workspace, toolchain pin, Makefile with the validation gate
- [x] `calin-proto`: wire types, REST and WebSocket contract, TypeScript generation
- [x] Web app scaffold: Solid 2, Vite+, Bun
- [x] `calin-core`: store (SQLite index + NDJSON logs), event bus, session manager
- [x] `calin-agents`: runner interface and the mock runner
- [x] `calin-server` and `calin-cli`: REST, WebSocket with replay, embedded web app, `calin serve`
- [x] Web app: session list, new session, live transcript with reconnect and replay
- [x] End to end, in a real browser against the real binary: a mock session streams, survives a
      reload with an identical transcript, takes a second turn, and can be stopped
- [x] Scrubbed Claude Code recordings in `fixtures/` (ahead of M1)

Known gaps carried forward:

- The WebSocket `error` frame names no topic, so the web app cannot tell which subscribe failed.
- `session_upserted` is sent on creation and on status changes only, so `head_seq` and
  `updated_at` in the session list lag in between.
- Stopping a session leaves its running items open; the web app should show them as interrupted.
- Log lines are flushed but not fsynced: they survive a crash of the process, not a power cut.
- Replay loads a session's whole backlog into memory.
- The graph spike is done (`spikes/graph-layout/README.md`): SVG with a small tree layout, no ELK
  for the session graph. It was measured in a headless browser only, not on a phone.

## M1: One session, real agent, real repos (done 2026-11-27)

Sessions take a repo set from the start, so M1 already creates one worktree per attached repo and
shows changes grouped by repo. What stays in M2 is the home repo's repo list, attaching a repo
mid-session, and pull requests.

- [x] Contract: repos, repo set, permissions, interrupt, changes, diffs, files, terminal socket
- [x] `calin-git`: worktrees, changes against a base, file diff, tree, safe file read and write
- [x] `calin-term`: pseudo-terminal library
- [x] `calin-agents`: Claude Code runner over stream-json, tested against the recorded fixtures;
      mock runner asks for permission
- [x] Core and server: repo registry, repo set and worktrees, permission flow, interrupt, continue
      after stop or failure, file-change events, all new routes, terminal socket
- [x] Web: repo picker, permission mode, permission cards, interrupt
- [x] Web: Changes with diff view, Files with editor, Terminal
- [x] A real Claude Code session through the server's API: worktree on its own branch, a
      permission request allowed over the API, the edit in changes and diff, a second turn
- [x] `make e2e` covering repos, permissions, changes, files and the terminal with the mock runner
- [x] The same real session driven from the web app

Known gaps carried forward from M1:

- Claude Code 2.1.332 has no `TodoWrite` tool, so no plan is shown for real sessions yet. Find
  what the CLI uses for task lists now and map it.
- A tool call cut short by an interrupt is recorded as `failed`; there is no `interrupted` status.
- A CLI version other than the recorded one only logs a warning.
- `file_changed` for a rename carries no old path.
- Output produced inside a subagent is attributed to the lead agent until M3.
- Idle worktrees are never reclaimed yet.
- Branch names are slugs of the whole first prompt line and can be long.
- An agent that edits files through its shell instead of an edit tool produces no `file_changed`
  event. The Changes list is still right (it asks git), but the tab's count badge, which counts
  events, is not.
- In the Changes list a long branch name squeezes the repo name; the repo name should win.
- A permission card is listed after the tool call it belongs to; they should be one card.
- A 409 from saving a file does not say which of its three causes it was, except in its message.
- Real-agent runs are checked by hand; `make e2e` covers the mock runner only.

## M2: Home repo and delivery (done 2026-11-27, except the check against real GitHub)

- [x] Contract: settings, repo catalog from the home repo, clone, repo sets, attach mid-session,
      the tool endpoint for agents, ship and pull requests
- [x] Core and server: settings, `.calin/repos.yaml`, scan and clone, attach mid-session, path
      policy for attached repos with directory grants, setup command, the tool endpoint (MCP over
      HTTP), session briefing in the agent's system prompt
- [x] Web: settings, catalog with clone and sets, attach from the session page, Markdown in agent
      text, permission prompt merged into its tool card
- [x] Delivery: commit, push and one linked pull request per changed repo; watching state, checks
      and reviews; "Send back to the agent"
- [x] `make e2e` covers the home repo catalog, attach mid-session and shipping in dry run
- [x] A real Claude Code session attached a second repo through the tool endpoint and edited it
- [ ] Delivery against real GitHub. Everything was tested with a stand-in for `gh` and local bare
      remotes. A first real run should check: `gh auth status` exit codes (offline currently reads
      as "not signed in"), the shapes of `statusCheckRollup` and `reviewDecision`, that
      `gh pr create` prints only the URL, repo resolution from a worktree, and drafts on plans
      that refuse them. Needs a repository the owner agrees to open test pull requests in.

Known gaps carried forward from M2:

- Real Claude sessions show no plan: the CLI withholds its task-list tools from current models
  unless `CLAUDE_CODE_ENABLE_TODO_TOOLS=1`. The mapping exists; Calin does not set the variable.
- Nothing marks the start or end of a ship, so the web app waits for pull request or error
  events and gives up after 30 seconds of silence.
- `error` events name a repo only in prose.
- `git push` and commit hooks run without a timeout.
- The push remote is always `origin`; forks are not handled.
- `PUT /settings` errors name no field; `clone_root` cannot tell a stored value from the default.
- The agent prefers its shell over file tools in a freshly attached repo unless told otherwise;
  the session briefing tells it, and each shell command there still waits for approval in `ask`
  and `accept_edits`.

## M3: Orchestration graph (done 2026-11-27)

- [x] Contract: messages, questions, child sessions, graph and timeline
- [x] Subagents are agents of their own: spawned, attributed and closed, including the ones the
      CLI runs in the background
- [x] Child sessions through the tool endpoint (`dispatch`, `report`), notes (`send_message`),
      questions (`ask_user`); a child forks from its parent's commit and reports back; stop cascades
- [x] Graph (with "as of" a moment) and timeline for a whole session tree, from index projections
- [x] Web: SVG graph with a stable tree layout, timeline, scrubber and replay, question bar,
      subagent grouping and agent filter in the transcript, child sessions nested in the list
- [x] File changes made through the shell are recorded at turn end
- [x] `make e2e` covers the graph and timeline against the real server (33 checks)
- [x] On the real CLI: a lead dispatched a child, the child worked and reported, the lead merged
      the child's branch

Known gaps carried forward from M3:

- The mock runner cannot dispatch, send notes or ask, so `make e2e` covers subagents but not
  child sessions, notes or questions. Those are covered by core tests and one real run.
- On a busy session the note, report and change edges are wide arcs that tangle beside the tree.
- A child that is stopped, or failed by a server restart, sends no report: its parent is not told.
- Nothing has run on a real phone; the graph's frame rates were measured in a headless browser.
- A stopped or failed turn is not scanned for shell-made changes.
- Timeline marks and arrow hit areas are smaller than 44 px.

## M4: Workflows (in progress)

- [x] Spec: `docs/specs/2026-11-27-workflows.md`
- [x] `calin-workflow`: parsing, validation, templates and the engine, as a pure library
- [ ] Contract, then core and server: workflows from the home repo, a run inside a session,
      checks, gates, parallel branches, budget
- [ ] Web: workflow select, the Workflow view with the run overlaid
- [ ] ACP runner for one more agent
- [ ] Queue and parallel limit

## Later

M5 sign-in, server install, PWA, desktop · M6 integrations

## Decisions since the spec

- 2026-11-26: Solid 2.0 is the current stable line and is what the scaffold installs.
- 2026-11-26: Sign-in will be OpenID Connect (Auth0 or another provider) rather than passkeys.
- 2026-11-26: The home repo does not exist yet. It will hold the repo list and workflows, and a
  script that links its skills and instructions into the host's global Claude configuration.
