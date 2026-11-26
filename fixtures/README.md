# Fixtures

Recorded transcripts of real agent CLIs. Runner tests replay them and assert the
normalized events that come out, so a runner change cannot silently alter how a
real transcript is interpreted.

- `claude/<cli version>/*.jsonl`: Claude Code in `--output-format stream-json`.
  One record per line: `{"dir": "in" | "out", "msg": {...}}`, where `in` is what
  Calin wrote to the CLI's stdin and `out` is what the CLI printed.
- `raw/`: unscrubbed recordings. Git-ignored. They contain local paths and
  details of the recording account and must never be committed.

Never edit a fixture by hand. To add or refresh one, record it into `raw/`, then
run `scripts/scrub-fixture.py`, which removes machine and account details and
refuses to write a file that still contains the user name or home directory.
