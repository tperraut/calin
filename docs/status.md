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

## M2: Home repo and delivery (in progress, branch `m2`)

- [x] Contract: settings, repo catalog from the home repo, clone, repo sets, attach mid-session,
      the tool endpoint for agents
- [ ] Core and server: settings, `.calin/repos.yaml`, scan and clone, attach mid-session, path
      policy for attached repos, setup command, the tool endpoint, session system prompt
- [ ] Web: settings, catalog with clone and sets, attach from the session page, M1 polish
- [ ] Delivery: commit, push and one linked draft pull request per changed repo; PR and CI status
- [ ] End to end: mock in `make e2e`, then a real agent attaching a repo through the tool endpoint

## Later

M3 graph · M4 workflows · M5 sign-in, server install, PWA, desktop · M6 integrations

## Decisions since the spec

- 2026-11-26: Solid 2.0 is the current stable line and is what the scaffold installs.
- 2026-11-26: Sign-in will be OpenID Connect (Auth0 or another provider) rather than passkeys.
- 2026-11-26: The home repo does not exist yet. It will hold the repo list and workflows, and a
  script that links its skills and instructions into the host's global Claude configuration.
