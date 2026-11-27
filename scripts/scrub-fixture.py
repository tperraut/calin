#!/usr/bin/env python3
"""Turn a raw Claude Code stream-json recording into a fixture that is safe to commit.

A raw recording names the machine it was made on: home paths, session ids, the
account's connected services, installed skills and plugins, and usage figures.
This script keeps the protocol shape and drops or normalizes everything else.

Usage:
    scripts/scrub-fixture.py RAW.jsonl OUT.jsonl --workdir /abs/path/used/as/cwd

Input lines are either bare CLI output objects, or records written by
scripts/record-claude.py: {"dir": "in" | "out", "msg": {...}}. Output lines are
always {"dir": ..., "msg": ...}; a bare input line is treated as "out".

The script fails, and writes nothing, if the scrubbed text still contains the
user name, the host name, the home directory or the original working directory,
spelled as a path or as the CLI spells a directory in its own file names.
"""

import argparse
import getpass
import json
import os
import re
import socket
import sys

# Keys of `system/init` that describe the protocol rather than the machine.
INIT_KEEP = {
    "type",
    "subtype",
    "cwd",
    "session_id",
    "model",
    "permissionMode",
    "apiKeySource",
    "claude_code_version",
    "output_style",
    "capabilities",
    "uuid",
}

# Tools that ship with the CLI. Anything else in `tools` came from the account
# (MCP connectors, plugins) and is dropped.
BUILTIN_TOOLS = [
    "Task",
    "Bash",
    "Glob",
    "Grep",
    "Read",
    "Edit",
    "Write",
    "NotebookEdit",
    "WebFetch",
    "WebSearch",
    "TodoWrite",
    "TaskCreate",
    "TaskUpdate",
    "TaskGet",
    "TaskList",
]

# Dropped wherever they appear: opaque, large, or about the account.
DROP_KEYS = {
    "signature",
    "request_id_header",
    "memory_paths",
    "plugins",
    "mcp_servers",
    "slash_commands",
    "terminal_slash_commands",
    "skills",
    "agents",
    "analytics_disabled",
    "product_feedback_disabled",
}

# The CLI's per-user temporary directory, named after the numeric user id.
TEMP_ROOT_RE = re.compile(r"/tmp/claude-\d+\b")
UUID_RE = re.compile(r"\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b")
TIMESTAMP_RE = re.compile(r"\b\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?Z\b")
OPAQUE_ID_RE = re.compile(r"\b(msg|req|toolu)_[0-9A-Za-z]{10,}\b")


def slug(path: str) -> str:
    """A directory as the CLI names it inside its own storage: every other character a dash."""
    return re.sub(r"[^A-Za-z0-9]", "-", path)


class Scrubber:
    def __init__(self, workdir: str):
        self.workdir = workdir.rstrip("/")
        self.home = os.path.expanduser("~")
        self.user = getpass.getuser()
        self.host = socket.gethostname()
        self.ids: dict[str, str] = {}

    def forbidden(self) -> set[str]:
        """Strings that must not survive scrubbing."""
        return {self.user, self.host, self.home, self.workdir, slug(self.home), slug(self.workdir)} - {""}

    def stable(self, kind: str, value: str) -> str:
        """Map each distinct id to a small stable placeholder, so references still line up."""
        key = f"{kind}:{value}"
        if key not in self.ids:
            n = sum(1 for k in self.ids if k.startswith(kind + ":")) + 1
            self.ids[key] = f"{kind}-{n:04d}"
        return self.ids[key]

    def text(self, s: str) -> str:
        s = s.replace(self.workdir, "/work")
        s = s.replace(slug(self.workdir), "-work")
        s = s.replace(self.home, "/home/user")
        s = s.replace(slug(self.home), "-home-user")
        s = TEMP_ROOT_RE.sub("/tmp/claude", s)
        # What is left is the bare user name, for example the owner column of `ls -l`.
        s = s.replace(self.user, "user")
        s = UUID_RE.sub(lambda m: self.stable("uuid", m.group(0)), s)
        s = OPAQUE_ID_RE.sub(lambda m: self.stable(m.group(1), m.group(0)), s)
        s = TIMESTAMP_RE.sub("2026-01-01T00:00:00.000Z", s)
        return s

    def value(self, v):
        if isinstance(v, str):
            return self.text(v)
        if isinstance(v, list):
            return [self.value(x) for x in v]
        if isinstance(v, dict):
            # Keys are scrubbed too: some maps are keyed by tool-use id.
            return {self.text(k): self.value(x) for k, x in v.items() if k not in DROP_KEYS}
        return v

    def message(self, msg: dict) -> dict | None:
        kind = msg.get("type")
        subtype = msg.get("subtype")

        if kind == "system" and subtype == "init":
            out = {k: v for k, v in msg.items() if k in INIT_KEEP}
            out["tools"] = [t for t in BUILTIN_TOOLS if t in msg.get("tools", [])]
            return self.value(out)

        # Lists of the account's commands; nothing a runner needs.
        if kind == "system" and subtype == "commands_changed":
            return {"type": "system", "subtype": "commands_changed", "commands": []}

        if kind == "rate_limit_event":
            info = msg.get("rate_limit_info", {})
            return self.value(
                {
                    "type": "rate_limit_event",
                    "rate_limit_info": {
                        "status": info.get("status", "allowed"),
                        "rateLimitType": info.get("rateLimitType", "five_hour"),
                        "resetsAt": 1800000000,
                        "unifiedWindows": {
                            "five_hour": {"utilization": 0.1, "resetsAt": 1800000000, "status": "allowed"},
                            "seven_day": {"utilization": 0.1, "resetsAt": 1800000000, "status": "allowed"},
                        },
                    },
                    "uuid": msg.get("uuid", ""),
                    "session_id": msg.get("session_id", ""),
                }
            )

        return self.value(msg)


DELTA_FIELDS = {"text_delta": "text", "input_json_delta": "partial_json", "thinking_delta": "thinking"}


def rejoin_deltas(records: list[tuple[str, dict]], scrubber: Scrubber) -> None:
    """Scrub streamed chunks as a whole.

    A path can be split across two chunks, where neither half matches on its
    own. Join each content block's chunks, scrub the joined text, then cut it
    back into the same number of chunks so the streaming shape is preserved.
    """
    runs: dict[tuple, list[tuple[dict, str]]] = {}
    for direction, msg in records:
        if direction != "out" or msg.get("type") != "stream_event":
            continue
        event = msg.get("event", {})
        if event.get("type") != "content_block_delta":
            continue
        delta = event.get("delta", {})
        field = DELTA_FIELDS.get(delta.get("type"))
        if field is None or not isinstance(delta.get(field), str):
            continue
        key = (msg.get("api_message_id"), msg.get("parent_tool_use_id"), event.get("index"), field)
        runs.setdefault(key, []).append((delta, field))

    for chunks in runs.values():
        joined = scrubber.text("".join(delta[field] for delta, field in chunks))
        n = len(chunks)
        size = -(-len(joined) // n) if joined else 0
        for i, (delta, field) in enumerate(chunks):
            delta[field] = joined[i * size : (i + 1) * size] if size else ""


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("raw")
    ap.add_argument("out")
    ap.add_argument("--workdir", required=True, help="the cwd the recording was made in")
    args = ap.parse_args()

    scrubber = Scrubber(args.workdir)
    records: list[tuple[str, dict]] = []
    with open(args.raw, encoding="utf-8") as f:
        for n, line in enumerate(f, 1):
            line = line.strip()
            if not line:
                continue
            record = json.loads(line)
            if "dir" in record and "msg" in record:
                direction, msg = record["dir"], record["msg"]
            elif record.get("dir") in ("meta", "out_raw"):
                continue  # recorder bookkeeping, not protocol
            else:
                direction, msg = "out", record
            # Join split chunks on the raw text, before per-message scrubbing
            # could rewrite half of a path and hide the other half.
            records.append((direction, msg))

    rejoin_deltas(records, scrubber)
    lines = []
    for direction, msg in records:
        scrubbed = scrubber.message(msg)
        if scrubbed is not None:
            lines.append(json.dumps({"dir": direction, "msg": scrubbed}, ensure_ascii=False, sort_keys=True))

    text = "\n".join(lines) + "\n"
    leaks = sorted(w for w in scrubber.forbidden() if w in text)
    if leaks:
        print(f"error: scrubbed output still contains: {', '.join(leaks)}", file=sys.stderr)
        return 1

    os.makedirs(os.path.dirname(os.path.abspath(args.out)), exist_ok=True)
    with open(args.out, "w", encoding="utf-8") as f:
        f.write(text)
    print(f"wrote {len(lines)} lines to {args.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
