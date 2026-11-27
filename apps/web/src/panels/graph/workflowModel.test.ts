import { describe, expect, it } from "vite-plus/test";
import type { WorkflowGraph } from "../../gen/WorkflowGraph";
import type { WorkflowNodeInfo } from "../../gen/WorkflowNodeInfo";
import type { WorkflowRun } from "../../gen/WorkflowRun";
import { finishRun, foldNode, startRun } from "../../lib/workflow";
import {
  nodeLines,
  nodeName,
  portSentence,
  portTargets,
  runOverlay,
  stateWords,
  takenPath,
} from "./workflowModel";

const info = (id: string, kind: string, ports: Array<[string, boolean]>, label = id) =>
  ({
    id,
    kind,
    label,
    ports: ports.map(([name, failure]) => ({ name, failure })),
  }) satisfies WorkflowNodeInfo;

/** The spec's example (workflows spec, section 2), with a ship whose `failed` is not wired. */
const graph: WorkflowGraph = {
  nodes: [
    info("start", "start", [["next", false]], ""),
    info(
      "implement",
      "agent",
      [
        ["done", false],
        ["failed", true],
      ],
      "{{task}}",
    ),
    info(
      "tests",
      "check",
      [
        ["pass", false],
        ["fail", true],
      ],
      "cargo test",
    ),
    info(
      "retry",
      "loop",
      [
        ["repeat", false],
        ["exhausted", true],
      ],
      "max 3",
    ),
    info(
      "fix",
      "agent",
      [
        ["done", false],
        ["failed", true],
      ],
      "The checks failed",
    ),
    info("review", "agent", [
      ["approve", false],
      ["changes", false],
      ["failed", true],
    ]),
    info(
      "gate",
      "gate",
      [
        ["approve", false],
        ["reject", true],
      ],
      "Open pull requests?",
    ),
    info("ship", "ship", [
      ["done", false],
      ["failed", true],
    ]),
    info("ok", "end", [], ""),
    info("gave-up", "end", [], ""),
  ],
  edges: [
    { from: "start", port: "next", to: "implement" },
    { from: "implement", port: "done", to: "tests" },
    { from: "tests", port: "pass", to: "review" },
    { from: "tests", port: "fail", to: "retry" },
    { from: "retry", port: "repeat", to: "fix" },
    { from: "retry", port: "exhausted", to: "gave-up" },
    { from: "fix", port: "done", to: "tests" },
    { from: "review", port: "approve", to: "gate" },
    { from: "review", port: "changes", to: "retry" },
    { from: "gate", port: "approve", to: "ship" },
    { from: "ship", port: "done", to: "ok" },
  ],
};

type Step = [node: string, port?: string, outcome?: string];

/** A run that went through `steps`: each starts its node, and finishes it through `port` when given. */
function runThrough(steps: Step[]): WorkflowRun {
  let run = startRun({
    type: "workflow_started",
    workflow_id: "implement",
    name: "Implement, test, review",
    graph,
    budget_usd: 5,
  });
  for (const [node, port, outcome] of steps) {
    run = foldNode(run, {
      type: "workflow_node",
      node_id: node,
      phase: "started",
      port: null,
      outcome: null,
    });
    if (port !== undefined) {
      run = foldNode(run, {
        type: "workflow_node",
        node_id: node,
        phase: "finished",
        port,
        outcome: outcome ?? null,
      });
    }
  }
  return run;
}

/** Tests fail twice, the loop is taken twice, then the tests pass and the run waits at review. */
const twice: Step[] = [
  ["start", "next"],
  ["implement", "done"],
  ["tests", "fail", "cargo test: 2 failed"],
  ["retry", "repeat"],
  ["fix", "done"],
  ["tests", "fail", "cargo test: 1 failed"],
  ["retry", "repeat"],
  ["fix", "done"],
  ["tests", "pass"],
  ["review"],
];

const withMax = (run: WorkflowRun, id: string, max: number): WorkflowRun => ({
  ...run,
  nodes: run.nodes.map((node) =>
    node.id === id ? { ...node, max, iteration: Math.min(node.runs, max) } : node,
  ),
});

describe("runOverlay", () => {
  it("says where each node stands: idle, active or finished, with its runs and last port", () => {
    const overlay = runOverlay(runThrough(twice));
    const of = (id: string) => overlay.byId.get(id);
    expect(of("gate")).toMatchObject({ state: "idle", runs: 0, lastPort: null, failed: false });
    expect(of("review")).toMatchObject({ state: "active", runs: 1 });
    expect(of("tests")).toMatchObject({
      state: "finished",
      runs: 3,
      lastPort: "pass",
      failed: false,
    });
    expect(of("fix")).toMatchObject({ state: "finished", runs: 2, lastPort: "done" });
    expect(overlay.active).toBe("review");
    expect(overlay.nodes.map((node) => node.id)).toEqual(graph.nodes.map((node) => node.id));
  });

  it("flags a node that last left through a failure port, and keeps its outcome", () => {
    const overlay = runOverlay(runThrough(twice.slice(0, 3)));
    expect(overlay.byId.get("tests")).toMatchObject({
      state: "finished",
      lastPort: "fail",
      failed: true,
      outcome: "cargo test: 2 failed",
    });
    expect(overlay.active).toBeUndefined();
  });

  it("counts a loop's entries, against its limit once the server has said it", () => {
    const run = runThrough(twice);
    expect(runOverlay(run).byId.get("retry")?.loop).toBe("entered 2 times");
    expect(runOverlay(withMax(run, "retry", 3)).byId.get("retry")?.loop).toBe("2 of 3");
    // Not entered yet: nothing to say.
    expect(runOverlay(runThrough([["start", "next"]])).byId.get("retry")?.loop).toBeUndefined();
    // Only loops count this way.
    expect(runOverlay(run).byId.get("tests")?.loop).toBeUndefined();
  });

  it("counts each edge once per traversal, and leaves the others at zero", () => {
    const overlay = runOverlay(runThrough(twice));
    const taken = (from: string, port: string) =>
      overlay.edges.find((edge) => edge.edge.from === from && edge.edge.port === port)?.taken;
    expect(taken("tests", "fail")).toBe(2);
    expect(taken("retry", "repeat")).toBe(2);
    expect(taken("fix", "done")).toBe(2);
    expect(taken("tests", "pass")).toBe(1);
    expect(taken("implement", "done")).toBe(1);
    expect(taken("retry", "exhausted")).toBe(0);
    expect(taken("gate", "approve")).toBe(0);
    expect(overlay.edges.find((edge) => edge.edge.port === "exhausted")?.failure).toBe(true);
    expect(overlay.edges.find((edge) => edge.edge.port === "repeat")?.failure).toBe(false);
  });

  it("finishes the active node when the run ends", () => {
    const run = finishRun(runThrough(twice), {
      type: "workflow_finished",
      status: "failed",
      reason: "stopped",
    });
    const overlay = runOverlay(run);
    expect(overlay.active).toBeUndefined();
    expect(overlay.byId.get("review")?.state).toBe("finished");
  });
});

describe("ports and words", () => {
  it("says where each port leads, or how taking it ends the run", () => {
    const run = runThrough([]);
    expect(portTargets(run, "tests").map(portSentence)).toEqual([
      "pass leads to review",
      "fail leads to retry",
    ]);
    expect(portTargets(run, "ship").map(portSentence)).toEqual([
      "done leads to ok",
      "failed ends the run as failed",
    ]);
    expect(portTargets(run, "review").map(portSentence)).toEqual([
      "approve leads to gate",
      "changes leads to retry",
      "failed ends the run as failed",
    ]);
    expect(portSentence({ port: "done", failure: false, to: undefined })).toBe(
      "done ends the run as succeeded",
    );
    expect(portTargets(run, "nowhere")).toEqual([]);
  });

  it("names a node by its id, kind and state, with its loop count", () => {
    const overlay = runOverlay(withMax(runThrough(twice), "retry", 3));
    expect(nodeName(overlay.byId.get("review") ?? overlay.nodes[0])).toBe(
      "review, Agent step: Running now",
    );
    expect(nodeName(overlay.byId.get("tests") ?? overlay.nodes[0])).toBe(
      "tests, Check: Finished through pass, 3 runs",
    );
    expect(nodeName(overlay.byId.get("retry") ?? overlay.nodes[0])).toBe(
      "retry, Loop: Finished through repeat, 2 runs, 2 of 3",
    );
    expect(stateWords(overlay.byId.get("gate") ?? overlay.nodes[0])).toBe("Not run yet");
  });

  it("says an active gate waits for you rather than runs", () => {
    const atGate = runOverlay(runThrough([...twice.slice(0, -1), ["review", "approve"], ["gate"]]));
    const gate = atGate.byId.get("gate") ?? atGate.nodes[0];
    expect(gate.state).toBe("active");
    expect(stateWords(gate)).toBe("Waiting for you");
  });

  it("tells the path taken, a node once per visit", () => {
    expect(takenPath(runThrough([]))).toEqual([]);
    expect(takenPath(runThrough([["start"]]))).toEqual(["start"]);
    expect(takenPath(runThrough(twice))).toEqual([
      "start",
      "implement",
      "tests",
      "retry",
      "fix",
      "tests",
      "retry",
      "fix",
      "tests",
      "review",
    ]);
  });

  it("writes one line per node with its kind, label, state and where its ports lead", () => {
    const run = runThrough(twice);
    const lines = nodeLines(run, runOverlay(run));
    expect(lines).toHaveLength(graph.nodes.length);
    expect(lines[2]).toBe(
      'tests: Check "cargo test". Finished through pass, 3 runs. pass leads to review; fail leads to retry.',
    );
    expect(lines[3]).toContain("Loop entered 2 times.");
    expect(lines[8]).toBe("ok: End. Not run yet.");
  });
});
