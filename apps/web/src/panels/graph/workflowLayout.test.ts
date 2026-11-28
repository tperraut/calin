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

const sizeOf = (example: WorkflowGraph, orientation: Orientation): number[] => {
  const { width, height } = layoutWorkflow(example, orientation);
  return [width, height];
};

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
    // Each node still goes after the longest path to it, wherever the file declares it.
    expect(Object.fromEntries(layersOf(upward))).toEqual({ start: 0, a: 1, b: 2, end: 3 });
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

  it("lays out an empty graph as nothing but its padding", () => {
    const layout = layoutWorkflow({ nodes: [], edges: [] }, "down");
    expect(layout.nodes).toEqual([]);
    expect(layout.layers).toEqual([]);
    expect([layout.width, layout.height]).toEqual([32, 32]);
  });

  it("lays a lone node out in one layer", () => {
    expect(layoutWorkflow(graph([node("start", "start")], []), "across").layers).toEqual([
      ["start"],
    ]);
  });

  it("centres a narrow layer on the widest one", () => {
    const diamond = graph(
      [
        node("start", "start", [
          ["left", false],
          ["right", false],
        ]),
        node("a", "agent"),
        node("b", "agent"),
      ],
      ["start.left -> a", "start.right -> b"],
    );
    for (const orientation of orientations) {
      const placed = layoutWorkflow(diamond, orientation).byId;
      const side = (id: string) => {
        const one = placed.get(id);
        if (!one) return Number.NaN;
        return orientation === "across" ? one.y + one.h / 2 : one.x + one.w / 2;
      };
      expect(side("start")).toBe((side("a") + side("b")) / 2);
    }
  });

  it("sizes the drawing for its layers, its widest layer and the lanes of its back edges", () => {
    // Seven layers, three nodes in the widest, two back edges.
    expect(sizeOf(specExample(), "across")).toEqual([1784, 324]);
    expect(sizeOf(specExample(), "down")).toEqual([632, 836]);
  });

  /**
   * One node with three ports, given out of port order: `pass` to the end,
   * `fail` back to the start, and `odd`, a port the node does not list.
   */
  const ported = () =>
    graph(
      [node("start", "start"), node("a", "check"), node("end", "end")],
      ["start -> a", "a.odd -> end", "a.fail -> start", "a.pass -> end"],
    );

  const drawn = (layout: WorkflowLayout) => ({
    nodes: layout.nodes.map(({ id, x, y }) => [id, x, y]),
    edges: layout.edges.map(({ index, back, d, label, middle }) => ({
      index,
      back,
      d,
      label: [label.x, label.y],
      middle: [middle.x, middle.y],
    })),
    size: [layout.width, layout.height],
  });

  it("draws each edge from its port's place, in port order, to the pixel, across", () => {
    expect(drawn(layoutWorkflow(ported(), "across"))).toEqual({
      nodes: [
        ["start", 16, 16],
        ["a", 280, 16],
        ["end", 544, 16],
      ],
      edges: [
        {
          index: 0,
          back: false,
          d: "M184,46C232,46,232,46,280,46",
          label: [190, 41],
          middle: [232, 46],
        },
        // The unlisted port leaves last, at three quarters of the side.
        {
          index: 1,
          back: false,
          d: "M448,61C496,61,496,46,544,46",
          label: [454, 56],
          middle: [496, 53.5],
        },
        // Out past the node, down into its lane, back along it and in through the gap before the start.
        {
          index: 2,
          back: true,
          d: "M448,46L453,46Q458,46,458,51L458,92Q458,100,450,100L14,100Q6,100,6,92L6,66Q6,61,11,61L16,61",
          label: [454, 41],
          middle: [232, 100],
        },
        {
          index: 3,
          back: false,
          d: "M448,31C496,31,496,46,544,46",
          label: [454, 26],
          middle: [496, 38.5],
        },
      ],
      size: [728, 124],
    });
  });

  it("draws each edge from its port's place, in port order, to the pixel, down", () => {
    expect(drawn(layoutWorkflow(ported(), "down"))).toEqual({
      nodes: [
        ["start", 16, 16],
        ["a", 16, 140],
        ["end", 16, 264],
      ],
      edges: [
        {
          index: 0,
          back: false,
          d: "M100,76C100,108,100,108,100,140",
          label: [105, 90],
          middle: [100, 108],
        },
        {
          index: 1,
          back: false,
          d: "M142,200C142,232,100,232,100,264",
          label: [147, 214],
          middle: [121, 232],
        },
        {
          index: 2,
          back: true,
          d: "M100,200L100,205Q100,210,105,210L200,210Q208,210,208,202L208,14Q208,6,200,6L147,6Q142,6,142,11L142,16",
          label: [105, 214],
          middle: [208, 108],
        },
        {
          index: 3,
          back: false,
          d: "M58,200C58,232,100,232,100,264",
          label: [63, 214],
          middle: [79, 232],
        },
      ],
      size: [232, 340],
    });
  });

  it("turns each back edge into its lane further out, but never past the middle of the gap", () => {
    for (const [orientation, insets] of [
      ["across", [10, 16, 22, 28, 34, 40, 44, 44, 44]],
      ["down", [10, 16, 22, 28, 28, 28, 28, 28, 28]],
    ] as const) {
      const layout = layoutWorkflow(thirtyNodes(), orientation);
      const turns = layout.edges
        .filter((edge) => edge.back)
        .map((edge) => {
          const from = layout.byId.get(edge.edge.from);
          // The first corner of the path is where it turns into its lane.
          const corner = /Q([-\d.]+),([-\d.]+)/.exec(edge.d);
          if (!from || !corner) return Number.NaN;
          return orientation === "across"
            ? Number(corner[1]) - (from.x + from.w)
            : Number(corner[2]) - (from.y + from.h);
        });
      expect(turns).toEqual(insets);
      // Each lane runs 16 px further out than the one before.
      const lanes = layout.edges
        .filter((edge) => edge.back)
        .map((edge) => (orientation === "across" ? edge.middle.y : edge.middle.x));
      expect(lanes.map((lane) => lane - lanes[0])).toEqual(insets.map((_, lane) => lane * 16));
    }
  });
});
