import { describe, expect, it } from "vite-plus/test";
import type { Graph } from "../../gen/Graph";
import type { GraphNode } from "../../gen/GraphNode";
import {
  agentNode,
  makeAgent,
  makeEdge,
  makeGraph,
  makeSubagent,
  repoNode,
  userNode,
} from "../../test/orchestration";
import { layoutGraph, sizeOf, type Layout, type LayoutMode, type Point } from "./layout";
import { edgeEmphasis, graphModel } from "./model";
import {
  resolvePoints,
  routeOverlay,
  type LaneMemory,
  type OverlayEdge,
  type RouteBox,
  type Routing,
} from "./routing";
import { graphAt, synthesize } from "./stress/synthetic";

const modes: readonly LayoutMode[] = ["indented", "columns"];

type Routed = {
  layout: Layout;
  boxes: Map<string, RouteBox>;
  edges: OverlayEdge[];
  routing: Routing;
};

function route(
  graph: Graph,
  mode: LayoutMode,
  previous?: { layout: Layout; routing: Routing },
): Routed {
  const model = graphModel(graph, graph.root_session_id);
  const layout = layoutGraph(graph.nodes, previous?.layout.positions, mode);
  const boxes = new Map<string, RouteBox>();
  for (const node of model.nodes) {
    const at = layout.positions.get(node.id);
    if (at) boxes.set(node.id, { ...at, ...sizeOf(node.node), kind: node.node.kind });
  }
  const edges = model.edges.filter((edge) => edge.route === "curve");
  return {
    layout,
    boxes,
    edges,
    routing: routeOverlay(mode, boxes, edges, previous?.routing.memory),
  };
}

const pathOf = (routed: Routed, id: string): Point[] => {
  const found = routed.routing.routes.get(id);
  if (!found) throw new Error(`no route for ${id}`);
  return resolvePoints(found.points, (node) => routed.boxes.get(node));
};

/** True when the segment from `a` to `b` enters the inside of the box, shrunk by `inset`. */
function enters(a: Point, b: Point, box: RouteBox, inset: number): boolean {
  const x0 = box.x + inset;
  const x1 = box.x + box.w - inset;
  const y0 = box.y + inset;
  const y1 = box.y + box.h - inset;
  // Liang–Barsky: clip the segment to the box, strictly inside.
  let t0 = 0;
  let t1 = 1;
  const dx = b.x - a.x;
  const dy = b.y - a.y;
  const edges: [number, number][] = [
    [-dx, a.x - x0],
    [dx, x1 - a.x],
    [-dy, a.y - y0],
    [dy, y1 - a.y],
  ];
  for (const [p, q] of edges) {
    if (p === 0) {
      if (q <= 0) return false;
      continue;
    }
    const r = q / p;
    if (p < 0) t0 = Math.max(t0, r);
    else t1 = Math.min(t1, r);
    if (t0 >= t1) return false;
  }
  return t1 - t0 > 1e-6;
}

/** Every place where an overlay path runs through a card. */
function throughCards(routed: Routed): string[] {
  const found: string[] = [];
  for (const edge of routed.edges) {
    const points = pathOf(routed, edge.id);
    for (let index = 1; index < points.length; index += 1) {
      const a = points[index - 1];
      const b = points[index];
      for (const [id, box] of routed.boxes) {
        const own = id === edge.from || id === edge.to;
        // A path ends on its own cards; only its first and last runs may touch them.
        if (own && (index === 1 || index === points.length - 1)) continue;
        if (enters(a, b, box, own ? 0.5 : -0.5)) found.push(`${edge.id} #${index} through ${id}`);
      }
    }
  }
  return found;
}

/** Places where the paths of different bundles cross: lines of one bundle crossed at one place count once. */
function crossingCount(routed: Routed): number {
  const segments = routed.edges.flatMap((edge) => {
    const points = pathOf(routed, edge.id);
    const bundle = routed.routing.routes.get(edge.id)?.bundle;
    return points.slice(1).map((b, index) => ({ a: points[index], b, bundle }));
  });
  const cross = (o: Point, p: Point, q: Point): number =>
    (p.x - o.x) * (q.y - o.y) - (p.y - o.y) * (q.x - o.x);
  const places = new Set<string>();
  for (let i = 0; i < segments.length; i += 1) {
    for (let j = i + 1; j < segments.length; j += 1) {
      const s = segments[i];
      const t = segments[j];
      if (s.bundle === t.bundle) continue;
      const d1 = cross(s.a, s.b, t.a);
      const d2 = cross(s.a, s.b, t.b);
      const d3 = cross(t.a, t.b, s.a);
      const d4 = cross(t.a, t.b, s.b);
      if (d1 * d2 < 0 && d3 * d4 < 0) {
        const along = d1 / (d1 - d2);
        const x = t.a.x + (t.b.x - t.a.x) * along;
        const y = t.a.y + (t.b.y - t.a.y) * along;
        // Within a bundle the lines run a few pixels apart: one crossing for them all.
        const pair = [s.bundle ?? "", t.bundle ?? ""]
          .toSorted((a, b) => a.localeCompare(b))
          .join(" x ");
        places.add(`${pair}@${Math.round(x / 8)},${Math.round(y / 8)}`);
      }
    }
  }
  return places.size;
}

const sub = (id: string, parent: string, session = "s1"): GraphNode =>
  agentNode(
    makeSubagent(id, "explorer", {
      session_id: session,
      parent_agent_id: parent.replace("agent:", ""),
    }),
    parent,
  );

/** A lead with `count` subagents, two repos, and the given edges. */
function fan(count: number, edges: Graph["edges"]): Graph {
  return makeGraph({
    nodes: [
      userNode(),
      agentNode(makeAgent("a1"), "user"),
      ...Array.from({ length: count }, (_, index) => sub(`w${index}`, "agent:a1")),
      repoNode("api"),
      repoNode("web"),
    ],
    edges,
  });
}

/** A chain: user, lead, a dispatched child, its subagent, and one more level down. */
function deep(): Graph {
  const child = makeAgent("c1", { session_id: "s2" });
  return makeGraph({
    nodes: [
      userNode(),
      agentNode(makeAgent("a1"), "user"),
      agentNode(child, "agent:a1", { session_title: "Child" }),
      sub("k1", "agent:c1", "s2"),
      sub("k2", "agent:c1", "s2"),
      sub("x1", "agent:a1"),
      repoNode("api"),
      repoNode("web"),
      repoNode("docs"),
      repoNode("site"),
    ],
    edges: [
      makeEdge("message", "agent:k1", "agent:a1"),
      makeEdge("message", "agent:a1", "agent:k2"),
      makeEdge("report", "agent:c1", "agent:a1"),
      makeEdge("message", "agent:x1", "agent:c1"),
      makeEdge("message", "user", "agent:a1"),
      makeEdge("changed", "agent:k1", "repo:site"),
      makeEdge("changed", "agent:k2", "repo:docs"),
      makeEdge("changed", "agent:a1", "repo:api"),
      makeEdge("changed", "agent:x1", "repo:web"),
      makeEdge("changed", "agent:c1", "repo:site"),
    ],
  });
}

/** The mocked busy session of the browser check: notes both ways, two reports, several changes. */
function busy(): Graph {
  const graph = makeGraph();
  return {
    ...graph,
    edges: [
      ...graph.edges,
      makeEdge("message", "user", "agent:a1"),
      makeEdge("message", "agent:a1", "user"),
      makeEdge("message", "agent:a3", "agent:a1"),
      makeEdge("message", "agent:a1", "agent:a3"),
      makeEdge("report", "agent:a3", "agent:a1"),
      makeEdge("report", "agent:sub1", "agent:a1"),
      makeEdge("message", "agent:sub3", "agent:a2"),
      makeEdge("changed", "agent:a3", "repo:web", { count: 3 }),
      makeEdge("changed", "agent:sub2", "repo:docs"),
      makeEdge("changed", "agent:a2", "repo:web"),
    ],
  };
}

const stress = (): Graph => graphAt(synthesize(200, 300), 200);

const shapes: [string, () => Graph][] = [
  ["the sample session", () => makeGraph()],
  ["the busy session", busy],
  [
    "a fan of twelve",
    () =>
      fan(12, [
        makeEdge("message", "agent:w0", "agent:w11"),
        makeEdge("message", "agent:w3", "agent:w5"),
        makeEdge("report", "agent:w7", "agent:a1"),
        makeEdge("changed", "agent:w2", "repo:web"),
        makeEdge("changed", "agent:w9", "repo:api"),
        makeEdge("changed", "agent:a1", "repo:api"),
      ]),
  ],
  ["a deep chain", deep],
  ["the 200-node stress graph", stress],
];

describe("routeOverlay", () => {
  for (const mode of modes) {
    for (const [name, make] of shapes) {
      it(`never runs through a card: ${name}, ${mode}`, () => {
        const routed = route(make(), mode);
        expect(routed.routing.routes.size).toBe(routed.edges.length);
        expect(throughCards(routed)).toEqual([]);
      });
    }
  }

  it("is deterministic, whatever the order the edges come in", () => {
    for (const mode of modes) {
      const graph = stress();
      const once = route(graph, mode).routing;
      const again = route(graph, mode).routing;
      const shuffled = route({ ...graph, edges: graph.edges.toReversed() }, mode).routing;
      expect(again).toEqual(once);
      expect(shuffled.routes).toEqual(once.routes);
    }
  });

  it("bundles the edges between two agents: one lane, a line each, an arrowhead each", () => {
    for (const mode of modes) {
      const routed = route(busy(), mode);
      const ids = [
        "message:agent:a3:agent:a1",
        "message:agent:a1:agent:a3",
        "report:agent:a3:agent:a1",
      ];
      const routes = ids.map((id) => routed.routing.routes.get(id));
      expect(new Set(routes.map((one) => one?.bundle)).size).toBe(1);
      expect(new Set(routes.map((one) => `${one?.channel}#${one?.lane}`)).size).toBe(1);
      const paths = ids.map((id) => pathOf(routed, id));
      // Side by side along the lane, close enough to read as one.
      const laneXs = paths.map((points) => points[1].x);
      expect(new Set(laneXs).size).toBe(3);
      expect(Math.max(...laneXs) - Math.min(...laneXs)).toBeLessThanOrEqual(6);
      // Each ends somewhere of its own: three arrowheads, not one.
      const ends = paths.map((points) => `${points.at(-1)?.x},${points.at(-1)?.y}`);
      expect(new Set(ends).size).toBe(3);
      // The two notes run opposite ways along the same lane.
      expect(paths[0][0]).toEqual(paths[1].at(-1) === undefined ? undefined : paths[0][0]);
      expect(pathOf(routed, ids[0]).at(-1)?.y).not.toBe(pathOf(routed, ids[1]).at(-1)?.y);
    }
  });

  it("brings every change into a repo along one trunk, into one point of the repo", () => {
    for (const mode of modes) {
      const routed = route(busy(), mode);
      const into = [
        "changed:agent:sub3:repo:web",
        "changed:agent:a3:repo:web",
        "changed:agent:a2:repo:web",
      ];
      const paths = into.map((id) => pathOf(routed, id));
      const last = paths.map((points) => JSON.stringify(points.slice(-2)));
      expect(new Set(last).size).toBe(1);
      const routes = into.map((id) => routed.routing.routes.get(id));
      expect(new Set(routes.map((one) => `${one?.channel}#${one?.lane}`)).size).toBe(1);
      // From one side: from above in the indented band, from the left in the column.
      const [before, end] = paths[0].slice(-2);
      const web = routed.boxes.get("repo:web") as RouteBox;
      if (mode === "indented") {
        expect(before.x).toBe(end.x);
        expect(before.y).toBeLessThan(web.y);
      } else {
        expect(before.y).toBe(end.y);
        expect(end.x).toBe(web.x);
      }
    }
  });

  it("keeps an edge's lane while nodes and edges come, and packs them only when asked", () => {
    for (const mode of modes) {
      const first = busy();
      const before = route(first, mode);
      const grown: Graph = {
        ...first,
        // A subagent arrives in the middle of the tree, with notes of its own.
        nodes: [...first.nodes.slice(0, 4), sub("sub9", "agent:a1"), ...first.nodes.slice(4)],
        edges: [
          ...first.edges,
          makeEdge("message", "agent:sub9", "agent:a3"),
          makeEdge("message", "agent:sub2", "agent:sub3"),
          makeEdge("changed", "agent:sub9", "repo:api"),
        ],
      };
      const after = route(grown, mode, before);
      for (const edge of before.edges) {
        const was = before.routing.routes.get(edge.id);
        const now = after.routing.routes.get(edge.id);
        expect(`${now?.channel}#${now?.lane}`, edge.id).toBe(`${was?.channel}#${was?.lane}`);
      }
      expect(throughCards(after)).toEqual([]);
      // Grown one step at a time, the stress graph keeps every lane as well.
      const steps = synthesize(200, 300);
      let last = route(graphAt(steps, 150), mode);
      for (let count = 151; count <= 160; count += 1) {
        const next = route(graphAt(steps, count), mode, last);
        for (const edge of last.edges) {
          const was = last.routing.routes.get(edge.id);
          const now = next.routing.routes.get(edge.id);
          expect(`${now?.channel}#${now?.lane}`, edge.id).toBe(`${was?.channel}#${was?.lane}`);
        }
        last = next;
      }
      // Without the memory, the same graph packs at least as tight.
      const packed = route(graphAt(steps, 160), mode);
      expect(packed.routing.right).toBeLessThanOrEqual(last.routing.right);
    }
  });

  describe("crossings, where the fewest possible is known", () => {
    const fanEdges = (pairs: [number, number][]): Graph["edges"] =>
      pairs.map(([a, b]) => makeEdge("message", `agent:w${a}`, `agent:w${b}`));

    it("nests edges that nest: none cross", () => {
      for (const mode of modes) {
        const routed = route(
          fan(
            6,
            fanEdges([
              [0, 5],
              [1, 4],
              [2, 3],
            ]),
          ),
          mode,
        );
        expect(crossingCount(routed)).toBe(0);
      }
    });

    it("shares a lane between edges that follow each other, crossing nothing", () => {
      for (const mode of modes) {
        const routed = route(
          fan(
            6,
            fanEdges([
              [0, 1],
              [2, 3],
              [4, 5],
            ]),
          ),
          mode,
        );
        expect(crossingCount(routed)).toBe(0);
        const lanes = routed.edges.map((edge) => routed.routing.routes.get(edge.id)?.lane);
        expect(new Set(lanes).size).toBe(1);
      }
    });

    it("crosses once where two edges interleave, and no more", () => {
      for (const mode of modes) {
        const routed = route(
          fan(
            4,
            fanEdges([
              [0, 2],
              [1, 3],
            ]),
          ),
          mode,
        );
        expect(crossingCount(routed)).toBeLessThanOrEqual(1);
      }
    });

    it("fans out from the lead to its subagents without a crossing", () => {
      for (const mode of modes) {
        const edges = [0, 1, 2, 3].map((index) =>
          makeEdge("message", "agent:a1", `agent:w${index}`),
        );
        expect(crossingCount(route(fan(4, edges), mode))).toBe(0);
      }
    });

    it("brings changes from a column of agents into two repos side by side", () => {
      const into = (upper: string, lower: string) => [
        makeEdge("changed", "agent:w0", upper),
        makeEdge("changed", "agent:w1", upper),
        makeEdge("changed", "agent:w2", lower),
        makeEdge("changed", "agent:w3", lower),
      ];
      // The lower agents into the left repo: nothing need cross.
      expect(crossingCount(route(fan(4, into("repo:web", "repo:api")), "indented"))).toBe(0);
      // The other way round one track has to cross one trunk.
      expect(crossingCount(route(fan(4, into("repo:api", "repo:web")), "indented"))).toBe(1);
      for (const mode of modes) {
        expect(
          crossingCount(route(fan(4, into("repo:api", "repo:web")), mode)),
        ).toBeLessThanOrEqual(1);
      }
    });
  });

  it("keeps the indented gutter of the busy session narrow enough for a phone", () => {
    const routed = route(busy(), "indented");
    // Seven lanes: four bundles between agents, three trunks into repos.
    expect(routed.routing.right - routed.layout.width).toBeLessThanOrEqual(64);
  });

  it("follows its cards: corners tied to a node move with it", () => {
    const routed = route(busy(), "indented");
    const found = routed.routing.routes.get("message:agent:a3:agent:a1");
    const moved = new Map(routed.boxes);
    const a1 = routed.boxes.get("agent:a1") as RouteBox;
    moved.set("agent:a1", { ...a1, y: a1.y + 10 });
    const before = resolvePoints(found?.points ?? [], (id) => routed.boxes.get(id));
    const after = resolvePoints(found?.points ?? [], (id) => moved.get(id));
    expect(after.at(-1)?.y).toBe((before.at(-1)?.y ?? 0) + 10);
    expect(after[0]).toEqual(before[0]);
  });

  it("routes nothing for an edge whose ends are not both drawn", () => {
    const boxes = new Map<string, RouteBox>([["a", { x: 0, y: 0, w: 176, h: 52, kind: "agent" }]]);
    const memory: LaneMemory = new Map();
    const result = routeOverlay(
      "indented",
      boxes,
      [{ id: "e", from: "a", to: "b", kind: "message" }],
      memory,
    );
    expect(result.routes.size).toBe(0);
  });

  it("loops an edge from a card back to itself on the card's right side", () => {
    for (const mode of modes) {
      const routed = route(fan(2, [makeEdge("message", "agent:w0", "agent:w0")]), mode);
      const w0 = routed.boxes.get("agent:w0") as RouteBox;
      const right = w0.x + w0.w;
      const middle = w0.y + w0.h / 2;
      expect(pathOf(routed, "message:agent:w0:agent:w0")).toEqual([
        { x: right, y: middle - 6 },
        { x: right + 14, y: middle - 6 },
        { x: right + 14, y: middle + 6 },
        { x: right, y: middle + 6 },
      ]);
      expect(routed.routing.routes.get("message:agent:w0:agent:w0")?.label).toEqual({
        fraction: 0.5,
      });
    }
  });

  it("draws an edge between two repos straight, from centre to centre", () => {
    for (const mode of modes) {
      const routed = route(fan(1, [makeEdge("changed", "repo:api", "repo:web")]), mode);
      const centre = (id: string): Point => {
        const box = routed.boxes.get(id) as RouteBox;
        return { x: box.x + box.w / 2, y: box.y + box.h / 2 };
      };
      expect(pathOf(routed, "changed:repo:api:repo:web")).toEqual([
        centre("repo:api"),
        centre("repo:web"),
      ]);
      expect(routed.routing.routes.get("changed:repo:api:repo:web")?.channel).toBe("");
    }
  });

  it("runs an edge out of a repo back along the repo's trunk, its count near the agent", () => {
    for (const mode of modes) {
      const routed = route(
        fan(2, [
          makeEdge("changed", "agent:w1", "repo:web"),
          makeEdge("changed", "repo:web", "agent:w0"),
        ]),
        mode,
      );
      const into = routed.routing.routes.get("changed:agent:w1:repo:web");
      const out = routed.routing.routes.get("changed:repo:web:agent:w0");
      expect(`${out?.channel}#${out?.lane}`).toBe(`${into?.channel}#${into?.lane}`);
      // It leaves where the other comes in, and ends on the agent's right side.
      expect(pathOf(routed, "changed:repo:web:agent:w0")[0]).toEqual(
        pathOf(routed, "changed:agent:w1:repo:web").at(-1),
      );
      const w0 = routed.boxes.get("agent:w0") as RouteBox;
      expect(pathOf(routed, "changed:repo:web:agent:w0").at(-1)?.x).toBe(w0.x + w0.w);
      expect(out?.label).toEqual({ distance: -22 });
      expect(into?.label).toEqual({ distance: 22 });
    }
  });

  it("draws straight an edge from an agent that lies right of its repo's column", () => {
    const boxes = new Map<string, RouteBox>([
      ["repo:web", { x: 0, y: 0, w: 132, h: 44, kind: "repo" }],
      ["agent:a1", { x: 440, y: 0, w: 176, h: 52, kind: "agent" }],
    ]);
    const edge: OverlayEdge = { id: "e", from: "agent:a1", to: "repo:web", kind: "changed" };
    const found = routeOverlay("columns", boxes, [edge]).routes.get("e");
    expect(found?.channel).toBe("");
    expect(resolvePoints(found?.points ?? [], (id) => boxes.get(id))).toEqual([
      { x: 528, y: 26 },
      { x: 66, y: 22 },
    ]);
  });

  it("brings a change into a repo with nothing above it from two rows above", () => {
    const boxes = new Map<string, RouteBox>([
      ["repo:web", { x: 0, y: 0, w: 132, h: 44, kind: "repo" }],
      ["agent:a1", { x: 0, y: 100, w: 176, h: 52, kind: "agent" }],
    ]);
    const edge: OverlayEdge = { id: "e", from: "agent:a1", to: "repo:web", kind: "changed" };
    const found = routeOverlay("indented", boxes, [edge]).routes.get("e");
    const points = resolvePoints(found?.points ?? [], (id) => boxes.get(id));
    // Halfway up the two empty rows over the repo, then down into its top.
    expect(points.slice(2)).toEqual([
      { x: 190, y: -64 },
      { x: 66, y: -64 },
      { x: 66, y: 10 },
    ]);
  });
});

describe("routeOverlay, corner by corner", () => {
  const agent = (x: number, y: number): RouteBox => ({ x, y, w: 176, h: 52, kind: "agent" });
  const repo = (x: number, y: number): RouteBox => ({ x, y, w: 132, h: 44, kind: "repo" });

  /** Routes `edges` (kind, from, to) between `cards`; each edge's id is `kind:from:to`. */
  function routeCards(
    mode: LayoutMode,
    cards: Record<string, RouteBox>,
    edges: [OverlayEdge["kind"], string, string][],
    previous?: LaneMemory,
  ) {
    const boxes = new Map(Object.entries(cards));
    const list = edges.map(([kind, from, to]) => ({ id: `${kind}:${from}:${to}`, from, to, kind }));
    const routing = routeOverlay(mode, boxes, list, previous);
    const at = (id: string) => {
      const found = routing.routes.get(id);
      if (!found) throw new Error(`no route for ${id}`);
      const points = resolvePoints(found.points, (node) => boxes.get(node));
      return {
        where: `${found.channel}#${found.lane} ${found.bundle}`,
        label: found.label,
        points: points.map((point) => [point.x, point.y]),
      };
    };
    return { routing, at };
  }

  it("runs a bundle along the gutter, a line each, its ports spread around the card's middle", () => {
    const { routing, at } = routeCards("indented", { a: agent(0, 0), b: agent(28, 64) }, [
      ["report", "b", "a"],
      ["message", "a", "b"],
    ]);
    // The gutter's first lane is 14 px right of the widest card (204), the lines 2 px apart.
    expect(at("message:a:b")).toEqual({
      where: "gutter#0 a|b",
      label: { fraction: 0.42 },
      points: [
        [176, 29.5],
        [217, 29.5],
        [217, 86.5],
        [204, 86.5],
      ],
    });
    expect(at("report:b:a")).toEqual({
      where: "gutter#0 a|b",
      label: { fraction: 0.58 },
      points: [
        [204, 93.5],
        [219, 93.5],
        [219, 22.5],
        [176, 22.5],
      ],
    });
    expect(routing.right).toBe(222);
    expect([...routing.memory]).toEqual([
      ["order|gutter", 1],
      ["gutter|a|b", 0],
    ]);
  });

  it("runs an edge within one column along the corridor to its right, out of and into right sides", () => {
    const { routing, at } = routeCards("columns", { a: agent(0, 0), b: agent(0, 128) }, [
      ["message", "b", "a"],
    ]);
    expect(at("message:b:a")).toEqual({
      where: "c0#0 a|b",
      label: { fraction: 0.5 },
      points: [
        [176, 154],
        [184, 154],
        [184, 26],
        [176, 26],
      ],
    });
    expect(routing.right).toBe(188);
  });

  it("runs an edge between neighbouring columns along the corridor between them, into the left side", () => {
    const { routing, at } = routeCards("columns", { a: agent(0, 0), b: agent(220, 64) }, [
      ["message", "b", "a"],
    ]);
    expect(at("message:b:a").points).toEqual([
      [220, 90],
      [184, 90],
      [184, 26],
      [176, 26],
    ]);
    expect(routing.right).toBe(396);
  });

  it("runs an edge level across an empty column to the corridor before the far card", () => {
    const { at } = routeCards("columns", { a: agent(0, 0), b: agent(440, 128) }, [
      ["message", "a", "b"],
    ]);
    expect(at("message:a:b")).toMatchObject({
      where: "c1#0 a|b",
      points: [
        [176, 26],
        [404, 26],
        [404, 154],
        [440, 154],
      ],
    });
  });

  it("crosses a column a card blocks along the gap between two rows", () => {
    const { routing, at } = routeCards(
      "columns",
      { a: agent(0, 0), c: agent(220, 0), b: agent(440, 128) },
      [
        ["message", "a", "b"],
        ["report", "b", "a"],
      ],
    );
    // Down the corridor right of a, along the gap under its row (y 58), down the corridor left of b.
    expect(at("message:a:b")).toEqual({
      where: "c1#0 a|b",
      label: { fraction: 0.42 },
      points: [
        [176, 29.5],
        [183, 29.5],
        [183, 57.4],
        [403, 57.4],
        [403, 157.5],
        [440, 157.5],
      ],
    });
    expect(at("report:b:a").points).toEqual([
      [440, 150.5],
      [405, 150.5],
      [405, 58.6],
      [185, 58.6],
      [185, 22.5],
      [176, 22.5],
    ]);
    expect(routing.right).toBe(616);
    expect([...routing.memory]).toEqual([
      ["order|c0", 1],
      ["c0|a|b@1", 0],
      ["order|c1", 1],
      ["c1|a|b@2", 0],
      ["track|a|b", 0],
    ]);
  });

  it("brings changes into the band's repos along a trunk each, over the row, the leftmost highest", () => {
    const { routing, at } = routeCards(
      "indented",
      { a: agent(0, 0), b: agent(28, 64), r1: repo(0, 200), r2: repo(144, 200) },
      [
        ["changed", "a", "r2"],
        ["changed", "a", "r1"],
        ["changed", "b", "r1"],
      ],
    );
    // The tracks split the 84 px between the tree (bottom 116) and the row in thirds.
    expect(at("changed:a:r1")).toEqual({
      where: "gutter#0 >r1",
      label: { distance: 22 },
      points: [
        [176, 29.5],
        [290, 29.5],
        [290, 144],
        [66, 144],
        [66, 210],
      ],
    });
    expect(at("changed:b:r1").points).toEqual([
      [204, 90],
      [290, 90],
      [290, 144],
      [66, 144],
      [66, 210],
    ]);
    // A second line out of the same card has its count further along.
    expect(at("changed:a:r2")).toEqual({
      where: "gutter#1 >r2",
      label: { distance: 46 },
      points: [
        [176, 22.5],
        [297, 22.5],
        [297, 172],
        [210, 172],
        [210, 210],
      ],
    });
    expect(routing.right).toBe(301);
  });

  it("brings changes into a column's repo along the corridor left of it, around a card in the way", () => {
    const { routing, at } = routeCards(
      "columns",
      { a: agent(0, 0), c: agent(220, 0), d: agent(220, 64), r: repo(440, 0) },
      [
        ["changed", "a", "r"],
        ["changed", "d", "r"],
        ["changed", "r", "c"],
      ],
    );
    expect(at("changed:a:r")).toEqual({
      where: "c1#0 >r",
      label: { distance: 22 },
      points: [
        [176, 26],
        [184, 26],
        [184, 58],
        [404, 58],
        [404, 27],
        [440, 27],
      ],
    });
    expect(at("changed:d:r").points).toEqual([
      [396, 90],
      [404, 90],
      [404, 27],
      [440, 27],
    ]);
    expect(at("changed:r:c")).toEqual({
      where: "c1#0 >r",
      label: { distance: -22 },
      points: [
        [440, 27],
        [404, 27],
        [404, 26],
        [396, 26],
      ],
    });
    expect(routing.right).toBe(572);
    expect(routing.memory.get("track|>r@a")).toBe(0);
  });

  it("enters a column's repo level with its middle, 27 px under its top", () => {
    const { at } = routeCards("columns", { d: agent(220, 64), r: repo(440, 64) }, [
      ["changed", "d", "r"],
    ]);
    expect(at("changed:d:r").points).toEqual([
      [396, 90],
      [404, 90],
      [404, 91],
      [440, 91],
    ]);
  });

  it("counts a corridor's lanes from the right when that crosses fewer lines", () => {
    const { routing, at } = routeCards(
      "columns",
      {
        a: agent(0, 0),
        d: agent(220, 64),
        b: agent(440, 128),
        r1: repo(440, 0),
        r2: repo(440, 200),
      },
      [
        ["message", "a", "b"],
        ["changed", "d", "r1"],
        ["changed", "a", "r2"],
      ],
    );
    // Counted from the left, a's line into r2 would cross the two lanes before it.
    expect(routing.memory.get("order|c1")).toBe(-1);
    expect(at("message:a:b")).toMatchObject({
      where: "c1#0 a|b",
      points: [
        [176, 22.5],
        [416, 22.5],
        [416, 154],
        [440, 154],
      ],
    });
    expect(at("changed:d:r1")).toMatchObject({
      where: "c1#1 >r1",
      points: [
        [396, 90],
        [410, 90],
        [410, 27],
        [440, 27],
      ],
    });
    expect(at("changed:a:r2")).toMatchObject({
      where: "c1#2 >r2",
      points: [
        [176, 29.5],
        [404, 29.5],
        [404, 227],
        [440, 227],
      ],
    });
  });

  it("puts each band row's tracks over that row, its repos taken left to right", () => {
    const { at } = routeCards(
      "indented",
      { r2: repo(144, 200), a: agent(0, 0), r1: repo(0, 200), r3: repo(0, 260) },
      [
        ["changed", "a", "r1"],
        ["changed", "a", "r2"],
        ["changed", "a", "r3"],
      ],
    );
    // Row 200 splits the 148 px under the tree in thirds; r3 has the 16 px over its own row.
    expect(at("changed:a:r1").points[2][1]).toBeCloseTo(52 + 148 / 3);
    expect(at("changed:a:r2").points[2][1]).toBeCloseTo(52 + (2 * 148) / 3);
    expect(at("changed:a:r3")).toEqual({
      where: "gutter#2 >r3",
      label: { distance: 70 },
      points: [
        [176, 19],
        [304, 19],
        [304, 252],
        [66, 252],
        [66, 270],
      ],
    });
  });

  it("draws straight a change from an agent in its repo's own column", () => {
    const { at } = routeCards("columns", { a: agent(440, 64), r: repo(440, 0) }, [
      ["changed", "a", "r"],
    ]);
    expect(at("changed:a:r")).toEqual({
      where: "#0 changed:a:r",
      label: { fraction: 0.5 },
      points: [
        [528, 90],
        [506, 22],
      ],
    });
  });

  it("squeezes a corridor's lanes into the room a wide card leaves, from the side it keeps to", () => {
    const cards = {
      a: agent(0, 0),
      b: agent(0, 64),
      c: agent(0, 128),
      d: agent(0, 192),
    };
    const edges: [OverlayEdge["kind"], string, string][] = [
      ["message", "a", "c"],
      ["message", "b", "d"],
    ];
    const laneXs = (width: number, previous?: LaneMemory) => {
      const { at } = routeCards(
        "columns",
        { ...cards, a: { ...cards.a, w: width } },
        edges,
        previous,
      );
      return [at("message:a:c").points[1][0], at("message:b:d").points[1][0]];
    };
    // A 200 px card leaves 4 px of corridor: two lanes 4 px apart rather than 6.
    expect(laneXs(200)).toEqual([208, 212]);
    expect(laneXs(200, new Map([["order|c0", -1]]))).toEqual([212, 208]);
    // Wider still, no room is left: the lanes meet.
    expect(laneXs(210)).toEqual([218, 218]);
    expect(laneXs(176)).toEqual([184, 190]);
    // 8 px still holds two lanes 6 px apart.
    expect(laneXs(196)).toEqual([204, 210]);
  });

  it("measures a corridor from the widest agent of its column, never from a repo", () => {
    const { at } = routeCards(
      "columns",
      {
        a: { ...agent(0, 0), w: 150 },
        b: { ...agent(0, 128), w: 150 },
        q: { ...repo(0, 300), w: 200 },
      },
      [["message", "a", "b"]],
    );
    expect(at("message:a:b").points[1]).toEqual([158, 26]);
  });

  it("spreads ports over what a short card has, and stacks them on one too short", () => {
    const { at } = routeCards(
      "indented",
      { a: { ...agent(0, 0), h: 20 }, b: { ...agent(0, 64), h: 10 } },
      [
        ["message", "a", "b"],
        ["report", "a", "b"],
      ],
    );
    // 20 px less 7 at each end leaves 6 px between the two ports; 10 px leaves none.
    expect(at("message:a:b").points).toEqual([
      [176, 13],
      [189, 13],
      [189, 69],
      [176, 69],
    ]);
    expect(at("report:a:b").points).toEqual([
      [176, 7],
      [191, 7],
      [191, 69],
      [176, 69],
    ]);
  });

  it("orders the ports into a card's left side: the line from above enters higher", () => {
    const { at } = routeCards("columns", { a: agent(0, 0), b: agent(220, 64), c: agent(0, 128) }, [
      ["message", "a", "b"],
      ["message", "c", "b"],
    ]);
    expect(at("message:a:b").points.at(-1)).toEqual([220, 86.5]);
    expect(at("message:c:b").points.at(-1)).toEqual([220, 93.5]);
  });

  it("spreads the ports of a card's left side over every line into it, crossing or not", () => {
    const { at } = routeCards(
      "columns",
      { a: agent(0, 0), c: agent(220, 0), e: agent(220, 192), b: agent(440, 128) },
      [
        ["message", "a", "b"],
        ["message", "e", "b"],
      ],
    );
    expect(at("message:a:b").points.at(-1)).toEqual([440, 150.5]);
    expect(at("message:e:b").points.at(-1)).toEqual([440, 157.5]);
  });

  it("gives runs along one gap tracks of their own, keeping a remembered one while it is free", () => {
    const cards = { a: agent(0, 0), c: agent(220, 0), b: agent(440, 128), e: agent(440, 192) };
    const edges: [OverlayEdge["kind"], string, string][] = [
      ["message", "a", "b"],
      ["message", "a", "e"],
    ];
    const gapY = (previous?: LaneMemory) => {
      const { at } = routeCards("columns", cards, edges, previous);
      return [at("message:a:b").points[2][1], at("message:a:e").points[2][1]];
    };
    // The middle of the gap first, then 3 px over it, then 3 px under it.
    expect(gapY()).toEqual([58, 55]);
    expect(
      gapY(
        new Map([
          ["track|a|b", 2],
          ["track|a|e", 2],
        ]),
      ),
    ).toEqual([61, 58]);
  });

  it("takes the gap under a card when the other card's middle is level with its top", () => {
    const { at } = routeCards(
      "columns",
      { a: agent(0, 64), c: agent(220, 64), b: agent(440, 38) },
      [["message", "a", "b"]],
    );
    expect(at("message:a:b").points).toEqual([
      [176, 90],
      [184, 90],
      [184, 122],
      [404, 122],
      [404, 64],
      [440, 64],
    ]);
  });

  it.each([
    ["2 px under the row", { x: 220, y: 54, w: 176 }, false],
    ["less than 2 px under it", { x: 220, y: 53, w: 176 }, true],
    ["2 px over the row", { x: 220, y: -54, w: 176 }, false],
    ["less than 2 px over it", { x: 220, y: -53, w: 176 }, true],
    ["from the corridor on", { x: 404, y: 0, w: 10 }, false],
    ["a pixel before the corridor", { x: 403, y: 0, w: 10 }, true],
    ["ending where the card ends", { x: 166, y: 0, w: 10 }, false],
    ["ending a pixel past it", { x: 167, y: 0, w: 10 }, true],
  ])("goes around a card %s only when it is in the way", (_, place, blocks) => {
    const { at } = routeCards(
      "columns",
      { a: agent(0, 0), c: { ...agent(0, 0), ...place }, b: agent(440, 128) },
      [["message", "a", "b"]],
    );
    expect(at("message:a:b").points).toHaveLength(blocks ? 6 : 4);
  });

  it("shares a lane between bundles 2 px apart along the tree, not closer", () => {
    const lanes = (y: number) => {
      const { at } = routeCards(
        "indented",
        { a: agent(0, 0), b: agent(28, 64), c: agent(28, y), d: agent(56, y + 64) },
        [
          ["message", "a", "b"],
          ["message", "c", "d"],
        ],
      );
      return [at("message:a:b").where, at("message:c:d").where];
    };
    expect(lanes(118)).toEqual(["gutter#0 a|b", "gutter#0 c|d"]);
    expect(lanes(117)).toEqual(["gutter#0 a|b", "gutter#1 c|d"]);
    // The same with the lower bundle given its lane first.
    const lowerFirst = (y: number) => {
      const { at } = routeCards(
        "indented",
        { x: agent(0, 0), y: agent(28, 64), a: agent(28, y), b: agent(56, y + 64) },
        [
          ["message", "x", "y"],
          ["message", "a", "b"],
        ],
      );
      return [at("message:a:b").where, at("message:x:y").where];
    };
    expect(lowerFirst(118)).toEqual(["gutter#0 a|b", "gutter#0 x|y"]);
    expect(lowerFirst(117)).toEqual(["gutter#0 a|b", "gutter#1 x|y"]);
  });

  it("orders a bundle's lines by kind before the card they leave", () => {
    const { at } = routeCards("indented", { a: agent(0, 0), b: agent(28, 64) }, [
      ["message", "b", "a"],
      ["report", "a", "b"],
    ]);
    expect(at("message:b:a").label).toEqual({ fraction: 0.42 });
    expect(at("report:a:b").label).toEqual({ fraction: 0.58 });
  });

  it("puts the nearer lane's port first on a card level with the other", () => {
    const { at } = routeCards("columns", { a: agent(0, 0), b: agent(220, 0) }, [
      ["message", "a", "b"],
      ["report", "b", "a"],
    ]);
    expect(at("message:a:b").points).toEqual([
      [176, 22.5],
      [183, 22.5],
      [183, 29.5],
      [220, 29.5],
    ]);
    expect(at("report:b:a").points).toEqual([
      [220, 22.5],
      [185, 22.5],
      [185, 29.5],
      [176, 29.5],
    ]);
  });

  it("keeps a lone lane at the start of a corridor too narrow for two, or closed", () => {
    for (const [width, x] of [
      [200, 208],
      [210, 218],
    ]) {
      const { at } = routeCards("columns", { a: { ...agent(0, 0), w: width }, b: agent(0, 128) }, [
        ["message", "a", "b"],
      ]);
      expect(at("message:a:b").points[1]).toEqual([x, 26]);
    }
  });

  it("squeezes a bundle's lines with its lanes, from either side", () => {
    const cards = {
      a: { ...agent(0, 0), w: 200 },
      b: agent(0, 64),
      c: agent(0, 128),
      d: agent(0, 192),
    };
    const edges: [OverlayEdge["kind"], string, string][] = [
      ["message", "a", "c"],
      ["report", "a", "c"],
      ["message", "b", "d"],
    ];
    const laneXs = (previous?: LaneMemory) => {
      const { at } = routeCards("columns", cards, edges, previous);
      return ["message:a:c", "report:a:c", "message:b:d"].map((id) => at(id).points[1][0]);
    };
    // Lanes 4 px apart rather than 6: the lines 2 px apart become 4/6 of that.
    const left = laneXs();
    expect(left[0]).toBeCloseTo(208 - 2 / 3);
    expect(left[1]).toBeCloseTo(208 + 2 / 3);
    expect(left[2]).toBe(212);
    const right = laneXs(new Map([["order|c0", -1]]));
    expect(right[0]).toBeCloseTo(212 - 2 / 3);
    expect(right[1]).toBeCloseTo(212 + 2 / 3);
    expect(right[2]).toBe(208);
  });

  it("gives a corridor's trunks their lanes by how far their lines spread, the least first", () => {
    const { at } = routeCards(
      "columns",
      { a: agent(0, 300), d: agent(220, 400), r1: repo(440, 400), r2: repo(440, 0) },
      [
        ["changed", "d", "r1"],
        ["changed", "a", "r2"],
      ],
    );
    expect(at("changed:d:r1").where).toBe("c1#0 >r1");
    expect(at("changed:a:r2").where).toBe("c1#1 >r2");
  });

  it("orders the lines of a bundle of one kind from one card by id", () => {
    const boxes = new Map([
      ["a", agent(0, 0)],
      ["b", agent(28, 64)],
    ]);
    const m1 = { id: "m1", from: "a", to: "b", kind: "message" } as const;
    const m2 = { ...m1, id: "m2" };
    for (const edges of [
      [m2, m1],
      [m1, m2],
    ]) {
      const { routes } = routeOverlay("indented", boxes, edges);
      expect(routes.get("m1")?.label).toEqual({ fraction: 0.42 });
      expect(routes.get("m2")?.label).toEqual({ fraction: 0.58 });
    }
  });
});

describe("resolvePoints", () => {
  it("keeps a corner where it is when its node is not drawn", () => {
    expect(resolvePoints([{ x: 5, y: 7, xn: "gone", yn: "gone" }], () => undefined)).toEqual([
      { x: 5, y: 7 },
    ]);
  });
});

describe("edgeEmphasis", () => {
  const model = graphModel(busy(), "s1");
  const touching = (id: string): string[] =>
    model.edges
      .filter((edge) => edge.from === id || edge.to === id)
      .map((edge) => edge.id)
      .toSorted();

  it("brings out exactly the edges of the picked node", () => {
    for (const id of ["agent:a1", "agent:sub3", "repo:web", "user"]) {
      const result = edgeEmphasis(model, { kind: "node", id }, undefined);
      expect(result.active).toBe(true);
      expect([...result.hot].toSorted()).toEqual(touching(id));
    }
  });

  it("brings out those of the focused node when nothing is picked, and the picked one first", () => {
    expect([...edgeEmphasis(model, undefined, "agent:a3").hot].toSorted()).toEqual(
      touching("agent:a3"),
    );
    const picked = edgeEmphasis(model, { kind: "node", id: "agent:a2" }, "agent:a3");
    expect([...picked.hot].toSorted()).toEqual(touching("agent:a2"));
  });

  it("brings out the picked edge alone, and nothing when nothing is picked or focused", () => {
    const edge = edgeEmphasis(model, { kind: "edge", id: "report:agent:a3:agent:a1" }, "agent:a1");
    expect([...edge.hot]).toEqual(["report:agent:a3:agent:a1"]);
    expect(edgeEmphasis(model, undefined, undefined)).toEqual({ active: false, hot: new Set() });
    // A node the graph no longer holds brings out nothing.
    expect(edgeEmphasis(model, { kind: "node", id: "agent:gone" }, undefined).active).toBe(false);
  });
});
