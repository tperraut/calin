import { describe, expect, it } from "vite-plus/test";
import type { WorkflowRun } from "../gen/WorkflowRun";
import type { WorkflowNote } from "../state/sessionReducer";
import { node, specGraph, workflowInfo } from "../test/workflows";
import {
  finishRun,
  foldNode,
  isFailurePort,
  isUsable,
  loopProgress,
  mergeRun,
  noteWords,
  problemLines,
  QUICK_WORKFLOW,
  sentWorkflow,
  shapeSummary,
  startRun,
} from "./workflow";

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
    [finished("commit", "save", "failed", "hook refused", true), "Commit failed: save", "danger"],
    [finished("gate", "gate", "timeout", "nobody came", true), "Gate: timeout", "danger"],
    [finished("gate", "gate", "later", "asked again"), "Gate: later", "neutral"],
    [
      finished("ship", "ship", "failed", "push refused", true),
      "Ship ship left through failed",
      "danger",
    ],
    [finished("teleport", "beam", "done", "arrived"), "teleport beam left through done", "neutral"],
  ])("finishes %#: %s", (note, text, tone) => {
    const words = noteWords(note);
    expect(words.text).toBe(text);
    expect(words.tone).toBe(tone);
    if (note.what === "node_finished") expect(words.detail).toBe(note.outcome);
  });

  it("says an outcome only when it is more than the port it left through", () => {
    expect(noteWords(finished("agent", "review", "approve", " approve ")).detail).toBeNull();
    expect(noteWords(finished("agent", "review", "changes", "   ")).detail).toBeNull();
    expect(noteWords(finished("agent", "review", "changes", " typo ")).detail).toBe("typo");
  });

  it("names a step of a kind it does not know by its kind and label", () => {
    expect(noteWords(started("teleport", "beam", ""))).toMatchObject({
      text: "teleport: beam",
      detail: null,
    });
    expect(noteWords(started("agent", "implement", "  "))).toMatchObject({
      text: "Agent step: implement",
      detail: null,
    });
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

  it("keeps the folded budget, and the server's entry count for a loop with no known limit", () => {
    const local = folded();
    const served: WorkflowRun = {
      ...local,
      budget_usd: 9,
      nodes: local.nodes.map((one) => (one.id === "retry" ? { ...one, iteration: 1 } : one)),
    };
    const merged = mergeRun(local, served);
    expect(merged?.budget_usd).toBe(5);
    expect(merged?.nodes.find((one) => one.id === "retry")).toMatchObject({
      runs: 2,
      iteration: 1,
      max: null,
    });
    const unbudgeted = { ...local, budget_usd: null };
    expect(mergeRun(unbudgeted, served)?.budget_usd).toBe(9);
    // A node the server does not list keeps what the events said.
    expect(mergeRun(local, { ...served, nodes: [] })?.nodes).toEqual(local.nodes);
  });

  it("shows the server's run when no event folded one, and ignores another workflow's", () => {
    const served = { ...folded(), cost_usd: 3 };
    expect(mergeRun(null, served)).toBe(served);
    expect(mergeRun(null, undefined)).toBeUndefined();
    const local = folded();
    expect(mergeRun(local, { ...served, workflow_id: "other" })).toBe(local);
  });
});

describe("a run folded from its events", () => {
  const begun = (): WorkflowRun =>
    startRun({
      type: "workflow_started",
      workflow_id: "itr",
      name: "Implement",
      graph: specGraph(),
      budget_usd: null,
    });
  const on = (node_id: string) =>
    ({ type: "workflow_node", node_id, phase: "started", port: null, outcome: null }) as const;
  const off = (node_id: string, port: string | null, outcome: string | null = null) =>
    ({ type: "workflow_node", node_id, phase: "finished", port, outcome }) as const;
  const nodeOf = (run: WorkflowRun, id: string) => run.nodes.find((one) => one.id === id);

  it("starts with every node idle, nothing taken and nothing spent", () => {
    const run = begun();
    expect(run.nodes.map((one) => one.id)).toEqual(specGraph().nodes.map((one) => one.id));
    expect(nodeOf(run, "tests")).toEqual({
      id: "tests",
      state: "idle",
      runs: 0,
      last_port: null,
      outcome: null,
      iteration: null,
      max: null,
    });
    expect(run).toMatchObject({ taken: [], status: "running", reason: null, cost_usd: 0 });
  });

  it("counts each start of a node, and a loop's entries up to its limit", () => {
    const limited: WorkflowRun = {
      ...begun(),
      nodes: begun().nodes.map((one) => (one.id === "retry" ? { ...one, max: 2 } : one)),
    };
    const once = foldNode(limited, on("tests"));
    expect(nodeOf(once, "tests")).toMatchObject({ state: "active", runs: 1, iteration: null });
    expect(nodeOf(foldNode(once, on("tests")), "tests")?.runs).toBe(2);
    const loops = [on("retry"), on("retry"), on("retry")].reduce(foldNode, limited);
    expect(nodeOf(loops, "retry")).toMatchObject({ runs: 3, iteration: 2, max: 2 });
  });

  it("finishes a node through its port, and takes the edge wired to that port", () => {
    const run = foldNode(foldNode(begun(), on("tests")), off("tests", "fail", "1 failed"));
    expect(nodeOf(run, "tests")).toMatchObject({
      state: "finished",
      last_port: "fail",
      outcome: "1 failed",
    });
    expect(run.taken).toEqual([{ from: "tests", port: "fail", to: "retry" }]);
  });

  it("keeps the last port of a node that finishes through none, or through one not wired", () => {
    const first = foldNode(begun(), off("tests", "pass"));
    const portless = foldNode(first, off("tests", null));
    expect(nodeOf(portless, "tests")?.last_port).toBe("pass");
    expect(portless.taken).toEqual([{ from: "tests", port: "pass", to: "review" }]);
    const unwired = foldNode(begun(), off("ship", "failed"));
    expect(nodeOf(unwired, "ship")?.last_port).toBe("failed");
    expect(unwired.taken).toEqual([]);
  });

  it("changes nothing for a node it does not know or a phase it has no name for", () => {
    const run = begun();
    expect(foldNode(run, on("ghost"))).toBe(run);
    // What a newer server could send: a phase this build has no type for.
    expect(foldNode(run, { ...on("tests"), phase: "paused" as "started" })).toBe(run);
  });

  it("ends the run: the active node finishes, the others stay as they were", () => {
    const run = foldNode(foldNode(begun(), off("implement", "done")), on("tests"));
    const ended = finishRun(run, { type: "workflow_finished", status: "failed", reason: "budget" });
    expect(ended).toMatchObject({ status: "failed", reason: "budget" });
    expect(ended.nodes.map((one) => [one.id, one.state]).slice(0, 4)).toEqual([
      ["start", "idle"],
      ["implement", "finished"],
      ["tests", "finished"],
      ["retry", "idle"],
    ]);
  });
});

describe("failure ports", () => {
  it("takes a node's own flag for its port, and reads the usual names otherwise", () => {
    const check = node("tests", "check", "", ["pass", "fail"]);
    expect(isFailurePort(check, "fail")).toBe(true);
    expect(isFailurePort(check, "pass")).toBe(false);
    const custom = {
      ...check,
      ports: [
        { name: "pass", failure: false },
        { name: "fail", failure: false },
        { name: "broke", failure: true },
      ],
    };
    expect(isFailurePort(custom, "fail")).toBe(false);
    expect(isFailurePort(custom, "broke")).toBe(true);
    expect(isFailurePort(undefined, "exhausted")).toBe(true);
    expect(isFailurePort(undefined, "done")).toBe(false);
  });
});

describe("loop words", () => {
  it("counts against the limit when it is known, and entries otherwise", () => {
    expect(loopProgress(2, 3)).toBe("2 of 3");
    expect(loopProgress(5, 3)).toBe("3 of 3");
    expect(loopProgress(1, null)).toBe("entered once");
    expect(loopProgress(2, null)).toBe("entered 2 times");
  });

  it("calls the last attempt an attempt", () => {
    expect(noteWords(started("loop", "retry", "", 3), 3).text).toBe("Loop: attempt 3 of 3");
  });
});

describe("workflow files", () => {
  it("can choose only a workflow that has a graph and no problem", () => {
    expect(isUsable(workflowInfo("odd", { problem: "line 1: unknown key" }))).toBe(false);
    expect(isUsable(workflowInfo("empty", { graph: null }))).toBe(false);
  });

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

  it("sends the id of the workflow picked, and nothing for Quick or none", () => {
    expect(sentWorkflow(workflowInfo("review"))).toBe("review");
    expect(sentWorkflow(workflowInfo(QUICK_WORKFLOW))).toBeUndefined();
    expect(sentWorkflow(undefined)).toBeUndefined();
  });
});
