import { describe, expect, it } from "vite-plus/test";
import type { WorkflowRun } from "../gen/WorkflowRun";
import type { WorkflowNote } from "../state/sessionReducer";
import { node, specGraph, workflowInfo } from "../test/workflows";
import { isUsable, mergeRun, noteWords, problemLines, shapeSummary, startRun } from "./workflow";

const started = (nodeKind: string, nodeId: string, label: string, run = 1): WorkflowNote => ({
  what: "node_started",
  nodeId,
  nodeKind,
  label,
  run,
});

const finished = (
  nodeKind: string,
  nodeId: string,
  port: string,
  outcome: string | null = null,
  failure = false,
): WorkflowNote => ({ what: "node_finished", nodeId, nodeKind, label: "", port, outcome, failure });

describe("shapeSummary", () => {
  it("counts every step but start and end, by kind in the spec's order", () => {
    expect(shapeSummary(specGraph())).toBe("7 steps: 3 agent, 1 check, 1 loop, 1 gate, 1 ship");
  });

  it("says one step, and none", () => {
    expect(shapeSummary({ nodes: [node("a", "agent", "", ["done"])], edges: [] })).toBe(
      "1 step: 1 agent",
    );
    expect(shapeSummary({ nodes: [node("s", "start", "", ["next"])], edges: [] })).toBe("No steps");
  });

  it("puts a kind it does not know last", () => {
    const graph = {
      nodes: [
        node("x", "teleport", "", []),
        node("c", "commit", "", []),
        node("a", "agent", "", []),
      ],
      edges: [],
    };
    expect(shapeSummary(graph)).toBe("3 steps: 1 agent, 1 commit, 1 teleport");
  });
});

describe("noteWords", () => {
  it.each([
    [started("agent", "implement", "{{task}}"), "Agent step: implement", "{{task}}"],
    [started("check", "tests", "cargo test"), "Check: cargo test", null],
    [started("check", "tests", ""), "Check: tests", null],
    [started("gate", "gate", "Open pull requests?"), "Gate: Open pull requests?", null],
    [started("ask", "pick", "Which color?"), "Question: Which color?", null],
    [started("parallel", "fan", "two branches"), "Parallel: fan", "two branches"],
    [started("commit", "save", "wip"), "Commit: wip", null],
    [started("ship", "ship", "Open PRs"), "Ship: Open PRs", null],
  ])("starts %#: %s", (note, text, detail) => {
    expect(noteWords(note)).toMatchObject({ text, detail });
  });

  it("counts a loop's attempts against its limit when it is known", () => {
    expect(noteWords(started("loop", "retry", "", 2), 3).text).toBe("Loop: attempt 2 of 3");
    expect(noteWords(started("loop", "retry", "", 2)).text).toBe("Loop: attempt 2");
    expect(noteWords(started("loop", "retry", "", 4), 3).text).toBe(
      "Loop: no attempt left (3 of 3 used)",
    );
  });

  it.each([
    [finished("check", "tests", "fail", "1 test failed", true), "Check failed: tests", "danger"],
    [finished("agent", "review", "changes", "needs work"), "Verdict of review: changes", "neutral"],
    [finished("agent", "fix", "failed", "crashed", true), "Agent step failed: fix", "danger"],
    [finished("gate", "gate", "approve", "ship it"), "Gate approved", "success"],
    [finished("gate", "gate", "reject", "not yet", true), "Gate rejected", "danger"],
    [finished("loop", "retry", "exhausted", null, true), "Loop retry: no attempt left", "danger"],
    [finished("commit", "save", "nothing"), "Commit: nothing to commit", "neutral"],
  ])("finishes %#: %s", (note, text, tone) => {
    const words = noteWords(note);
    expect(words.text).toBe(text);
    expect(words.tone).toBe(tone);
    if (note.what === "node_finished") expect(words.detail).toBe(note.outcome);
  });

  it("says the run started, succeeded and failed with its reason", () => {
    expect(noteWords({ what: "started", name: "Implement" }).text).toBe("Workflow: Implement");
    expect(noteWords({ what: "finished", status: "success", reason: null })).toMatchObject({
      text: "Workflow succeeded",
      tone: "success",
    });
    expect(
      noteWords({ what: "finished", status: "failed", reason: "budget of $5.00 passed" }),
    ).toMatchObject({ text: "Workflow failed", detail: "budget of $5.00 passed", tone: "danger" });
  });
});

describe("mergeRun", () => {
  const folded = (): WorkflowRun => {
    const run = startRun({
      type: "workflow_started",
      workflow_id: "itr",
      name: "Implement",
      graph: specGraph(),
      budget_usd: 5,
    });
    return {
      ...run,
      nodes: run.nodes.map((one) => (one.id === "retry" ? { ...one, runs: 2 } : one)),
    };
  };

  it("takes loop limits and the cost from the server, counting entries from the events", () => {
    const local = folded();
    const served: WorkflowRun = {
      ...local,
      cost_usd: 1.2,
      nodes: local.nodes.map((one) =>
        one.id === "retry" ? { ...one, runs: 1, iteration: 1, max: 3 } : one,
      ),
    };
    const merged = mergeRun(local, served);
    expect(merged?.cost_usd).toBe(1.2);
    expect(merged?.nodes.find((one) => one.id === "retry")).toMatchObject({
      runs: 2,
      iteration: 2,
      max: 3,
    });
  });

  it("shows the server's run when no event folded one, and ignores another workflow's", () => {
    const served = { ...folded(), cost_usd: 3 };
    expect(mergeRun(null, served)).toBe(served);
    expect(mergeRun(null, undefined)).toBeUndefined();
    const local = folded();
    expect(mergeRun(local, { ...served, workflow_id: "other" })).toBe(local);
  });
});

describe("workflow files", () => {
  it("lists each problem on its own line, without blanks", () => {
    const broken = workflowInfo("broken", {
      graph: null,
      problem: "line 3: unknown key `prompts`\n\nnode `x`: not reachable from start  \n",
    });
    expect(problemLines(broken)).toEqual([
      "line 3: unknown key `prompts`",
      "node `x`: not reachable from start",
    ]);
    expect(isUsable(broken)).toBe(false);
    expect(isUsable(workflowInfo("fine"))).toBe(true);
    expect(problemLines(workflowInfo("fine"))).toEqual([]);
  });
});
