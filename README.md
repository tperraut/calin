# Calin

A self-hosted orchestrator for coding agents. One small server runs agent CLIs in git worktrees,
records what they do, and shows it live in a web app you can use from a phone.

Calin is inspired by [cezar](https://github.com/open-mercato/cezar) and differs in three ways:

- **A session spans several repositories.** Start from a home repo, attach the repos the work
  touches, and review every change in one place.
- **Orchestration is a graph.** Who spawned whom and who said what to whom is recorded and drawn.
- **The workbench is built in.** Editor, diff, terminal and search are part of the app.

**Status: early, and moving fast.** Sessions across several repos, workflows, the orchestration
graph and sign-in work; nothing has been run on a real server or a real phone yet. See
[docs/status.md](docs/status.md) and the [design spec](docs/specs/2026-11-26-calin-design.md).

## Getting started

The [user guide](docs/guide.md) covers running Calin, sessions, workflows and running it on a
server. To set up a home repo (the repository every session starts from, with your repo list and
workflows):

```sh
calin home init ~/code/home --scan ~/code
```

It writes `.calin/repos.yaml` from the clones it finds, two starter workflows, a README and a
script that links your skills into `~/.claude`. It never overwrites a file and never commits.

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
login is needed. The server always listens on a loopback address. To reach it from other
devices, run it behind a reverse proxy with sign-in: see [Running on a server](#running-on-a-server).

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
| `CALIN_PUSH_CONTACT` | Public mode, optional: a `mailto:` address, for example `mailto:admin@example.com`, that push services can reach whoever runs the server at. Default: the public URL. Push notifications keep their key pair in `push-vapid-key.json` and the devices that subscribed in `push-subscriptions.json`, in the data directory. |
| `CALIN_UNSAFE_DEV_ALLOW_HTTP_LOOPBACK=1` | **Unsafe, for development and automated browser tests only.** Lets `CALIN_PUBLIC_URL` and `CALIN_OIDC_ISSUER` be plain `http` on `localhost` or `127.0.0.1` with an explicit port, and push subscriptions point at a push service on `http://127.0.0.1:<port>`. Never set it on a real server: sign-in over plain HTTP protects nothing. Any value other than `1` is an error. |
| `CLAUDE_CODE_ENABLE_TODO_TOOLS=1` | Not Calin's own: it is passed on to Claude Code, which then offers its task tools to every model. A session shows the agent's plan only when the agent has those tools. |

### Trying orchestration without an agent

In dry run the simulated agent follows a few words in the prompt, so you can see child sessions,
notes, reports and questions in the graph and timeline without using any plan:

| In the prompt | What the simulated agent does |
| --- | --- |
| `demo: orchestrate` | Dispatches two child sessions, tells you so, answers each child's report and then sums up. |
| `demo: orchestrate demo: note` | The same, and the second child also sends its parent a note before reporting. |
| `demo: ask` | Asks you a question with two suggested answers, then repeats your answer. |

A child session receives `demo: child` (or `demo: note`) in its brief: it changes a notes file and
reports back.

## Running on a server

Calin can run on a small Linux machine behind a domain, with sign-in through any OpenID Connect
provider (Auth0, for example) and an allow-list of who may sign in.

```sh
calin server install --domain calin.example.com \
  --issuer https://your-tenant.eu.auth0.com/ --client-id <id> --allow you@example.com
```

This writes a systemd unit, an environment file and a Caddy site to a staging folder, and prints
the commands to run as root. It changes nothing itself. `calin server check` verifies the result
and `calin server uninstall` prints how to remove it.

Whoever signs in gets a shell on that machine, so keep the allow-list short. Agents run as the
service user and are not sandboxed. If you use a reverse proxy other than Caddy, it must keep the
`Host` header and set `X-Forwarded-For`; a proxy that does neither makes outside requests look
local. The design is in [docs/specs/2026-11-27-remote-access.md](docs/specs/2026-11-27-remote-access.md).

Once it is served from its domain, the web app can be added to a phone's home screen and can send
notifications when a session needs you (Settings → Notifications).

## License

MIT. See [LICENSE](LICENSE).
