# Fixtures

Recorded transcripts of real agent CLIs. Runner tests replay them and assert the
normalized events that come out, so a runner change cannot silently alter how a
real transcript is interpreted.

- `claude/<cli version>/<name>.jsonl`: Claude Code driven the way the Claude
  runner drives it (`claude -p --input-format stream-json --output-format
  stream-json`). One record per line: `{"dir": "in" | "out", "msg": {...}}`,
  where `in` is what Calin wrote to the CLI's stdin and `out` is what the CLI
  printed.
- `claude/<cli version>/<name>.expected.jsonl`: what the runner's mapper makes
  of that recording, one output per line. The golden test in `calin-agents`
  compares against it.
- `claude/scenarios/<name>.json`: what was asked of the CLI to get the
  recording of the same name, so that it can be recorded again.
- `raw/`: unscrubbed recordings. Git-ignored. They contain local paths and
  details of the recording account and must never be committed.

## Claude Code recordings

| Recording | What it holds |
| --- | --- |
| `read-and-shell` | A read and a shell command, streamed, with no permission prompt |
| `two-turns-with-permission` | Two turns in one process, recorded without partial messages; a write is allowed, a command fails |
| `deny-permission` | A write is denied twice, with a message; the agent gives up |
| `interrupt` | An interrupt request during a long command, then a second turn in the same process |
| `edit-existing-file` | A read, then an allowed edit of an existing file |
| `resume` | A second process continues the conversation of `edit-existing-file` |
| `subagent` | A subagent runs a tool; its output carries `parent_tool_use_id` |
| `unknown-model` | The API refuses the model: an error result |
| `resume-missing` | `--resume` of a conversation that does not exist: an error result, then the CLI exits |

The first two predate `scripts/record-claude.py` and have no scenario file.
CLI 2.1.332 has no `TodoWrite` tool, so no recording holds a plan; the mapper's
plan handling is covered by a unit test instead.

## Adding or refreshing a recording

Never edit a fixture by hand. Record it again:

```sh
scripts/record-claude.py fixtures/claude/scenarios/<name>.json \
    --out fixtures/raw/claude-<version>/<name>.jsonl \
    --scrub fixtures/claude/<version>/<name>.jsonl
```

The script runs the real CLI in a new temporary directory (it costs tokens; the
scenarios use the cheapest model), answers its permission requests as the
scenario says and writes both directions to the raw file. Its docstring lists
what a scenario can ask for. `resume` continues the raw recording of
`edit-existing-file`, so record that one first, into the same directory.

`--scrub` runs `scripts/scrub-fixture.py`, which removes machine and account
details and refuses to write a file that still contains the user name, the host
name, the home directory or the working directory. Read the scrubbed file before
committing it all the same, and teach the scrubber about anything it missed.

A model does not answer the same way twice, so a new recording differs from the
old one. Regenerate its expected file and review the difference:

```sh
CALIN_BLESS=1 cargo test -p calin-agents every_fixture_maps
```
