# Usage, issues and automations

Status: accepted · 2026-11-28 · milestone M6

Three additions that make Calin useful day to day: knowing what it costs and how close the agent
plan's limits are, starting work from an issue, and starting work without being there.

## 1. Usage and limits

### What is recorded

Agents already report running totals (`usage` events: cost, input and output tokens). M6 adds the
plan limits that the agent CLI reports while it runs.

- A runner may report **limit windows**: a name (`five_hour`, `seven_day`), how much of the window
  is used (0 to 1), when it resets, and whether requests are currently refused. The Claude runner
  takes them from the CLI's `rate_limit_event`. They are recorded in the session log as a
  `limits` event attributed to the agent, at most once a minute per agent unless a value moved by
  more than one point or the refused state changed.
- The server keeps the newest report per runner in memory and serves it. After a restart there is
  none until an agent runs again.

### What can be asked

`GET /api/v1/usage?from=&to=&by=` returns totals and a breakdown over a time range.

- `from` and `to` are Unix milliseconds; the default range is the last 30 days. The range is at
  most 366 days.
- `by` is `day` (default), `session`, `repo`, `runner` or `model`.
- Cost and tokens are attributed to the moment they were reported, by taking the difference
  between consecutive `usage` events of an agent (totals are cumulative per agent process and may
  restart from zero when a process restarts without resuming; a decrease is treated as a restart,
  never as negative usage).
- `by=day` uses the server's local time zone and returns every day of the range, including days
  with nothing. `by=repo` attributes a session's usage to its primary repo; sessions without a
  repo are grouped under no repo. `by=session` returns root sessions, each including its
  descendants' usage, most expensive first, at most 200.
- The numbers come from an index projection that can be rebuilt from the logs.

`GET /api/v1/usage/limits` returns the newest limit windows per runner, with when they were
reported.

### What the user sees

A Usage page: total cost and tokens for the range with a range picker (7, 30, 90 days), a bar per
day, a table by session with a link to each, a breakdown by repo and by model, and the plan
limits as labelled meters with their reset times ("5-hour window: 62% used, resets at 18:00").
When a window is refusing requests the app says so wherever sessions are started, with the reset
time. Cost is what the agent CLI reports; on a subscription plan it is an estimate of value used,
not a bill, and the page says so.

## 2. Issues

- `GET /api/v1/repos/{id}/issues?state=&q=` lists a repo's GitHub issues through `gh`: number,
  title, state, labels, author, updated time, URL. `state` is `open` (default) or `closed`; `q`
  filters by text. At most 50, newest first. A repo with no GitHub remote, or `gh` unavailable,
  is a 409 with the reason.
- `GET /api/v1/repos/{id}/issues/{number}` returns one issue with its body (Markdown).
- A session can be created **from an issue**: `CreateSessionRequest.issue` names the repo and the
  number. The server fetches the issue and stores a reference on the session (`Session.issue`:
  repo id, number, title, URL). The session's prompt is what the user wrote, followed by the
  issue's title, body and URL under a clear heading, so the agent has the text without needing
  network access. The repo is attached as the primary repo unless the request says otherwise.
- When such a session ships, the pull request for the issue's repo gets `Closes #<number>` in its
  description (inside Calin's own block). Pull requests of other repos get a link to the issue.
- The web app: an Issues tab per repo in the new-session flow (search, pick one, the prompt box
  keeps what you type as extra instructions), the issue shown as a chip on the session, and a
  link to it.

Other trackers are not part of this milestone.

## 3. Automations

An automation starts a session by itself: on a schedule, or when something appears on GitHub.

### Definition

Stored in `<home>/automations.json` (machine-local; the server's machine is where they run).

| Field | Meaning |
| --- | --- |
| `name` | Shown in lists. |
| `enabled` | Off by default when created. |
| `trigger` | `schedule` with a five-field cron expression in the server's local time, or `issue` with a repo and an optional label, or `pull_request` with a repo. |
| `session` | What to start: prompt, workflow, repos, runner, model, permission mode. |

- The prompt is a template. `{{date}}` is the local date. For an `issue` or `pull_request`
  trigger, `{{number}}`, `{{title}}`, `{{body}}`, `{{url}}` and `{{author}}` describe the item.
- An automated session runs unattended, so its permission mode must be `autonomous` or
  `accept_edits`; `ask` is refused when the automation is saved.
- An `issue` trigger starts the session from that issue (section 2), so its pull request closes it.

### When it fires

- **Schedule**: at each matching minute while the server runs. Times missed while the server was
  down are not made up afterwards.
- **Issue / pull request**: the server asks GitHub through `gh` every 5 minutes for open items
  of that repo (with that label, for issues) and fires once for each item it has not seen. When
  the automation is enabled, everything that already exists is marked as seen, so enabling never
  starts a backlog. What has been seen is remembered across restarts.
- At most one session of an automation is active at a time (queued, preparing, running or
  waiting). A fire that finds one still active is skipped and recorded as skipped. A schedule
  therefore never piles up; a trigger keeps the item unseen and tries again at the next poll.
- An automation is switched off automatically after 3 consecutive runs that end `failed`, with
  the reason recorded, so a broken one cannot burn the plan unattended.
- Sessions started by automations count against the queue like any other and are marked with the
  automation that started them (`Session.automation_id`).
- Dry run: schedules fire; GitHub triggers do not poll.

### Routes

`GET /automations`, `POST /automations`, `GET`/`PUT`/`DELETE /automations/{id}`,
`POST /automations/{id}/run` (start one now, whatever the trigger; for an issue or pull request
trigger the request names the item's number), and `GET /automations/{id}/runs` (the last 50
fires: when, what fired it, the session started or why it was skipped).

Each automation is returned with its next scheduled time (for a schedule) or its last poll time
and result (for a trigger), and its last run.

### What the user sees

An Automations page: the list with trigger in words ("Every weekday at 08:00", "New issue labelled
`agent` in api"), enabled switch, next and last run with the session's status; an editor with the
same repo, workflow and permission controls as the new-session form, a cron field that shows the
next three times it would fire, and a "Run now" button; the run history. An automation that was
switched off after failures says so and why.

## 4. Home

The sessions list gains a summary line above it: how many sessions need the user (waiting, or in
review), how many are running or queued, and the plan limit that is closest to full. "Needs you"
sessions sort first.

## 5. Not in this milestone

- Other trackers (Linear, Jira), webhooks from GitHub (polling only), and automations that react
  to pushes or comments.
- Cost budgets across sessions. A workflow's own budget remains the only spending limit.
