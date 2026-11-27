# Calin: design spec

Status: accepted, in implementation · 2026-11-26 · progress in [status.md](../status.md)

Calin is a self-hosted orchestrator for coding agents, inspired by
[cezar](https://github.com/open-mercato/cezar). It runs as one small server on a VPS, is used from
a phone or any laptop, and has a desktop app that either runs it locally or connects to the server.

It differs from cezar in three ways, which are the reason to build it:

1. **A session spans several repositories.** Sessions start from a home repo and attach any number
   of other repos. Changes, files, terminal and pull requests are shown across all of them at once.
2. **Orchestration is drawn as a graph.** Who spawned whom, who said what to whom and which agent
   touched which repo are recorded as events and rendered live.
3. **The workbench is built in.** Editor, diff, terminal and search are part of the app, so a
   session can be reviewed and corrected from a phone without another tool.

Implementation follows the milestones in section 11.

## Contents

1. [Goals and non-goals](#1-goals-and-non-goals)
2. [Architecture](#2-architecture)
3. [Multi-repo sessions](#3-multi-repo-sessions)
4. [Agents and events](#4-agents-and-events)
5. [Graph views](#5-graph-views)
6. [Built-in workbench](#6-built-in-workbench)
7. [Workflows](#7-workflows)
8. [Remote access and security](#8-remote-access-and-security)
9. [Feature map against cezar](#9-feature-map-against-cezar)
10. [Repository layout and development method](#10-repository-layout-and-development-method)
11. [Milestones](#11-milestones)
12. [Risks and things to verify](#12-risks-and-things-to-verify)
13. [Decisions and open questions](#13-decisions-and-open-questions)

## 1. Goals and non-goals

**Goals**

- One orchestrator reachable from a phone and several laptops, with agents that keep working when
  every client is closed.
- Sessions that start in the home repo (skills, rules, agents, docs) and work across sibling repos,
  with one view of everything they changed.
- A live, replayable picture of how agents are orchestrated and how they communicate.
- Editor, diff, terminal and search built in and usable on a phone.
- A server small enough for a cheap VPS, and a web app that stays fast on a phone.
- Agents run through their own CLIs with the existing login. No API key is required.

**Non-goals for the first version**

- Multiple users, teams or permissions. Calin is single-user.
- Native phone apps and a relay service. A VPS makes them unnecessary; the phone uses a PWA.
- One hand-written integration per agent CLI. Calin has two runners: Claude Code and ACP.
- Replacing an IDE. The editor is for reading, reviewing and correcting, without language servers.
- Telemetry of any kind.

**Principles**

- *The home repo owns configuration.* Skills, rules, workflows and the repo list live there under
  git. Calin stores runtime state only, and that state can be deleted and rebuilt.
- *The event log is the truth.* Every view is a replay of a session's log.
- *Nothing merges on its own.* Every session ends at a human review step.
- *Works with zero configuration.* Every setting has a working default.

## 2. Architecture

![Calin system architecture](diagrams/calin-architecture.svg)

Interactive version: [calin-architecture.html](diagrams/calin-architecture.html). Its nodes link to
the other diagrams.

One Rust process contains everything. It runs on the VPS, or inside the desktop app.

| Part | Responsibility |
| --- | --- |
| API gateway | REST under `/api/v1`, one WebSocket per client for subscriptions and event replay, and one socket per open terminal |
| Orchestrator | Sessions and their repo sets, the queue, the lifecycle, the workflow engine |
| Agent runners | Start and steer agent CLIs; turn each CLI's output into Calin events |
| Calin MCP tools | The channel agents use to talk back: dispatch, message, attach a repo, ask the user |
| Event bus | Appends each event to the log, updates the index, pushes it to clients, in that order |
| Repo services | Worktrees, diffs, the file API and watcher, the terminal's PTY, pull requests through `gh` |

### Stack

| Layer | Choice | Reason |
| --- | --- | --- |
| Server | Rust, axum, tokio | One static binary and low memory. The same crate is linked into the desktop app. |
| Index | SQLite in WAL mode | Cross-repo and graph queries without a database server |
| Event store | One append-only NDJSON file per session | Cheap to write, trivially replayable, readable with standard tools |
| Wire types | Defined in Rust, generated to TypeScript | The server and the web app cannot disagree; drift fails the build |
| Web app | SolidJS 2, Vite+ (`vp`), Bun as package manager | Small bundles and fine-grained updates suit long live transcripts on a phone |
| Styling | Tailwind v4 and Kobalte | Accessible unstyled components that exist for Solid |
| Editor and diff | CodeMirror 6 | Works with touch input and loads on demand; Monaco is not usable on a phone |
| Terminal | xterm.js over a server PTY | Standard, and already proven in cezar |
| Graph | ELK layout in a web worker, custom Solid renderer | See [Graph views](#5-graph-views) |
| Desktop | Tauri 2 | Small shell; links the Rust core directly for local mode |
| Phone | PWA with web push | Installable, and no app store or relay |

### Two ways to run

- **Server.** `calin serve` on the VPS, behind HTTPS. The web app is embedded in the binary.
- **Desktop.** The Tauri app has connection profiles. *Local* starts the core inside the app
  process on loopback. *Remote* is a window onto a Calin server. Both can be configured side by side.

### State on disk

```
~/.calin/                      machine-local, safe to delete
  repos.json                   the repo registry of this machine
  calin.db                     SQLite index, rebuilt from the logs if missing
  sessions/<id>/
    events.ndjson              the session's event log
    session.json               what the session was created with
    work/api/  work/web/  ...  one worktree per attached repo

<home repo>/.calin/            versioned with your other configuration
  repos.yaml                   logical repo names, git URLs, checks, named sets
  workflows/*.yaml
```

Resource targets, to be measured from M0 onward: under 50 MB of memory for an idle server, and an
initial web bundle under 250 KB compressed with the editor, terminal and graph loaded on demand.

## 3. Multi-repo sessions

This is the part cezar cannot do. In cezar a task belongs to one project and its state lives inside
that repo; tags only group tasks on a list. In Calin a session never belongs to a repo.

![Calin data model](diagrams/calin-data-model.svg)

Interactive version: [calin-data-model.html](diagrams/calin-data-model.html).

### Repo registry

The home repo lists the repos you work with, by logical name:

```yaml
# <home repo>/.calin/repos.yaml
repos:
  api:           { url: git@github.com:you/api.git,  checks: ["cargo test"], setup: "cargo fetch" }
  web:           { url: git@github.com:you/web.git,  checks: ["bun run check"], setup: "bun install" }
  design-system: { url: git@github.com:you/ds.git }
sets:
  storefront: [api, web, design-system]
```

Each machine resolves a name to a local clone by matching the remote URL under its scan roots
(for example `~/code`). The same file therefore works on the VPS and on each laptop, where paths
differ. A repo with no local clone is cloned on first use. Without a `repos.yaml`, Calin lists every
clone it finds under the scan roots.

### Repo set

- A session has a repo set: one `session_repo` row per attached repo. The home repo is one of them.
- Each attached repo gets a worktree inside the session folder. The home repo is the exception:
  it is attached as the live checkout unless the session asks for a worktree of it. The branch name is stored once on
  the session and used in every repo, so related branches are easy to find.
- The agent's working directory is the home repo, so a session starts where you would start
  Claude by hand. The worktrees are granted as additional directories.
- A repo can be attached in three ways: chosen at start (one by one or as a named set), added by you
  mid-session, or requested by the agent through the `attach_repo` tool when it discovers it needs it.
- Attach modes: `write` (a worktree, the default), `read` (the main checkout, edits denied) and
  `live` (edit the main checkout in place).
- After creating a worktree Calin runs the repo's `setup` command. Idle worktrees are reclaimed
  after a period; the branch is kept and the worktree is restored on Continue.

### Unified changes

- One Changes view per session, grouped by repo, with a total across repos.
- Each repo's diff is anchored at its merge-base with the freshest base ref. cezar's history shows
  that a stale local `main` otherwise inflates diffs by orders of magnitude.
- Each change records the agent and the event that made it, so a hunk can be traced to the tool call
  that wrote it.
- A review note on a line is sent back to the agent as a message, with the file and repo attached.
- Sessions overlap view: which running sessions touch the same repo or file, across the workspace.

### Delivery

The change set of a session is its attached repos and their pull requests. *Open PRs* creates one
draft PR per changed repo through `gh`, and each PR body links its siblings. The session shows a
combined status for PRs, CI and reviews. A red CI run can send the session back to the agent.

### Session lifecycle

![Calin session lifecycle](diagrams/calin-session-lifecycle.svg)

Interactive version: [calin-session-lifecycle.html](diagrams/calin-session-lifecycle.html).

## 4. Agents and events

![A multi-repo session, from start to review](diagrams/calin-session-sequence.svg)

Interactive version: [calin-session-sequence.html](diagrams/calin-session-sequence.html).

### Runners

One interface, three implementations:

| Runner | Transport | Covers |
| --- | --- | --- |
| Claude Code | `claude -p` with `--input-format stream-json --output-format stream-json` | Claude Code, with resume, partial messages, subagents and cost |
| ACP | Agent Client Protocol, JSON-RPC over stdio | Any agent that speaks ACP natively or through an adapter |
| Mock | Replays recorded fixtures | Tests, end-to-end runs and demos with no login |

The Claude runner passes `--add-dir` for each attached repo, `--mcp-config` for Calin's tools,
`--permission-prompt-tool` so that permission requests appear in the UI, and `--resume` to continue.
All of these flags exist in the installed CLI (2.1.332).

Each runner is tested against recorded transcripts of the real CLI. This is cezar's golden-fixture
practice and it is what keeps a runner correct when the CLI changes.

### Event model

Every runner emits the same normalized events, modeled on ACP's vocabulary:

- Items with a stable id and a lifecycle: text, reasoning, tool call (`started`, `delta`, `completed`).
- Plan updates, usage and cost, status changes.
- Orchestration events: `agent.spawned`, `message.sent`, `repo.attached`, `file.changed`,
  `permission.requested`, `question.asked`, `check.finished`.

Each event has a per-session sequence number, the agent that emitted it and a **parent event**.
The parent is the cause: the tool call that produced a spawn, the message that woke an agent.
Clients subscribe with their last sequence number and receive what they missed, then live events.

### Calin MCP tools

Agents talk to the orchestrator through an MCP endpoint that Calin serves and scopes with a
per-session token:

| Tool | Effect |
| --- | --- |
| `attach_repo` | Add a repo to the session and receive its worktree path |
| `dispatch` | Start a child session with a brief, a repo subset and a budget |
| `send_message` | Message another agent in the session tree, or the user |
| `report` | Return a result or a verdict to the parent |
| `ask_user` | Ask a question and park until answered |
| `request_permission` | Target of the permission prompt; resolved in the UI or by policy |

A thin `calin` CLI offers the same operations for agents without MCP support.

### Two kinds of delegation

- **Child session.** A separate agent process with its own worktrees, forked from the parent's
  branch, and a budget carved out of the parent's. It reports back into the parent's conversation.
  This is cezar's dispatch model and it is the safe way to run work in parallel.
- **In-session agent.** An agent's own subagents, or a reviewer, sharing the session's worktrees.
  The runner reports them from the CLI's stream, so they appear in the graph without extra work.

### Permissions

Three modes per session: *ask* (requests appear as cards in the UI and as push notifications),
*accept edits* (file edits are allowed, commands ask) and *autonomous* (never asks). cezar has no
channel for answering a permission request from its UI; Calin has one from the start.

## 5. Graph views

The graph is a first-class view, not a decoration, because the data model records causality.

| View | Shows |
| --- | --- |
| Session graph | Agents, repos and you as nodes. Edges for spawned, messaged and changed. Live, with counts on edges. |
| Timeline | One lane per agent. Turns and tool calls as spans, messages as arrows between lanes, waiting as gaps. |
| Workflow | The workflow's nodes and edges with the current run overlaid: active node, taken edges, loop counts. |
| Workspace map | Sessions against repos across the whole workspace, with overlaps highlighted. |

Common behavior:

- Selecting a node filters the transcript to that agent. Selecting an edge lists its messages.
- A scrubber replays the session from its log, so a finished run can be watched again.
- Layout runs in a web worker with ELK. Positions of existing nodes are kept when new ones arrive,
  so the picture does not jump while agents work.

The renderer is written for Calin in Solid on SVG, with a canvas fallback for large graphs. The
Solid port of xyflow was pre-release when this was written, so it is not a dependency. Check its
status at M3, and again for the workflow *editor* in M4, where drag-and-connect matters more than
animation.

## 6. Built-in workbench

| Tool | First version | Later |
| --- | --- | --- |
| Files | One tree with a root per attached repo; quick open across all roots | |
| Editor | CodeMirror 6, tabs, syntax highlighting, save into the worktree, change markers | Language servers proxied from the server |
| Diff | Side by side on wide screens, inline on a phone; per repo and across repos | Compare variants of the same task |
| Search | ripgrep on the server, across the session's repos | |
| Terminal | A shell in any attached worktree; several tabs | Shared terminal that the agent can read |
| Review | Comment on a line and send it to the agent | |

Saving a file is an ordinary event in the log, attributed to you, so manual corrections appear in
the timeline next to the agent's changes.

## 7. Workflows

Workflows are YAML graphs in the home repo, in the spirit of cezar's `version: 2` format: nodes,
edges and output ports, with every cycle passing through a loop node so every run is finite.

Calin adds:

- `repo:` on a check node, so a command runs in one repo's worktree. A check without `repo:` runs
  each changed repo's default checks from `repos.yaml`.
- A `parallel` node that starts several child sessions and waits for all of them.
- Node types kept from cezar: `agent`, `check`, `loop`, `gate.human`, `ask-user`, `dispatch`,
  `git.commit`, `github.draft-pr`.

The built-in workflow needs no file: run the agent on the prompt, then stop at review.

## 8. Remote access and security

A Calin server gives whoever signs in a shell and agents with a shell. Access control is therefore
part of the first network-reachable milestone, not a later addition.

- Sign-in through OpenID Connect (Auth0, or any provider that speaks it), then a same-site session
  cookie. Only identities on an allow-list are admitted; an empty allow-list admits nobody.
- Device tokens for the desktop app and the CLI, issued after a browser sign-in.
- TLS from Caddy on a public domain.
- Every state-changing request is checked for origin. There is no cross-origin API.
- On loopback (desktop local mode) there is no sign-in; requests are checked against DNS rebinding.
- Each agent's MCP token is valid for its own session tree only.
- Agents are not sandboxed in the first version. They run as the `calin` user with a full shell in
  the session's worktrees. Run the server as a dedicated user without sudo.
- Later: one systemd scope per session for memory limits and for reaping leftover processes.

`calin server install` writes the systemd unit and the proxy configuration. Agent logins (`claude`,
`gh`) are done once on the server, through the built-in terminal.

## 9. Feature map against cezar

| cezar feature | Calin | When |
| --- | --- | --- |
| Uses your own agent CLI login | Kept | M1 |
| Live run: text, tool calls, tokens, cost | Kept | M1 |
| Worktree per task | Changed: a worktree per repo per session | M1, M2 |
| Multiple projects, switch between them | Replaced by multi-repo sessions | M2 |
| Skills as Markdown | Kept, read from the home repo | M1 |
| Task dispatch to child tasks | Kept, and drawn in the graph | M3 |
| Workflows as YAML graphs, mixed agents per step | Kept, with per-repo checks | M4 |
| Queue, parallel limit, budgets | Kept | M4 |
| Autonomous mode | Kept | M4 |
| Server install on a VPS | Kept | M5 |
| Responsive phone UI | Kept, as a PWA with push | M5 |
| Desktop app | Kept; embeds the core instead of managing a Node install | M5 |
| Run on a GitHub issue, draft PRs | Kept; one PR per changed repo | M2, M6 |
| Dashboard, usage and cost | Kept | M6 |
| Automations: schedules and triggers | Kept | M6 |
| Run a task ×2 or ×3 and compare | Deferred | after M6 |
| Jira and Linear | Deferred until the tracker is chosen | after M6 |
| No database, plain files | Changed: logs as files, plus a SQLite index | M0 |
| Eleven agent backends | Changed: Claude Code and ACP | M1, M4 |
| Managers, design mode, HTML artifacts, webhooks, agent config editor | Not planned yet | |
| Native phone apps and relay, telemetry | Dropped | |

cezar is about 131,000 lines of server code and 136,000 lines of web code. Calin does not aim to
match that; the map above is the cut.

## 10. Repository layout and development method

```
calin/
  crates/
    calin-proto/      wire types and events; generates TypeScript
    calin-core/       sessions, store, event bus, queue
    calin-git/        worktrees, diff, remotes, pull requests
    calin-agents/     runner interface; claude, acp and mock runners
    calin-workflow/   YAML graph engine
    calin-server/     axum API, WebSocket, MCP endpoint, files, PTY
    calin-cli/        the calin binary
  apps/
    web/              SolidJS app
    desktop/          Tauri shell
  docs/specs/         dated specs, this file first
  fixtures/           recorded agent transcripts
  AGENTS.md
```

What keeps agent-written code coherent is the harness around it, more than any skill set:

- **`AGENTS.md` as a router.** For each kind of task: what to read first and the rules that apply.
- **One validation command.** Format, lint, type-check, tests and a type-drift check, fast enough to
  run before every commit.
- **The mock runner.** The whole app runs offline, so end-to-end tests are cheap and deterministic.
- **Golden fixtures.** A runner change must replay every recorded transcript unchanged.
- **Dated specs.** A feature starts as a short spec in `docs/specs/`; code cites it.
- **A few project skills.** Solid reactivity rules (agents tend to write React idioms), server
  conventions, and how to verify a UI change in a real browser.

Skill sets:

- **Superpowers** from the first commit. Its design, plan, test-first and review skills are
  stack-agnostic and suit a project that is still being shaped interactively.
- **Open Mercato skills** from M2 or M3, selectively: pipeline setup, spec writing, autonomous PR
  creation, PR review and QA. They assume a GitHub repo with a validation command and labels, and
  they overlap Superpowers, so only one of the two should drive a given session.
- From M1 Calin is developed inside Calin, which is the earliest honest test of it.

## 11. Milestones

Each milestone ends with something usable. No dates are attached.

| | Milestone | Done when |
| --- | --- | --- |
| M0 | Skeleton | The validation command passes; the server serves the web app; a mock session streams events to the browser and survives a reload |
| M1 | One session, real agent, real repos | A real Claude Code session runs in a worktree per attached repo with a live transcript, permissions, changes grouped by repo, diff, editor and terminal; it can be interrupted, stopped and continued |
| M2 | Home repo and delivery | The repo list comes from the home repo, a repo can be attached mid-session by you or by the agent, and a session opens linked draft PRs |
| M3 | Graph | Agents dispatch children and message each other through MCP; the session graph and timeline are live and replayable |
| M4 | Workflows | YAML graphs run with per-repo checks, gates, queue and budgets; the ACP runner works with one more agent |
| M5 | Everywhere | OpenID Connect sign-in, `calin server install`, the PWA with push, and the desktop app in local and remote mode |
| M6 | Integrations | GitHub issues, PR and CI watch, schedules and triggers, usage and cost |

Section 12 lists the technical unknowns and the milestone that settles each.

## 12. Risks and things to verify

| Risk | How it is settled |
| --- | --- |
| Vite+ with the Solid plugin and Bun | Settled in M0: the scaffold builds, type-checks and lints with Vite+ 1.8, Solid 2.0 and Bun 1.4. |
| The Claude stream-json protocol is not a versioned public contract | Golden fixtures per CLI version. The mapper ignores what it does not know, and a CLI version other than the recorded one logs a warning; surfacing that warning in the UI is still to do. |
| Granting a new directory to a running Claude session | Settled on CLI 2.1.332: the CLI asks permission for a path outside its directories, and once Calin allows it the read, edit or write goes through. The request also carries an `addDirectories` suggestion for the rest of the session. No restart is needed. |
| A custom graph renderer is real work | Spike at the start of M3: 200 nodes laid out and animated on a phone. Fallback: adopt the xyflow port if it is stable by then. |
| Disk use: a worktree and dependencies per repo per session | `setup` commands, idle reclamation, and a disk view from M2 |
| A public server that hands out shells | The server refuses any non-loopback bind until sign-in ships in M5 |
| Scope: cezar is a large product | The feature map is the cut. Every milestone is usable on its own. |
| Subscription logins on a headless server | Verify that `claude` login and token refresh work unattended on the VPS in M1 |

## 13. Decisions and open questions

Decided on 2026-11-26:

- **The home repo** does not exist yet. It will be the repo every session is launched from. It holds
  `.calin/repos.yaml` and the workflows, plus a script that links its skills and instructions into
  the host's global Claude configuration. Skills therefore load in every session regardless of the
  working directory, and docs refer to other repos by their name in `repos.yaml`, not by path.
- **The home repo in a session** is attached in `live` mode by default: the agent works in the
  home checkout itself, as when launching Claude there by hand. A session can opt into a private
  worktree of it instead. Every other repo defaults to `write` (a worktree).
- **Reach**: a public domain with OpenID Connect sign-in (Auth0 or another provider).

Still open:

1. **Other agents.** Which CLIs besides Claude Code do you use? This sets when the ACP runner is due.
2. **Tickets.** Which tracker: GitHub Issues, Linear, another?
3. **Parallel agents.** Is a child session with its own worktrees the right default, or do you
   mostly want several agents working in one shared set of worktrees?

## References

- cezar: <https://github.com/open-mercato/cezar> (`AGENTS.md`, `AGENT_PROTOCOL.md`, `docs/reference.md`)
- Open Mercato skills: <https://github.com/open-mercato/skills>
- Superpowers: <https://github.com/obra/superpowers>
- Vite+: <https://viteplus.dev/>, <https://voidzero.dev/posts/announcing-vite-plus-beta>
- Solid port of xyflow: <https://jsr.io/@dschz/solid-flow>
- Agent Client Protocol, Rust SDK: <https://docs.rs/crate/agent-client-protocol/0.14.0/source/README.md>
