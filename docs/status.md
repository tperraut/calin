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

## M4: Workflows (done 2026-11-27, except a second agent runner)

- [x] Spec: `docs/specs/2026-11-27-workflows.md`
- [x] `calin-workflow`: parsing, validation, templates and the engine, as a pure library
- [x] Core and server: workflows from the home repo with lazy reload, a run inside a session
      (agent turns, checks, loops, gates, questions, parallel and fresh children, commit, ship),
      budget, the run projection
- [x] The queue: at most `max_parallel` root sessions preparing or running
- [x] Web: workflow select, the Workflow view with the run overlaid, gates
- [x] `make e2e` covers a workflow run with a check, a gate and a ship step (40 checks)
- [ ] ACP runner for one more agent. Waiting on which agent CLI to target; none besides
      `claude` is installed here.
- [x] A workflow run with the real Claude Code runner: the agent's first change left the repo's
      own check failing, the run looped, the agent got the failing output and fixed it, the
      check passed and the run ended in review.

Known gaps carried forward from M4:

- No editor for workflow files; they are written by hand in the home repo.
- A run interrupted by a server restart is closed as failed, not resumed.
- The loop limit travels to the web app inside the node's label ("Up to N times").
- `question_answered` carries no comment; a gate's comment is visible only in its node outcome.
- A file named `quick.yaml` is listed as invalid beside the built-in, so that id appears twice.
- The sessions list cannot show why a session is queued; the session page can.

## M5: Remote access (done 2026-11-28, except the desktop app and the real-world checks)

- [x] Spec: `docs/specs/2026-11-27-remote-access.md`
- [x] `calin-auth`: OpenID Connect sign-in, sessions, device tokens, as a library (206 tests)
- [x] Server: public mode, the `/auth` routes, a guard on every route and both sockets, device
      tokens, security headers and a Content-Security-Policy
- [x] Web: sign-in screen, sign out, signed-in browsers and device tokens in Settings
- [x] Installable web app: manifest, icons, a service worker that never touches `/api` or `/auth`
- [x] Web Push: `calin-push` (reproduces the RFC 8291 example byte for byte), the `/push`
      routes, triggers for waiting, review, failed and merged, notification settings
- [x] `calin server install`, `check` and `uninstall`
- [x] `make e2e` signs in through a stand-in provider against the real binary (14 checks)
- [ ] Desktop app. Needs these installed with root first (Tauri 2's own list for Fedora, checked
      2026-11-28 against https://v2.tauri.app/start/prerequisites/):
      `sudo dnf install webkit2gtk4.1-devel openssl-devel curl wget file libappindicator-gtk3-devel librsvg2-devel libxdo-devel`
      and `sudo dnf group install "c-development"`.
- [ ] Checks that need the owner: a real provider and domain; a real server (the generated Caddy
      file has never been validated by Caddy); a real phone for install and notifications.

Known gaps carried forward from M5:

- Push to a real push service over TLS is untested from the server: the library's tests run on
  loopback. A failed send is not retried.
- An open WebSocket learns that its session ended only by being closed; the web app then asks
  `/me` to find out why.
- Device tokens can delete push subscriptions, change preferences and send a test; they are
  refused only when creating a subscription or another token.
- The read-only system paths of the generated unit stop agents from installing system packages.
- `claude --version` in `calin server check` shows the CLI is installed, not that it is signed in.
- A reverse proxy that rewrites `Host` and adds no forwarding header makes outside requests look
  local to the tool endpoint's loopback check. Caddy's defaults are safe; the README says so.
- The manifest has one theme color, so the installed app's splash screen is light in both themes.

## M6: Usage, issues and automations (done 2026-11-28, except GitHub for real)

- [x] Spec: `docs/specs/2026-11-28-integrations.md`
- [x] Usage by day, session, repo, runner and model, from a rebuildable projection; the agent
      plan's limit windows, reported by the runner and shown with their reset times
- [x] GitHub issues through `gh`; a session started from an issue carries its text and its pull
      request closes it
- [x] Automations: a cron schedule, or new issues and pull requests; a baseline on enable, one
      active session at a time, switched off after three failures
- [x] Web: Usage page, issues in the new-session form, Automations pages, a summary line on the
      home page
- [x] `make e2e` covers usage, a session from an issue and an automation run (48 + 14 checks)
- [ ] Issues, issue and pull request triggers against real GitHub. Everything ran against a
      stand-in for `gh` and built-in sample issues.

Known gaps carried forward from M6:

- The usage range starts at the browser's local midnight while days are the server's, so the
  first day can be partial when the two time zones differ.
- A 400 from saving an automation names no field; the web app guesses which one from the wording.
- Cron: five fields only, no `@daily`, seconds or `L W # ?`.
- A repo set always adds its repos as worktrees.
- Cost is what the agent CLI reports; on a subscription it is an estimate, and the page says so.

## What is left

Everything in the original plan is on `main` except:

- the desktop app (M5), which needs system packages installed with root;
- a second agent runner through ACP (M4), which needs a choice of agent CLI;
- checks against the real world: pull requests and issues on GitHub, sign-in with a real
  provider and domain, an install on a real server, the app and notifications on a real phone;
- the polish items listed under each milestone above.

## Decisions since the spec

- 2026-11-26: Solid 2.0 is the current stable line and is what the scaffold installs.
- 2026-11-26: Sign-in will be OpenID Connect (Auth0 or another provider) rather than passkeys.
- 2026-11-26: The home repo does not exist yet. It will hold the repo list and workflows, and a
  script that links its skills and instructions into the host's global Claude configuration.
