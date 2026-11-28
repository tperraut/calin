import { describe, expect, it } from "vite-plus/test";
import { startRun } from "../../lib/workflow";
import type { TranscriptEntry } from "../../state/sessionReducer";
import { makeTimeline } from "../../test/orchestration";
import { specGraph } from "../../test/workflows";
import {
  laneName,
  nodeRowKey,
  nowRunning,
  orientationFor,
  pickedArrow,
  pickedSpan,
  shownView,
  spanItem,
  viewChoices,
} from "./panelModel";
import { laneViews } from "./timelineModel";
import { ACROSS_MIN_WIDTH } from "./workflowLayout";

const lanes = () => laneViews(makeTimeline(), "s1", () => undefined);

describe("the panel's views", () => {
  it("offers the workflow only to a session that runs one", () => {
    expect(viewChoices(false).map((one) => one.label)).toEqual(["Graph", "Timeline"]);
    expect(viewChoices(true).map((one) => one.label)).toEqual(["Graph", "Timeline", "Workflow"]);
  });

  it("shows the reader's choice, else the workflow when there is one, else the graph", () => {
    expect(shownView(undefined, false)).toBe("graph");
    expect(shownView(undefined, true)).toBe("workflow");
    expect(shownView("timeline", true)).toBe("timeline");
    expect(shownView("workflow", true)).toBe("workflow");
    // A workflow chosen and then gone: back to the graph.
    expect(shownView("workflow", false)).toBe("graph");
  });

  it("draws the workflow top to bottom only on a pane known to be narrow", () => {
    expect(orientationFor(0)).toBe("across");
    expect(orientationFor(1)).toBe("down");
    expect(orientationFor(400)).toBe("down");
    expect(orientationFor(ACROSS_MIN_WIDTH - 1)).toBe("down");
    expect(orientationFor(ACROSS_MIN_WIDTH)).toBe("across");
    expect(orientationFor(900)).toBe("across");
  });
});

describe("the workflow's announcements and rows", () => {
  const started = {
    type: "workflow_started",
    workflow_id: "w",
    name: "w",
    graph: specGraph(),
    budget_usd: null,
  } as const;

  it("announces the node running now, and nothing when none is", () => {
    const run = startRun(started);
    expect(nowRunning(undefined)).toBe("");
    expect(nowRunning(run)).toBe("");
    const active = {
      ...run,
      nodes: run.nodes.map((one) =>
        one.id === "tests" ? { ...one, state: "active" as const } : one,
      ),
    };
    expect(nowRunning(active)).toBe("Now running: tests");
  });

  it("finds the newest row where a node started or finished", () => {
    const row = (key: string, note: Extract<TranscriptEntry, { kind: "workflow" }>["note"]) =>
      ({ kind: "workflow", key, ts: 1, agentId: null, note }) as const;
    const node = { nodeKind: "check", label: "tests" };
    const transcript: TranscriptEntry[] = [
      row("w1", { what: "node_started", nodeId: "tests", run: 1, ...node }),
      row("w2", {
        what: "node_finished",
        nodeId: "tests",
        port: "fail",
        outcome: null,
        failure: true,
        ...node,
      }),
      row("w3", { what: "node_started", nodeId: "fix", run: 1, ...node }),
      row("w4", { what: "finished", status: "failed", reason: null }),
      { kind: "user", key: "u1", ts: 2, text: "tests" },
    ];
    expect(nodeRowKey(transcript, "tests")).toBe("w2");
    expect(nodeRowKey(transcript, "fix")).toBe("w3");
    expect(nodeRowKey(transcript, "ship")).toBeUndefined();
  });
});

describe("what is picked in the timeline", () => {
  it("finds a picked span in its lane, and nothing once the lane or the span is gone", () => {
    expect(pickedSpan(lanes(), { kind: "span", agentId: "a1", id: "t1" })?.span.title).toBe(
      "cargo test --workspace",
    );
    expect(pickedSpan(lanes(), { kind: "span", agentId: "gone", id: "t1" })).toBeUndefined();
    expect(pickedSpan(lanes(), { kind: "span", agentId: "a1", id: "gone" })).toBeUndefined();
    expect(pickedSpan(lanes(), { kind: "arrow", id: "ar1" })).toBeUndefined();
    expect(pickedSpan(lanes(), undefined)).toBeUndefined();
  });

  it("finds a picked arrow while the timeline holds it", () => {
    expect(pickedArrow(makeTimeline(), { kind: "arrow", id: "ar3" })?.label).toBe(
      "API: add /health",
    );
    expect(pickedArrow(makeTimeline(), { kind: "arrow", id: "gone" })).toBeUndefined();
    expect(pickedArrow(undefined, { kind: "arrow", id: "ar3" })).toBeUndefined();
    expect(pickedArrow(makeTimeline(), { kind: "span", agentId: "a1", id: "t1" })).toBeUndefined();
    expect(pickedArrow(makeTimeline(), undefined)).toBeUndefined();
  });

  it("names an arrow's end: the user, a lane, or an agent without one", () => {
    expect(laneName(lanes(), null)).toBe("You");
    expect(laneName(lanes(), "a1")).toBe("lead");
    expect(laneName(lanes(), "gone")).toBe("An agent");
  });

  it("shows a span in the transcript only for a tool call of the open session that it holds", () => {
    const tool = pickedSpan(lanes(), { kind: "span", agentId: "a1", id: "t1" });
    const turn = pickedSpan(lanes(), { kind: "span", agentId: "a1", id: "turn-1" });
    const child = pickedSpan(lanes(), { kind: "span", agentId: "a2", id: "c1" });
    const item = (id: string): TranscriptEntry => ({
      kind: "item",
      key: id,
      ts: 1,
      agentId: null,
      item: { id, status: "completed", content: { kind: "text", text: "" } },
      interrupted: false,
    });
    const held = [item("t1"), item("turn-1"), item("c1")];
    expect(spanItem(tool, "s1", held)).toBe("t1");
    expect(spanItem(undefined, "s1", held)).toBeUndefined();
    expect(spanItem(turn, "s1", held)).toBeUndefined();
    expect(spanItem(child, "s1", held)).toBeUndefined();
    expect(spanItem(tool, "s2", held)).toBeUndefined();
    expect(spanItem(tool, "s1", [item("turn-1")])).toBeUndefined();
    expect(spanItem(tool, "s1", [{ kind: "user", key: "u1", ts: 1, text: "t1" }])).toBeUndefined();
  });
});
