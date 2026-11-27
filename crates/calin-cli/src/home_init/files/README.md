# Home repo

This repository is where every Calin session starts. It lists the other
repositories you work in, holds the workflows sessions can run, and keeps the
skills and instructions your agents load wherever they work. Calin itself keeps
only runtime state; what is here is your configuration, versioned with git.

## Layout

| Path | What it is |
| --- | --- |
| `.calin/repos.yaml` | The repos you work in, by name, with their git URL, a `setup` command and `checks`; and named sets of repos |
| `.calin/workflows/*.yaml` | Workflows a session can run. The file name is the workflow's id |
| `skills/`, `agents/`, `commands/` | Claude Code skills, subagents and slash commands, linked into the global configuration by the script below |
| `CLAUDE.md` | Instructions for every session (create it when you need it) |
| `scripts/link-claude.sh` | Links the four items above into `~/.claude` (or `$CLAUDE_CONFIG_DIR`) |

## The repo list

`.calin/repos.yaml` names each repo once:

```yaml
repos:
  api:
    url: git@github.com:you/api.git   # how Calin finds or clones it
    setup: cargo fetch                # run in each new worktree before the agent starts
    checks: ["cargo test"]            # run by a workflow's check step
  web: { url: git@github.com:you/web.git, checks: ["bun run check"] }
sets:
  storefront: [api, web]              # attach several repos in one click
```

On each machine Calin matches a repo to a local clone by its git URL, looking
under the scan roots set in Calin's Settings. A repo declared without a `url`
is matched by its name instead: a clone in a folder of that name directly under
a scan root. A declared repo with no clone yet can be cloned from Calin's
Settings page.

Docs and instructions in this repo should refer to other repos by these names,
not by their paths, which differ from machine to machine.

## Workflows

Each file in `.calin/workflows/` is a small graph: agent turns, checks, loops,
gates and shipping. The two starters here are commented. Calin re-reads them
when they change; a file with an error is listed with the problem in Settings
and cannot be chosen until it is fixed. The format is in Calin's
`docs/specs/2026-11-27-workflows.md`.

## Pointing Calin at this repo

In Calin, open Settings, set **Home repo** to the absolute path of this
repository, add the folders that hold your clones as **Scan roots**, and save.
Commit this repository first: sessions start in it.

## Linking skills and instructions

```sh
scripts/link-claude.sh --dry-run   # see what it would do
scripts/link-claude.sh             # create the links
scripts/link-claude.sh --unlink    # remove them again
```

It only creates symbolic links, and leaves alone anything it did not create.
