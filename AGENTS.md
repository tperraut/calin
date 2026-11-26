# AGENTS.md: working in this repository

Calin is a self-hosted orchestrator for coding agents. One Rust server runs agent CLIs in git
worktrees, records everything they do as an append-only event log, and serves a SolidJS web app
that shows sessions live. A session can span several repositories.

Read `docs/specs/2026-11-26-calin-design.md` before changing behavior. `docs/status.md` says which
milestone is in progress and what is done.

## Commands

Everything goes through `make`, which puts the toolchains on `PATH` itself. Do not call `cargo`,
`vp` or `bun` from a bare shell and assume they resolve.

| Command | What it does |
| --- | --- |
| `make check` | The validation gate: Rust format, clippy, tests, generated-type drift, web check, web tests, web build |
| `make gen` | Regenerate `apps/web/src/gen/` from `crates/calin-proto` |
| `make fmt` | Format Rust and web code |
| `make dev-server` | Server with the mock runner on `127.0.0.1:4680` |
| `make dev-web` | Vite dev server, proxying `/api` to the server |
| `make build` | Web app, then the release binary that embeds it |

`make check` must pass before any commit. If you need a narrower loop, run the underlying command
with the same `PATH` the Makefile exports.

## Layout

| Path | What lives there | Rules |
| --- | --- | --- |
| `crates/calin-proto` | Every type that crosses the network: REST bodies, WebSocket frames, events | The single source of truth. Change a shape here first, run `make gen`, then use it. Never edit `apps/web/src/gen/` by hand. New fields on existing types are optional so old logs still parse. |
| `crates/calin-core` | Sessions, the store (SQLite index + NDJSON logs), the event bus, the session manager | No HTTP and no process spawning here. The log is the source of truth; the index must be rebuildable from it. |
| `crates/calin-git` | Everything that runs `git` or touches a checkout: worktrees, changes, diffs, directory listings, file reads and writes | A blocking library: callers use `spawn_blocking`. Shells out to the `git` CLI. Every path from outside goes through its one path-safety function. Never deletes a branch. |
| `crates/calin-term` | Pseudo-terminals for the built-in terminal | No HTTP here; the server bridges a terminal to its socket. |
| `crates/calin-agents` | The runner interface and its implementations (`mock`, later `claude`, `acp`) | Nothing runner-specific leaks past the interface. Every runner emits `calin_proto::EventBody` only. A runner is tested against recorded fixtures in `fixtures/`. |
| `crates/calin-server` | axum routes, the WebSocket, static assets | Routes live under `/api/v1` and match the table in `calin-proto/src/api.rs` exactly. Handlers stay thin and call `calin-core`. |
| `crates/calin-cli` | The `calin` binary | Argument parsing and wiring only. |
| `apps/web` | The SolidJS app | See "Web app" below. |
| `docs/specs` | Dated design specs and diagrams | A feature that changes behavior starts or updates a spec. |
| `fixtures` | Recorded agent transcripts | Never edited by hand; re-record instead. |

## Invariants

- **Events are append-only.** An event that was written is never changed or removed. `seq` starts
  at 1 and increases by one per event within a session, with no gaps.
- **Log first.** An event is appended to the session's log before it is indexed or sent to any
  client. A client never sees an event that a restart would lose.
- **Replay is exact.** Subscribing with `after_seq = n` delivers every event with `seq > n` once,
  in order, then live events. No duplicates and no gaps across the replay-to-live switch.
- **Zero configuration.** Every setting has a working default. Opt-in behavior hides behind a
  `CALIN_*` environment variable, documented in the README the same commit it is added.
- **Dry run always works.** `CALIN_DRY_RUN=1` runs every agent on the mock runner with no login
  and no network. Tests and end-to-end checks rely on it.
- **Nothing merges on its own.** No code path merges, force-pushes or deletes a user's branch.

## Rust

- Edition 2024, `unsafe` is forbidden, clippy warnings are errors.
- Errors: `thiserror` enums in libraries, `anyhow` only in the binary. No `unwrap`/`expect`
  outside tests, except on invariants explained in a comment.
- Async is tokio. Blocking work (SQLite, git, file walks) goes through `spawn_blocking` or a
  dedicated thread; never block the runtime.
- Tests sit next to the code they cover. Anything touching disk uses `tempfile`; never the real
  `~/.calin`. The data directory always comes from one resolver that honors `CALIN_HOME`.

## Web app

- **This is Solid 2**, not Solid 1.x: `solid-js@2`, `@solidjs/web`, `@solidjs/vite-plugin`. Much
  of what models remember is the 1.x API. Before using a Solid API, confirm it exists in the
  installed package's types and docs under `apps/web/node_modules/`.
- Solid is not React. Components run once. Do not destructure props, do not early-return on
  reactive state, read signals where the value is used, and use Solid's control-flow components
  for conditionals and lists.
- Vite+ is the toolchain: `vp dev`, `vp build`, `vp check`, `vp test`. Test imports come from
  `vite-plus/test`. Configuration lives in `vite.config.ts`; there is no separate Vitest, ESLint
  or Prettier configuration.
- Bun is the package manager, through `vp install` / `vp add`. Add a dependency only when the
  task needs it, and check that it supports Solid 2 before adding it.
- Wire types are imported from `src/gen/`. Never restate one by hand.
- All server state arrives over REST and the one event WebSocket. Each open terminal has its own
  socket. Both kinds are opened only from `src/api/`; components do not call `fetch` or open
  sockets themselves.
- Phone first: every screen must work at 360 px wide, with touch targets of at least 44 px.
  Light and dark themes follow the system setting.

## Comments and prose

Comments say why, in one or two lines; they do not narrate the code. Cite the spec section rather
than restating it. Commit messages lead with what changed.

## Related documents

- `docs/specs/2026-11-26-calin-design.md`: the design, with diagrams in `docs/specs/diagrams/`.
- `docs/status.md`: milestone progress.
- `crates/calin-proto/src/api.rs`: the REST route table.
- `crates/calin-proto/src/ws.rs`: the WebSocket protocol.
- `crates/calin-proto/src/terminal.rs`: the terminal socket.
