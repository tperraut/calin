# Calin

A self-hosted orchestrator for coding agents. One small server runs agent CLIs in git worktrees,
records what they do, and shows it live in a web app you can use from a phone.

Calin is inspired by [cezar](https://github.com/open-mercato/cezar) and differs in three ways:

- **A session spans several repositories.** Start from a home repo, attach the repos the work
  touches, and review every change in one place.
- **Orchestration is a graph.** Who spawned whom and who said what to whom is recorded and drawn.
- **The workbench is built in.** Editor, diff, terminal and search are part of the app.

**Status: early.** The design is written and the skeleton is being built. See
[docs/status.md](docs/status.md) and the [design spec](docs/specs/2026-11-26-calin-design.md).

## Development

You need Rust (the version in `rust-toolchain.toml`) and [Vite+](https://viteplus.dev), which
brings its own Node and Bun.

```sh
make install     # web dependencies
make check       # the validation gate
make dev-server  # server with the mock agent on 127.0.0.1:4680
make dev-web     # web dev server
```

`make help` lists every target. [AGENTS.md](AGENTS.md) describes the layout and the rules.

## Running it

```sh
make build                       # web app, then the binary that embeds it
CALIN_DRY_RUN=1 target/release/calin serve
```

Open <http://127.0.0.1:4680>. With `CALIN_DRY_RUN=1` every agent is simulated, so no agent CLI or
login is needed. The server only listens on loopback addresses until sign-in exists.

| Variable | Effect |
| --- | --- |
| `CALIN_HOME` | Data directory. Default `~/.calin`. `calin serve --home <dir>` overrides it. |
| `CALIN_DRY_RUN=1` | Run every agent on the built-in mock runner. |
| `CALIN_MOCK_DELAY_MS` | Delay between the mock runner's chunks. Default 40. |
| `CALIN_CLAUDE_BIN` | Path of the Claude Code CLI. Default `claude` on `PATH`. When it runs, `claude` is the default runner; without it sessions run on the mock runner. |
| `CALIN_LOG` | Log filter, for example `debug` or `calin_core=trace`. Default `info`. |

## License

MIT. See [LICENSE](LICENSE).
