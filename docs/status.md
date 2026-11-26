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
- The graph layout spike moved to the start of M3, where its result is used.

## M1: One session, real agent, real repos (in progress, branch `m1`)

Sessions take a repo set from the start, so M1 already creates one worktree per attached repo and
shows changes grouped by repo. What stays in M2 is the home repo's repo list, attaching a repo
mid-session, and pull requests.

- [x] Contract: repos, repo set, permissions, interrupt, changes, diffs, files, terminal socket
- [ ] `calin-git`: worktrees, changes against a base, file diff, tree, safe file read and write
- [ ] `calin-term`: pseudo-terminal library
- [ ] `calin-agents`: Claude Code runner over stream-json, tested against the recorded fixtures;
      mock runner asks for permission
- [ ] Core and server: repo registry, repo set and worktrees, permission flow, interrupt, continue
      after stop or failure, file-change events, all new routes, terminal socket
- [ ] Web: repo picker, permission mode, permission cards, interrupt
- [ ] Web: Changes with diff view, Files with editor, Terminal
- [ ] End to end with the mock runner, then a real Claude Code session in a real repo

## Later

M2 multi-repo · M3 graph · M4 workflows · M5 sign-in, server install, PWA, desktop · M6 integrations

## Decisions since the spec

- 2026-11-26: Solid 2.0 is the current stable line and is what the scaffold installs.
- 2026-11-26: Sign-in will be OpenID Connect (Auth0 or another provider) rather than passkeys.
- 2026-11-26: The home repo does not exist yet. It will hold the repo list and workflows, and a
  script that links its skills and instructions into the host's global Claude configuration.
