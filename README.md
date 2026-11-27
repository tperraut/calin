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
| `CALIN_GH_BIN` | Path of the GitHub CLI, which opens and watches pull requests with your `gh` login. Default `gh` on `PATH`. Never run in dry run, where pull requests are made up. |
| `CALIN_LOG` | Log filter, for example `debug` or `calin_core=trace`. Default `info`. |
| `CALIN_PUBLIC_URL` | The exact origin users type, for example `https://calin.example.com`. Setting it selects public mode: the server still listens on loopback only, behind a reverse proxy that terminates TLS, and every request must come from a signed-in browser or carry a device token. Must be `https`. The server refuses to start unless the four variables below it are valid too. |
| `CALIN_OIDC_ISSUER` | Public mode: the OpenID Connect provider's issuer URL, for example `https://your-tenant.eu.auth0.com/`. Register `<CALIN_PUBLIC_URL>/auth/callback` with it as the redirect URI. |
| `CALIN_OIDC_CLIENT_ID` | Public mode: the client id registered with the provider. |
| `CALIN_OIDC_CLIENT_SECRET` | Public mode, optional: the client secret. Without it Calin is a public client and relies on PKCE alone. |
| `CALIN_ALLOWED_USERS` | Public mode: who may sign in, comma-separated: email addresses, or `sub:<subject>`. Must not be empty. Sessions and device tokens of an identity removed from it end at the next start. They are kept in `auth.json` in the data directory. |
| `CALIN_UNSAFE_DEV_ALLOW_HTTP_LOOPBACK=1` | **Unsafe, for development and automated browser tests only.** Lets `CALIN_PUBLIC_URL` and `CALIN_OIDC_ISSUER` be plain `http` on `localhost` or `127.0.0.1` with an explicit port. Never set it on a real server: sign-in over plain HTTP protects nothing. Any value other than `1` is an error. |
| `CLAUDE_CODE_ENABLE_TODO_TOOLS=1` | Not Calin's own: it is passed on to Claude Code, which then offers its task tools to every model. A session shows the agent's plan only when the agent has those tools. |

## License

MIT. See [LICENSE](LICENSE).
