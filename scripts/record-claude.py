#!/usr/bin/env python3
"""Record a Claude Code stream-json conversation, both directions, for a fixture.

The CLI is driven the way the Claude runner drives it: one long-lived process,
user turns and permission answers on stdin, everything it prints on stdout.

Usage:
    scripts/record-claude.py SCENARIO.json --out fixtures/raw/claude-<version>/<name>.jsonl
        [--workdir DIR] [--scrub fixtures/claude/<version>/<name>.jsonl]

The recording is RAW: it holds local paths and account details. It belongs in
the git-ignored fixtures/raw/. Pass --scrub to also run scripts/scrub-fixture.py
on it, then read the scrubbed file before committing it.

A scenario is a JSON object. Only `turns` is required.

    {
      "files": {"notes.txt": "one\n"},     files created in the working directory first
      "turns": ["first prompt", "..."],     one user message each; the next is sent after `result`
      "model": "haiku",                     default "haiku"
      "permission_mode": "default",         default "default"
      "args": ["--tools", "Read,Write"],    extra CLI arguments
      "permissions": ["deny", "allow"],     answers to permission requests, in order; the last
                                            one repeats; default ["allow"]
      "deny_message": "Not allowed.",       sent with a deny
      "interrupt": {                        send an interrupt request ...
        "turn": 1,                          ... during this turn (1-based) ...
        "after_tool_use": "Bash",           ... once the CLI reported a call of this tool ...
        "delay": 4.0                        ... and this many seconds passed
      },
      "resume_from": "edit-existing-file",  continue the conversation of that scenario's raw
                                            recording (same --out directory), in its directory
      "persist": true,                      keep the conversation on disk so it can be resumed
      "timeout": 180                        give up after this many seconds, default 180
    }

Without --workdir the CLI runs in a new temporary directory. Never record inside
a repository: the transcript would describe it.
"""

import argparse
import json
import os
import subprocess
import sys
import tempfile
import threading
import time


def load_previous(out_path: str, name: str) -> tuple[str, str]:
    """The working directory and conversation id of an earlier raw recording."""
    path = os.path.join(os.path.dirname(os.path.abspath(out_path)), name + ".jsonl")
    workdir = session = None
    with open(path, encoding="utf-8") as f:
        for line in f:
            record = json.loads(line)
            if record.get("dir") == "meta" and "workdir" in record:
                workdir = record["workdir"]
            msg = record.get("msg") or {}
            if record.get("dir") == "out" and msg.get("type") == "system" and msg.get("subtype") == "init":
                session = msg.get("session_id")
    if not workdir or not session:
        sys.exit(f"error: {path} names no working directory or conversation id")
    return workdir, session


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("scenario")
    ap.add_argument("--out", required=True, help="raw recording to write (under fixtures/raw/)")
    ap.add_argument("--workdir", help="directory to run the CLI in (default: a new temporary one)")
    ap.add_argument("--scrub", help="also write the scrubbed fixture here")
    ap.add_argument("--claude", default=os.environ.get("CALIN_CLAUDE_BIN", "claude"))
    args = ap.parse_args()

    with open(args.scenario, encoding="utf-8") as f:
        scenario = json.load(f)
    turns = scenario["turns"]
    answers = scenario.get("permissions") or ["allow"]
    interrupt = scenario.get("interrupt")
    timeout = scenario.get("timeout", 180)

    resume = None
    if scenario.get("resume_from"):
        workdir, resume = load_previous(args.out, scenario["resume_from"])
    else:
        workdir = args.workdir or tempfile.mkdtemp(prefix="calin-record-")
    workdir = os.path.realpath(workdir)
    os.makedirs(workdir, exist_ok=True)
    for name, content in scenario.get("files", {}).items():
        path = os.path.join(workdir, name)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w", encoding="utf-8") as f:
            f.write(content)

    cmd = [
        args.claude, "-p",
        "--input-format", "stream-json",
        "--output-format", "stream-json",
        "--verbose",
        "--include-partial-messages",
        "--permission-prompt-tool", "stdio",
        "--permission-mode", scenario.get("permission_mode", "default"),
        "--model", scenario.get("model", "haiku"),
    ]  # fmt: skip
    if not scenario.get("persist") and not resume:
        cmd.append("--no-session-persistence")
    if resume:
        cmd += ["--resume", resume]
    cmd += scenario.get("args", [])

    os.makedirs(os.path.dirname(os.path.abspath(args.out)), exist_ok=True)
    rec = open(args.out, "w", encoding="utf-8")
    lock = threading.Lock()

    def record(obj: dict) -> None:
        with lock:
            rec.write(json.dumps(obj, ensure_ascii=False) + "\n")
            rec.flush()

    record({"dir": "meta", "workdir": workdir, "cmd": cmd[1:], "scenario": scenario})
    proc = subprocess.Popen(
        cmd, cwd=workdir, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        text=True, bufsize=1, start_new_session=True,
    )  # fmt: skip

    def send(obj: dict) -> None:
        record({"dir": "in", "msg": obj})
        try:
            proc.stdin.write(json.dumps(obj) + "\n")
            proc.stdin.flush()
        except (BrokenPipeError, ValueError):
            pass

    def user(text: str) -> None:
        send({"type": "user", "message": {"role": "user", "content": [{"type": "text", "text": text}]}})

    def give_up() -> None:
        time.sleep(timeout)
        print(f"error: no end after {timeout}s, killing the CLI", file=sys.stderr)
        try:
            os.killpg(proc.pid, 9)
        except ProcessLookupError:
            pass

    threading.Thread(target=give_up, daemon=True).start()

    turn = 1
    asked = 0
    interrupted = False
    user(turns[0])
    for line in proc.stdout:
        line = line.strip()
        if not line:
            continue
        try:
            msg = json.loads(line)
        except ValueError:
            record({"dir": "out_raw", "line": line})
            continue
        record({"dir": "out", "msg": msg})
        kind = msg.get("type")

        if kind == "control_request" and msg.get("request", {}).get("subtype") == "can_use_tool":
            request = msg["request"]
            answer = answers[min(asked, len(answers) - 1)]
            asked += 1
            if answer == "allow":
                response = {"behavior": "allow", "updatedInput": request.get("input", {})}
            else:
                response = {"behavior": "deny", "message": scenario.get("deny_message", "Denied by the user.")}
            send({
                "type": "control_response",
                "response": {"subtype": "success", "request_id": msg["request_id"], "response": response},
            })  # fmt: skip

        if (
            kind == "assistant"
            and interrupt
            and not interrupted
            and turn == interrupt.get("turn", 1)
            and any(
                block.get("type") == "tool_use" and block.get("name") == interrupt.get("after_tool_use")
                for block in msg.get("message", {}).get("content", [])
            )
        ):
            interrupted = True

            def later() -> None:
                time.sleep(interrupt.get("delay", 4.0))
                send({"type": "control_request", "request_id": "interrupt-1", "request": {"subtype": "interrupt"}})

            threading.Thread(target=later, daemon=True).start()

        if kind == "result":
            if turn < len(turns):
                turn += 1
                user(turns[turn - 1])
            else:
                break

    try:
        proc.stdin.close()
    except BrokenPipeError:
        pass
    try:
        proc.wait(timeout=20)
    except subprocess.TimeoutExpired:
        os.killpg(proc.pid, 9)
        proc.wait()
    stderr = proc.stderr.read()
    record({"dir": "meta", "exit": proc.returncode, "stderr": stderr[-2000:], "turns_done": turn})
    rec.close()
    print(f"recorded {args.out} (exit {proc.returncode}) in {workdir}")
    if stderr.strip():
        print("stderr: " + stderr.strip()[-500:])

    if args.scrub:
        scrubber = os.path.join(os.path.dirname(os.path.abspath(__file__)), "scrub-fixture.py")
        return subprocess.call([sys.executable, scrubber, args.out, args.scrub, "--workdir", workdir])
    return 0


if __name__ == "__main__":
    sys.exit(main())
