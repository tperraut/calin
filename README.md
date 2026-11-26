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

## License

MIT. See [LICENSE](LICENSE).
