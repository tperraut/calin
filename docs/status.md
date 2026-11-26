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
- There is no browser end-to-end suite in the repo yet; the run above was scripted by hand.
- The graph layout spike moved to the start of M3, where its result is used.

## M1: One session, one repo

- [ ] Claude Code runner over stream-json, with recorded fixtures
- [ ] Session worktree, stop and continue
- [ ] Permission requests answered from the UI
- [ ] Diff view, editor, terminal

## Later

M2 multi-repo · M3 graph · M4 workflows · M5 sign-in, server install, PWA, desktop · M6 integrations

## Decisions since the spec

- 2026-11-26: Solid 2.0 is the current stable line and is what the scaffold installs.
- 2026-11-26: Sign-in will be OpenID Connect (Auth0 or another provider) rather than passkeys.
- 2026-11-26: The home repo does not exist yet. It will hold the repo list and workflows, and a
  script that links its skills and instructions into the host's global Claude configuration.
