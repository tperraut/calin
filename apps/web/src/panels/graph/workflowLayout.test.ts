import { describe, expect, it } from "vite-plus/test";
import type { WorkflowGraph } from "../../gen/WorkflowGraph";
import type { WorkflowNodeInfo } from "../../gen/WorkflowNodeInfo";
import {
  backEdges,
  layersOf,
  layoutWorkflow,
  type Orientation,
  type PlacedNode,
  type WorkflowLayout,
} from "./workflowLayout";

const PORTS: Record<string, Array<[string, boolean]>> = {
  start: [["next", false]],
  end: [],
  agent: [
    ["done", false],
    ["failed", true],
  ],
  check: [
    ["pass", false],
    ["fail", true],
  ],
  loop: [
    ["repeat", false],
    ["exhausted", true],
  ],
  gate: [
    ["approve", false],
    ["reject", true],
  ],
  ship: [
    ["done", false],
    ["failed", true],
  ],
};

const node = (id: string, kind: string, ports = PORTS[kind] ?? []): WorkflowNodeInfo => ({
  id,
  kind,
  label: id,
  ports: ports.map(([name, failure]) => ({ name, failure })),
});

/** `from.port -> to`, or `from -> to` for the only port. */
function graph(nodes: WorkflowNodeInfo[], edges: string[]): WorkflowGraph {
  return {
    nodes,
    edges: edges.map((text) => {
      const [left, to] = text.split("->").map((part) => part.trim());
      const [from, port] = left.split(".");
      const ports = nodes.find((one) => one.id === from)?.ports ?? [];
      return { from, port: port ?? ports[0]?.name ?? "next", to };
    }),
  };
}

/** The example of the workflows spec, section 2. */
function specExample(): WorkflowGraph {
  return graph(
    [
      node("start", "start"),
      node("implement", "agent"),
      node("tests", "check"),
      node("retry", "loop"),
      node("fix", "agent"),
      node("review", "agent", [
        ["approve", false],
        ["changes", false],
        ["failed", true],
      ]),
      node("gate", "gate"),
      node("ship", "ship"),
      node("ok", "end"),
      node("gave-up", "end"),
    ],
    [
      "start -> implement",
      "implement.done -> tests",
      "tests.pass -> review",
      "tests.fail -> retry",
      "retry.repeat -> fix",
      "retry.exhausted -> gave-up",
      "fix.done -> tests",
      "review.approve -> gate",
      "review.changes -> retry",
      "gate.approve -> ship",
      "ship.done -> ok",
    ],
  );
}

const overlaps = (a: PlacedNode, b: PlacedNode): boolean =>
  a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h;

function expectNoOverlap(layout: WorkflowLayout): void {
  const nodes = layout.nodes;
  for (let i = 0; i < nodes.length; i += 1) {
    for (let j = i + 1; j < nodes.length; j += 1) {
      expect(overlaps(nodes[i], nodes[j]), `${nodes[i].id} and ${nodes[j].id}`).toBe(false);
    }
  }
}

function expectContained(layout: WorkflowLayout): void {
  for (const one of layout.nodes) {
    for (const value of [one.x, one.y, one.w, one.h]) expect(Number.isFinite(value)).toBe(true);
    expect(one.x).toBeGreaterThanOrEqual(0);
    expect(one.y).toBeGreaterThanOrEqual(0);
    expect(one.x + one.w).toBeLessThanOrEqual(layout.width);
    expect(one.y + one.h).toBeLessThanOrEqual(layout.height);
  }
}

const layerMap = (layout: WorkflowLayout): Record<string, number> =>
  Object.fromEntries(layout.nodes.map((one) => [one.id, one.layer]));

const orientations: Orientation[] = ["across", "down"];

/** A 30-node workflow: a chain of checks and agents, branches to ends, and loops back. */
function thirtyNodes(): WorkflowGraph {
  const nodes: WorkflowNodeInfo[] = [node("start", "start")];
  const edges: string[] = [];
  let previous = "start";
  for (let index = 1; index <= 9; index += 1) {
    const agent = `step-${index}`;
    const check = `check-${index}`;
    const loop = `loop-${index}`;
    nodes.push(node(agent, "agent"), node(check, "check"), node(loop, "loop"));
    edges.push(
      `${previous}${previous === "start" ? "" : ".pass"} -> ${agent}`,
      `${agent}.done -> ${check}`,
      `${check}.fail -> ${loop}`,
      `${loop}.repeat -> ${agent}`,
      `${agent}.failed -> failed`,
      `${loop}.exhausted -> failed`,
    );
    previous = check;
  }
  nodes.push(node("ok", "end"), node("failed", "end"));
  edges.push(`${previous}.pass -> ok`);
  return graph(nodes, edges);
}

describe("backEdges", () => {
  it("finds the two edges of the spec's example that close a cycle in file order", () => {
    const example = specExample();
    const back = [...backEdges(example)].map((index) => {
      const edge = example.edges[index];
      return `${edge.from}.${edge.port}->${edge.to}`;
    });
    expect(back.toSorted()).toEqual(["fix.done->tests", "review.changes->retry"]);
  });

  it("finds none in a graph without cycles, even where an edge goes up the file", () => {
    const upward = graph(
      [node("start", "start"), node("b", "agent"), node("a", "agent"), node("end", "end")],
      ["start -> a", "a.done -> b", "b.done -> end"],
    );
    expect(backEdges(upward).size).toBe(0);
  });

  it("counts a node that leads to itself", () => {
    const self = graph(
      [node("start", "start"), node("a", "loop")],
      ["start -> a", "a.repeat -> a"],
    );
    expect([...backEdges(self)]).toEqual([1]);
  });
});

describe("layoutWorkflow", () => {
  it("puts a straight line in one layer per node", () => {
    const line = graph(
      [node("start", "start"), node("a", "agent"), node("b", "agent"), node("end", "end")],
      ["start -> a", "a.done -> b", "b.done -> end"],
    );
    for (const orientation of orientations) {
      const layout = layoutWorkflow(line, orientation);
      expect(layerMap(layout)).toEqual({ start: 0, a: 1, b: 2, end: 3 });
      expect(layout.layers).toEqual([["start"], ["a"], ["b"], ["end"]]);
    }
  });

  it("puts a diamond's two sides in one layer, in file order", () => {
    // `start` has one port in a file; two edges from it still make the shape asked for.
    const diamond = graph(
      [
        node("start", "start", [
          ["left", false],
          ["right", false],
        ]),
        node("a", "agent"),
        node("b", "agent"),
        node("end", "end"),
      ],
      ["start.left -> a", "start.right -> b", "a.done -> end", "b.done -> end"],
    );
    const layout = layoutWorkflow(diamond, "across");
    expect(layerMap(layout)).toEqual({ start: 0, a: 1, b: 1, end: 2 });
    expect(layout.layers).toEqual([["start"], ["a", "b"], ["end"]]);
    // In a layer, the first in the file comes first: above when across, to the left when down.
    expect(layout.byId.get("a")?.y).toBeLessThan(layout.byId.get("b")?.y ?? 0);
    const down = layoutWorkflow(diamond, "down").byId;
    expect(down.get("a")?.x).toBeLessThan(down.get("b")?.x ?? 0);
  });

  it("places a node after the longest path to it", () => {
    const skip = graph(
      [node("start", "start"), node("a", "check"), node("b", "agent"), node("end", "end")],
      ["start -> a", "a.pass -> b", "a.fail -> end", "b.done -> end"],
    );
    expect(layerMap(layoutWorkflow(skip, "across"))).toEqual({ start: 0, a: 1, b: 2, end: 3 });
  });

  it("lays out the spec's example in layers by longest path, ignoring its back edges", () => {
    const example = specExample();
    expect(Object.fromEntries(layersOf(example))).toEqual({
      start: 0,
      implement: 1,
      tests: 2,
      retry: 3,
      review: 3,
      fix: 4,
      "gave-up": 4,
      gate: 4,
      ship: 5,
      ok: 6,
    });
    const layout = layoutWorkflow(example, "across");
    // In each layer, file order.
    expect(layout.layers).toEqual([
      ["start"],
      ["implement"],
      ["tests"],
      ["retry", "review"],
      ["fix", "gate", "gave-up"],
      ["ship"],
      ["ok"],
    ]);
    expect(layout.order).toEqual(layout.layers.flat());
    expect(layout.edges.filter((edge) => edge.back).map((edge) => edge.edge.from)).toEqual([
      "fix",
      "review",
    ]);
  });

  it("is deterministic, whatever arrays it is handed", () => {
    for (const orientation of orientations) {
      const first = layoutWorkflow(specExample(), orientation);
      const second = layoutWorkflow(structuredClone(specExample()), orientation);
      expect(second).toEqual(first);
      expect(layoutWorkflow(specExample(), orientation)).toEqual(first);
    }
  });

  it("never overlaps two nodes, and keeps every node inside its size, both ways", () => {
    for (const example of [specExample(), thirtyNodes()]) {
      for (const orientation of orientations) {
        const layout = layoutWorkflow(example, orientation);
        expect(layout.nodes).toHaveLength(example.nodes.length);
        expectNoOverlap(layout);
        expectContained(layout);
      }
    }
  });

  it("lays 30 nodes out with every edge drawn and finite", () => {
    const big = thirtyNodes();
    expect(big.nodes).toHaveLength(30);
    for (const orientation of orientations) {
      const layout = layoutWorkflow(big, orientation);
      expect(layout.edges).toHaveLength(big.edges.length);
      expect(layout.edges.filter((edge) => edge.back)).toHaveLength(9);
      for (const edge of layout.edges) {
        expect(edge.d).toMatch(/^M[-\d.]+,[-\d.]+/);
        expect(edge.d).not.toContain("NaN");
        expect(Number.isFinite(edge.middle.x) && Number.isFinite(edge.middle.y)).toBe(true);
      }
    }
  });

  it("runs layers left to right across, and top to bottom down", () => {
    const across = layoutWorkflow(specExample(), "across");
    const down = layoutWorkflow(specExample(), "down");
    expect(across.orientation).toBe("across");
    expect(down.orientation).toBe("down");
    for (const [earlier, later] of [
      ["start", "implement"],
      ["implement", "tests"],
      ["tests", "review"],
      ["review", "gate"],
      ["gate", "ship"],
      ["ship", "ok"],
    ]) {
      expect(across.byId.get(earlier)?.x).toBeLessThan(across.byId.get(later)?.x ?? 0);
      expect(down.byId.get(earlier)?.y).toBeLessThan(down.byId.get(later)?.y ?? 0);
    }
    // One layer shares one coordinate along the flow.
    expect(across.byId.get("retry")?.x).toBe(across.byId.get("review")?.x);
    expect(down.byId.get("retry")?.y).toBe(down.byId.get("review")?.y);
  });

  it("runs back edges around the outside, each in a lane of its own", () => {
    for (const orientation of orientations) {
      const layout = layoutWorkflow(specExample(), orientation);
      const back = layout.edges.filter((edge) => edge.back);
      expect(back).toHaveLength(2);
      const farSide = Math.max(
        ...layout.nodes.map((one) => (orientation === "across" ? one.y + one.h : one.x + one.w)),
      );
      const lanes = back.map((edge) => (orientation === "across" ? edge.middle.y : edge.middle.x));
      for (const lane of lanes) expect(lane).toBeGreaterThan(farSide);
      expect(new Set(lanes).size).toBe(2);
    }
  });

  it("labels each edge with its port near where it leaves", () => {
    const layout = layoutWorkflow(specExample(), "across");
    const fail = layout.edges.find(
      (edge) => edge.edge.from === "tests" && edge.edge.port === "fail",
    );
    const tests = layout.byId.get("tests");
    expect(fail && tests).toBeTruthy();
    if (!fail || !tests) return;
    expect(Math.abs(fail.label.x - (tests.x + tests.w))).toBeLessThan(12);
    // Two ports of one node leave from two places.
    const pass = layout.edges.find(
      (edge) => edge.edge.from === "tests" && edge.edge.port === "pass",
    );
    expect(pass?.label.y).not.toBe(fail.label.y);
  });

  it("skips edges that name a node the graph does not have", () => {
    const broken = graph(
      [node("start", "start"), node("a", "agent")],
      ["start -> a", "a.done -> nowhere", "ghost -> a"],
    );
    const layout = layoutWorkflow(broken, "across");
    expect(layout.edges.map((edge) => edge.index)).toEqual([0]);
    expect(layerMap(layout)).toEqual({ start: 0, a: 1 });
  });

  it("lays out an empty graph as nothing", () => {
    const layout = layoutWorkflow({ nodes: [], edges: [] }, "down");
    expect(layout.nodes).toEqual([]);
    expect(layout.layers).toEqual([]);
    expect(Number.isFinite(layout.width) && Number.isFinite(layout.height)).toBe(true);
  });
});
