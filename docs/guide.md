# Calin user guide

This guide is for someone who has the `calin` binary, or can build it, and wants to use it. It
describes what is implemented today. Where something has only been tested against a stand-in, it
says so. For how the code is laid out, read [AGENTS.md](../AGENTS.md); for progress, read
[status.md](status.md).

## 1. What Calin is

Calin is a self-hosted orchestrator for coding agents. One server process runs agent CLIs (today,
Claude Code) with your existing login, each session in git worktrees of its own. Everything an
agent does is recorded as an append-only event log and shown live in a web app that is served by
the same binary and works on a phone. A session can span several repositories, and agents can
start child sessions, message each other and ask you questions; Calin draws that as a graph.
Nothing merges on its own: every session ends in a review step where you decide.

There are two ways to run it:

- **On your machine.** `calin serve` listens on `127.0.0.1:4680`. Nobody signs in; only local
  processes can reach it.
- **On a server.** The same binary behind a reverse proxy with TLS, with sign-in through an
  OpenID Connect provider and an allow-list. See [section 9](#9-running-on-a-server).

There is no desktop app yet.

## 2. Quick start on your machine

### Build

You need Rust (the version in `rust-toolchain.toml`) and [Vite+](https://viteplus.dev), which
brings its own Node and Bun. From a checkout of Calin:

```sh
make install    # web dependencies
make build      # the web app, then target/release/calin with the app inside it
```

A binary built with plain `cargo build` has no web app in it and says so in its log.

### Look around in dry run

```sh
CALIN_DRY_RUN=1 target/release/calin serve
```

Open <http://127.0.0.1:4680>. In dry run every agent is a built-in simulated agent: no agent CLI,
no login, no network. It streams text, asks for a permission, shows a plan, and appends to a file
called `calin-mock-notes.md` in its working directory, so the Changes panel has something to show.

A few words in the prompt make the simulated agent do more:

| In the prompt | What the simulated agent does |
| --- | --- |
| `demo: orchestrate` | Dispatches two child sessions, tells you so, answers each child's report and then sums up. |
| `demo: orchestrate demo: note` | The same, and the second child also sends its parent a note before reporting. |
| `demo: ask` | Asks you a question with two suggested answers, then repeats your answer. |

Start a session with `demo: orchestrate` and open the **Graph** panel to see the tree grow.

The simulated agent really writes `calin-mock-notes.md`. Try dry run with no repo attached, or
with a throwaway clone, rather than with a repository you care about.

### A real session

Install [Claude Code](https://docs.anthropic.com/en/docs/claude-code) and sign in to it once in a
terminal. Then start Calin without `CALIN_DRY_RUN`:

```sh
target/release/calin serve
```

When `claude` is on your `PATH` (or `CALIN_CLAUDE_BIN` names it), sessions run on Claude Code;
otherwise they run on the simulated agent. `GET /api/v1/health` lists the runners the server
found, the default first.

In the web app, pick the repos to work in (or none), write a prompt and start. Without a home repo
you can register a clone by hand from the repo list ("Register a repo"). The next section sets up
a home repo, which is the intended way to work.

## 3. The home repo

The home repo is one git repository that every session starts from. It holds:

- `.calin/repos.yaml`: the repositories you work in, by name, and named sets of them;
- `.calin/workflows/*.yaml`: the workflows a session can run;
- your agent skills and instructions, which a script links into Claude Code's global
  configuration so they load in every session.

Calin itself keeps only runtime state. What is in the home repo is your configuration, versioned
like any other code.

### Create one

```sh
calin home init ~/code/home --scan ~/code
```

`calin home init [DIR]` works in `DIR` (default: the current directory). It creates the folder if
needed and runs `git init -b main` if it is not a repository yet; an existing repository's history
is left alone. It never overwrites a file: anything already there is reported as kept, so running
it twice is safe. It never commits. It writes:

| Path | What it is |
| --- | --- |
| `.calin/repos.yaml` | The repo list. With `--scan`, every git clone directly under the scanned folders, with the URL of its `origin` remote and commented-out guesses for `setup` and `checks`. Without `--scan`, a commented example. |
| `.calin/workflows/implement-and-check.yaml` | Implement the task, run the changed repos' checks, give the agent the failing output up to three times, end in review. |
| `.calin/workflows/implement-review-ship.yaml` | The same, then a review in a fresh context, your approval, and pull requests. |
| `README.md` | What the repository is for. Only written when there is no README. |
| `scripts/link-claude.sh` | Links `skills/`, `agents/`, `commands/` and `CLAUDE.md` into `${CLAUDE_CONFIG_DIR:-$HOME/.claude}`. |
| `skills/`, `agents/`, `commands/` | Empty folders with a `.gitkeep`, when they do not exist. |

`--scan` can be given several times. It looks one level deep, skips hidden folders and the home
repo itself, and names each repo after its folder; when two folders have the same name, the
second becomes `name-2` and a comment says why. A password or token in an `https` remote is
not copied into the file.

The command ends with the next steps: review `repos.yaml`, commit, then point Calin at the
repository.

### Point Calin at it

Commit first, then open **Settings** in the web app:

- **Home repo**: the absolute path of the repository.
- **Scan roots**: the folders that hold your clones, for example `~/code` spelled out in full.
- **Clone root**: where Calin clones a declared repo that has no clone yet. Empty means
  `<data dir>/repos`.
- **Sessions running at once**: how many sessions may prepare or run at the same time (default
  4; others wait as `queued`).

The same from a shell, while the server runs:

```sh
curl -X PUT http://127.0.0.1:4680/api/v1/settings -H 'Content-Type: application/json' \
  -d '{"home_repo":"/home/you/code/home","scan_roots":["/home/you/code"]}'
```

`PUT` replaces every setting, so include all the ones you want. Paths must be absolute; the home
repo must be the root of a git work tree and each scan root an existing folder, or the request is
refused. Saving the settings re-reads `repos.yaml` and rescans. Settings also has a button that
does just that, and it shows what the home repo declares, what was found locally, what is not
cloned, and each workflow with its problems if it has any.

### The repo list

```yaml
repos:
  api:
    url: git@github.com:you/api.git
    setup: cargo fetch
    checks: ["cargo test", "cargo clippy"]
  web: { url: git@github.com:you/web.git, checks: bun run check }
  notes: {}
sets:
  storefront: [api, web]
```

- The name is how sessions, sets and workflows refer to a repo, and it is the repo's folder inside
  a session. It must be unique and must not be `.`, `..` or contain `/` or `\`. An entry with an
  invalid or repeated name is skipped, and the home repo shows a problem in Settings.
- `url`: the repo's git URL.
- `setup`: a shell command run in each new worktree of the repo before the agent starts, for
  example to install dependencies.
- `checks`: shell commands that validate a change, run by a workflow's `check` step. One string is
  a list of one.
- `sets`: named groups. In the new-session form a set selects all its repos at once, each as a
  worktree. A name that is not a repo is ignored.
- Every field is optional. Unknown keys are ignored, so a file written for a newer Calin still
  reads. A file that does not parse never breaks anything: the repo list then holds only the
  repos you registered by hand, and Settings shows why.

### How repos are matched to clones

Each machine finds its own clones, so the same file works on a laptop and on a server:

- A repo with a `url` is matched to a clone under the scan roots (searched up to three levels
  deep, skipping hidden folders, `node_modules` and `target`) whose `origin` remote is the same
  repository. Spellings are compared loosely: `git@github.com:you/api.git`,
  `ssh://git@github.com/you/api` and `https://github.com/you/api` all match each other; the host
  ignores case, the path does not.
- A repo without a `url` is matched by name: a folder of that name directly under a scan root.
- A declared repo with no clone is listed as not cloned. If it has a `url`, **Clone** (in the repo
  list) clones it into the clone root; until then it cannot be attached.
- The home repo itself is always listed, first, marked "home".

## 4. Sessions

A session is one piece of work: a prompt, an agent, and a set of repositories.

### The repo set and the three modes

When you start a session you choose its repos. The first one is the **primary** repo: it is the
agent's working directory. The others are granted to the agent as additional directories. With no
repo, the agent works in an empty folder inside the session's own directory.

Each repo is attached in one of three modes:

| Mode | In the web app | What the agent works in |
| --- | --- | --- |
| `write` | Worktree | A new git worktree on the session's own branch, inside the session folder. The default for every repo except the home repo. |
| `read` | Read only | The clone itself; edits are refused. |
| `live` | Edit in place | The clone itself, edited on whatever branch it is on. The default for the home repo. |

With a home repo set, the new-session form starts with the home repo selected, first, in `live`
mode, so a session starts where you would start Claude by hand. Pick a different mode or primary
repo when you want one.

The session's branch name is `calin/<slug of the title>-<6 characters of the session id>`, the
same in every repo, so related branches are easy to find.

### Permission modes

| Mode | What waits for you |
| --- | --- |
| Ask | Every tool call. |
| Accept edits | Commands; file edits inside the session's repos are allowed. |
| Autonomous | Nothing. Every request is still recorded. |

A request that waits shows as a card on its tool call; the session is `waiting` until you allow
or deny it.

### Attaching a repo later

While a session is running, waiting or in review you can attach another repo from the session
page. The agent can do the same through Calin's `attach_repo` tool (`write` or `read`); it is told
how in its system prompt, and it gets the path to work in. Either way the attachment appears in
the transcript and the graph.

### Interrupt, stop, continue

- **Interrupt** ends the current turn; the session goes to review and you can send a new message.
- **Stop** ends the agent and marks the session cancelled. It also stops its child sessions.
- **Continue**: a message sent to a failed or cancelled session restarts the agent, which resumes
  its conversation. A message sent while the agent is working is delivered when its turn ends.

### The panels

Beside the transcript a session has these panels:

- **Changes**: what changed in each repo against the merge-base with the repo's base branch,
  grouped by repo, with a diff per file.
- **Files**: a file tree with one root per attached repo, and an editor. Saving writes into the
  worktree and is recorded in the log as your change. A `read` repo cannot be written.
- **Terminal**: shells in the session's repos, several at a time (up to eight per session). The
  variable `CALIN_SESSION_ID` tells a program which session it runs in.
- **Graph**: the session graph, the timeline, and the workflow run; see sections 6 and 7.

## 5. Reviewing and shipping

When a session reaches review, **Open pull requests** ships it. You can edit the title and choose
whether the pull requests are drafts. For every `write` repo whose branch differs from its base,
in the background:

1. uncommitted work is committed on the session branch;
2. the branch is pushed to `origin`, never forced, and never when it is the repo's default branch;
3. a pull request is opened against the repo's default branch with the `gh` CLI and your `gh`
   login, or the open one for that branch is reused;
4. once all are open, each description links its sibling pull requests.

A repo that fails is reported and the others carry on. The session is then `shipping`: Calin
watches each pull request's state, checks and review, and **Send back to the agent** hands a
failure back to the agent. When every pull request is merged or closed, the session is `done` and
read-only. Shipping again pushes new commits to the same pull requests.

What Calin never does: merge, force-push, delete a branch, or commit, push or ship a `live` or
`read` repo. Changes the agent makes to the home repo in `live` mode stay in that checkout for you
to commit.

In dry run nothing is pushed and GitHub is never called; pull requests get made-up URLs under
`https://example.invalid/`.

Pull requests have been tested only against a stand-in for `gh` and local remotes, never against
real GitHub. If `gh` is missing or not signed in, the health endpoint says why and shipping is
refused with that reason.

## 6. Orchestration

Agents talk to Calin through a tool endpoint it serves on loopback, scoped to their session tree:

- **Child sessions** (`dispatch`): an agent starts a child session with a title, a brief and a
  subset of its `write` repos (all of them by default). The child gets its own worktrees, on new
  branches starting at the parent's current commits, and runs with the parent's runner and
  permission mode. At most four children of one session run at a time, and a child cannot
  dispatch.
- **Reports** (`report`): when a child reaches review or fails, its report (or its last text) and
  its branch names go back to the agent that dispatched it.
- **Notes** (`send_message`): a note to the parent, to a child, or to you. A note to an agent
  becomes its next input; a note to you only appears in the transcript.
- **Questions** (`ask_user`): the session becomes `waiting` and shows the question, with
  suggested answers if the agent gave some. Your answer is the agent's next input.

Subagents that Claude Code runs inside its own process are shown as agents of their own too.
Child sessions appear in the session list, nested under their parent.

The **Graph** panel draws agents, repos and you as nodes, with edges for spawned, messaged and
changed. Selecting a node filters the transcript to that agent. A scrubber replays the session
from its log. **Timeline** shows one lane per agent, with turns and tool calls as spans and
messages as arrows between lanes. Both cover the whole tree of the session.

## 7. Workflows

Without a workflow, a session runs the built-in `quick` workflow: one agent turn on your prompt,
then review. A workflow file in the home repo's `.calin/workflows/` decides more: which prompts the
agent gets, which commands check its work, when to loop, when to ask you, and when to ship. Choose
one in the new-session form.

### The format by example

```yaml
version: 1
name: Implement, test, review          # shown in the app; the id is the file name
description: Optional one-liner.
budget_usd: 5                          # optional
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
  - { id: gate, type: gate, message: "Open pull requests?" }
  - { id: ship, type: ship }
  - { id: ok, type: end, status: success }
  - { id: gave-up, type: end, status: failed }
edges:
  - { from: start, to: implement }
  - { from: implement.done, to: tests }
  - { from: tests.pass, to: gate }
  - { from: tests.fail, to: retry }
  - { from: retry.repeat, to: fix }
  - { from: retry.exhausted, to: gave-up }
  - { from: fix.done, to: tests }
  - { from: gate.approve, to: ship }
  - { from: ship.done, to: ok }
```

The rules:

- The file name without `.yaml` (or `.yml`) is the id. It follows the node id rule below, and
  `quick` is taken.
- `version` is `1`; `name` is required. Unknown keys are errors, so a typo never changes
  behavior silently.
- Node ids match `[a-z][a-z0-9-]*`, are unique and at most 40 characters.
- An edge goes from `node` (its only port) or `node.port` to a node id. A port has at most one
  edge.
- One `start` node, at least one `end` node, every node reachable from `start`.
- Every cycle leaves a `loop` node through its `repeat` port, so every run ends.
- A port with no edge ends the run when taken: as failed for `failed`, `fail`, `exhausted` and
  `reject`, as success otherwise.
- Only `true` and `false` are booleans; `yes` and `no` are strings.

A file that does not validate is listed in Settings with every problem and its line, and cannot
be chosen. Calin reads the files again when they change.

### Node types

| Type | Fields | Ports | Outputs |
| --- | --- | --- | --- |
| `start` | | `next` | |
| `end` | `status`: `success` (default) or `failed` | | |
| `agent` | `prompt` (required), `fresh`, `verdicts`, `runner`, `model` | `done`, `failed`; or one per verdict, plus `failed` | `summary`, `verdict` |
| `check` | `command`, `repo`, `timeout_seconds` (default 600, at most 86,400) | `pass`, `fail` | `output`, `exit_code` |
| `loop` | `max` (required, 1 to 20) | `repeat`, `exhausted` | `iteration`, `max` |
| `gate` | `message` (required) | `approve`, `reject` | `comment` |
| `ask` | `question` (required), `options` | `answered` | `answer` |
| `parallel` | `branches`: 2 to 4 of `{ id, prompt, repos? }` | `done`, `failed` | `summary` |
| `commit` | `message` (required) | `done`, `nothing`, `failed` | |
| `ship` | `title`, `draft` (default `true`) | `done`, `failed` | `urls` |

- `agent` gives the lead agent a turn. With `fresh: true` the prompt goes to a new child session
  that starts from the session's current commits (so commit first if the work is not committed)
  and reports back; `runner` and `model` apply only then. With `verdicts`, the agent is told to
  end with `VERDICT: <word>` and the node leaves through that port, or through `failed` if there
  is none.
- `check` with `command` runs it in `repo`'s worktree (the primary repo by default). Without
  `command`, it runs the `checks` from `repos.yaml` of each changed `write` repo, stopping at the
  first failure; with nothing to check it passes. `output` is the last 16 KiB of the last command.
- `gate` and `ask` make the session wait for you.
- `parallel` starts one child session per branch and waits for all of them.
- `commit` commits in every `write` repo; `ship` opens or updates pull requests, as in section 5.

### Templates

`prompt`, `command`, `message`, `question`, `title` and each branch's `prompt` may contain
`{{task}}` (the session's prompt), `{{session.title}}`, `{{session.branch}}` and
`{{nodes.<id>.<output>}}` (from that node's newest run; empty if it has not run). Anything else
between `{{ }}` is an error when the file is loaded.

### Budgets and runs

When the session tree's cost passes `budget_usd`, the current node is interrupted and the run ends
as failed. Whatever happens, a run ends with the session in review (or shipping, if a `ship` node
opened pull requests); you decide what to do with the work. A message you send during a run
reaches the agent at its next turn. After the run, the session is an ordinary one.

The **Workflow** view in the Graph panel shows the graph with the active node, the edges taken,
loop counters and each node's outcome. A gate appears as a question with Approve and Reject.

There is no editor for workflow files yet. A run interrupted by a server restart is closed as
failed, not resumed.

## 8. Issues, automations and usage

### Issues

When `gh` is available, the new-session form has an **Issues** section for a repo: search, pick
one, and add instructions of your own. The agent gets the issue's title, body and URL in its
prompt; the issue's repo becomes the primary repo; and the pull request for that repo says
`Closes #<number>`. Other trackers are not supported.

### Automations

The **Automations** page starts sessions by themselves:

- on a **schedule**, a five-field cron expression in the server's local time (no `@daily`, no
  seconds); missed times are not made up;
- on a **new issue** in a repo (optionally with a label) or a **new pull request**, found by
  asking GitHub through `gh` every five minutes. When you enable one, what already exists is
  marked as seen, so enabling never starts a backlog.

Each automation has a prompt template (`{{date}}`; for issues and pull requests also
`{{number}}`, `{{title}}`, `{{body}}`, `{{url}}`, `{{author}}`), a workflow, repos and a
permission mode, which must be Accept edits or Autonomous since nobody is there to answer. At
most one session of an automation is active at a time; a fire that finds one is skipped. After
three failed runs in a row an automation switches itself off and says why. **Run now** starts one
at once. Automations are kept in `automations.json` in the data directory. In dry run, schedules
fire and GitHub triggers do not poll.

Issue and pull request triggers have been tested only against a stand-in for `gh`.

### Usage and plan limits

The **Usage** page shows cost and tokens over 7, 30 or 90 days, by day, session, repo and model,
and the agent plan's limit windows with their reset times ("5-hour window: 62% used"). The cost
is what the agent CLI reports; on a subscription it is an estimate, not a bill. Limits are known
only after an agent has run since the server started. The home page shows a summary line: what
needs you, what is running, and the limit closest to full.

## 9. Running on a server

### Public mode

Setting `CALIN_PUBLIC_URL` switches Calin to public mode. The server still listens only on
loopback; a reverse proxy on the same machine terminates TLS and forwards to it. Every request
must come from a signed-in browser or carry a device token. The server refuses to start unless
all of these are valid:

| Variable | Meaning |
| --- | --- |
| `CALIN_PUBLIC_URL` | The exact origin users type, `https://calin.example.com`. |
| `CALIN_OIDC_ISSUER` | Your OpenID Connect provider's issuer URL. Register `<CALIN_PUBLIC_URL>/auth/callback` with it as the redirect URI. |
| `CALIN_OIDC_CLIENT_ID` | The client id registered with the provider. |
| `CALIN_OIDC_CLIENT_SECRET` | Optional. Without it, Calin relies on PKCE alone. |
| `CALIN_ALLOWED_USERS` | Who may sign in, comma-separated: email addresses, or `sub:<subject>`. Must not be empty. |

An email matches only when the provider says it is verified. Signed-in browsers stay signed in
for 30 days from their last use. Removing someone from `CALIN_ALLOWED_USERS` ends their sessions
and device tokens at the next start.

### Installing

```sh
calin server install --domain calin.example.com \
  --issuer https://your-tenant.eu.auth0.com/ --client-id <id> --allow you@example.com
```

This writes a systemd unit, an environment file (mode 0600) and a Caddy site to a staging folder
(`calin-install` by default) and prints the commands to run as root. It changes nothing on the
machine itself. Options: `--client-secret-file` (read from a file so the secret stays out of your
shell history), `--user` (default `calin`), `--data-dir` (default `/var/lib/calin`), `--binary`
(default `/usr/local/bin/calin`), `--port` (default 4680), `--repos-dir` (folders the service may
write to, default `/srv/calin`; repeatable), `--staging`.

`calin server check --domain ... --issuer ...` verifies the result: the service, the public URL,
the provider, and the CLIs. It shows that `claude` is installed, not that it is signed in.
`calin server uninstall` prints how to remove an installation; it deletes nothing.

After the first sign-in, sign in to `claude` and `gh` as the service user through Calin's own
Terminal panel.

### Device tokens

For scripts, create a token in **Settings → Device tokens**. It is shown once, as `calin_...`.
Send it as `Authorization: Bearer <token>`:

```sh
curl -H "Authorization: Bearer $CALIN_TOKEN" https://calin.example.com/api/v1/sessions
```

Tokens and signed-in browsers are listed and revoked in Settings. In local mode there are none.

### The phone app and notifications

Served from its domain, the web app can be added to a phone's home screen. **Settings →
Notifications → Notify this device** subscribes the browser to push notifications: when a session
waits for you, reaches review, fails, or has all its pull requests merged. Each kind can be
switched off. Push needs public mode.

None of this has been tried on a real phone, with a real provider and domain, or on a real server
yet; the generated Caddy file has never been checked by Caddy.

### Warnings

- **Whoever signs in gets a shell** on the machine, through the Terminal panel and through the
  agents. Keep the allow-list short.
- **Agents are not sandboxed.** They run as the service user with a full shell. Run the service as
  a dedicated user without sudo, which is what the install does.
- **The reverse proxy** must keep the `Host` header and set `X-Forwarded-For`. A proxy that does
  neither makes outside requests look local, and the agents' tool endpoint answers only to local
  requests. Caddy's defaults are safe.
- Providers behind a private certificate authority, and outbound proxies, are not supported.

## 10. Reference

### `calin` commands

| Command | What it does |
| --- | --- |
| `calin` or `calin serve [--bind ADDR:PORT] [--home DIR]` | Run the server. `--bind` defaults to `127.0.0.1:4680` and must be a loopback address. |
| `calin home init [DIR] [--scan DIR]...` | Scaffold a home repo (section 3). |
| `calin server install ...` | Write the service files and print the root commands (section 9). |
| `calin server check ...` | Verify an installation. |
| `calin server uninstall` | Print how to remove an installation. |

`calin --help` and `calin <command> --help` list every option.

### Environment variables

| Variable | Effect |
| --- | --- |
| `CALIN_HOME` | Data directory. Default `~/.calin`. `--home` overrides it. |
| `CALIN_DRY_RUN=1` | Run every agent on the simulated agent. |
| `CALIN_MOCK_DELAY_MS` | Delay between the simulated agent's chunks. Default 40. |
| `CALIN_CLAUDE_BIN` | Path of the Claude Code CLI. Default `claude` on `PATH`. |
| `CALIN_GH_BIN` | Path of the GitHub CLI. Default `gh` on `PATH`. Never run in dry run. |
| `CALIN_LOG` | Log filter, for example `debug` or `calin_core=trace`. Default `info`. Logs go to stderr. |
| `CALIN_PUBLIC_URL`, `CALIN_OIDC_ISSUER`, `CALIN_OIDC_CLIENT_ID`, `CALIN_OIDC_CLIENT_SECRET`, `CALIN_ALLOWED_USERS` | Public mode and sign-in (section 9). |
| `CALIN_PUSH_CONTACT` | Public mode, optional: a `mailto:` address push services can reach the operator at. Default: the public URL. |
| `CALIN_UNSAFE_DEV_ALLOW_HTTP_LOOPBACK=1` | Development and browser tests only: allows plain `http` on loopback for the public URL, the provider and push. Never on a real server. |
| `CLAUDE_CODE_ENABLE_TODO_TOOLS=1` | Claude Code's own, passed through: without it, current models get no task tools and a session shows no plan. |

Inside the Terminal panel, `CALIN_SESSION_ID` is set to the session's id.

### The data directory

Everything here is machine-local and can be rebuilt or recreated, except your settings, the repos
you registered by hand, automations and sign-in state:

```
~/.calin/
  calin.db                   SQLite index, rebuilt from the logs if missing
  calin.lock                 held by the running server
  settings.json              home repo, scan roots, clone root, sessions at once
  repos.json                 repos registered by hand, and the ids of declared repos
  automations.json           automations, their runs and what their triggers have seen
  auth.json                  public mode: signed-in browsers and device tokens (hashed)
  push-vapid-key.json        public mode: the push key pair
  push-subscriptions.json    public mode: devices subscribed to notifications
  repos/                     default clone root
  sessions/<id>/
    events.ndjson            the session's event log, append-only
    session.json             what the session was created with
    work/                    the session's worktrees, one folder per repo name
```

### The API

The web app uses a REST API under `/api/v1` and one WebSocket; the route table is in
[`crates/calin-proto/src/api.rs`](../crates/calin-proto/src/api.rs). It is not versioned for
outside use yet.

## 11. Limits and known gaps

What is not there:

- No desktop app.
- One agent runner: Claude Code. A second one, through the Agent Client Protocol, waits on a
  choice of agent.
- No editor for workflow files; GitHub is the only issue tracker.

What has not been tried for real:

- Pull requests, issues and the issue and pull request triggers ran only against a stand-in for
  `gh`.
- Sign-in with a real provider and domain, an install on a real server, the installed app and
  notifications on a real phone. Push to a real push service is untested from the server, and a
  failed push is not retried.

Rough edges you may meet:

- Real Claude sessions show no plan unless you set `CLAUDE_CODE_ENABLE_TODO_TOOLS=1`.
- Stopping a session leaves its running tool calls shown as open; a tool call cut short by an
  interrupt shows as failed.
- A child session that is stopped, or failed by a server restart, sends no report to its parent.
- A workflow run or a session cut short by a server restart is marked failed; continue it with a
  message.
- Idle worktrees are never cleaned up; disk use grows with sessions.
- `git push` and commit hooks run without a timeout. The push remote is always `origin`; forks are
  not handled.
- An agent that edits files through its shell instead of its edit tool still shows in Changes,
  but the count on the Changes tab may lag.
- Some errors from saving settings or an automation do not name the field.
- The usage range starts at your browser's midnight while days are the server's.
- Log lines survive a crash of the process, not a power cut.
- Timeline marks and some arrow targets are smaller than a comfortable touch target.
