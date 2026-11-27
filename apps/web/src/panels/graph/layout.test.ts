import { describe, expect, it } from "vite-plus/test";
import type { GraphNode } from "../../gen/GraphNode";
import {
  agentNode,
  makeAgent,
  makeGraph,
  makeSubagent,
  repoNode,
  userNode,
} from "../../test/orchestration";
import {
  COLUMN_STEP,
  displacements,
  INDENT,
  layoutGraph,
  sizeOf,
  SLOT,
  type Layout,
  type LayoutMode,
  type Point,
} from "./layout";
import { synthesize } from "./stress/synthetic";

const modes: readonly LayoutMode[] = ["indented", "columns"];

const sub = (id: string, parent: string): GraphNode =>
  agentNode(
    makeSubagent(id, "explorer", { parent_agent_id: parent.replace("agent:", "") }),
    parent,
  );
const lead = (): GraphNode => agentNode(makeAgent("a1"), "user");

/** A lead with `count` subagents, and two repos. */
function wide(count: number): GraphNode[] {
  return [
    userNode(),
    lead(),
    ...Array.from({ length: count }, (_, index) => sub(`w${index}`, "agent:a1")),
    repoNode("api"),
    repoNode("web"),
  ];
}

/** A chain: each agent started by the one before. */
function deep(count: number): GraphNode[] {
  const nodes = [userNode(), lead()];
  let parent = "agent:a1";
  for (let index = 0; index < count; index += 1) {
    nodes.push(sub(`d${index}`, parent));
    parent = `agent:d${index}`;
  }
  return [...nodes, repoNode("api")];
}

const mixed = (count: number): GraphNode[] =>
  synthesize(count, Math.round(count * 1.5)).map((step) => step.node);

function overlaps(nodes: readonly GraphNode[], layout: Layout): string[] {
  const boxes = nodes.map((node) => {
    const at = layout.positions.get(node.id);
    if (!at) throw new Error(`${node.id} has no position`);
    return { id: node.id, ...at, ...sizeOf(node) };
  });
  const found: string[] = [];
  for (let a = 0; a < boxes.length; a += 1) {
    for (let b = a + 1; b < boxes.length; b += 1) {
      const one = boxes[a];
      const two = boxes[b];
      if (
        one.x < two.x + two.w &&
        two.x < one.x + one.w &&
        one.y < two.y + two.h &&
        two.y < one.y + one.h
      ) {
        found.push(`${one.id} × ${two.id}`);
      }
    }
  }
  return found;
}

/** Everything that may move when `leaf` is added: what comes after it in the tree, and the repos. */
function mayMove(layout: Layout, leaf: string, nodes: readonly GraphNode[]): Set<string> {
  const after = layout.order.slice(layout.order.indexOf(leaf) + 1);
  return new Set([
    ...after,
    ...nodes.filter((node) => node.kind === "repo").map((node) => node.id),
  ]);
}

describe("layoutGraph", () => {
  it("is deterministic, and a second pass over its own positions changes nothing", () => {
    for (const mode of modes) {
      const nodes = mixed(120);
      const first = layoutGraph(nodes, undefined, mode);
      expect([...layoutGraph(nodes, undefined, mode).positions]).toEqual([...first.positions]);
      expect([...layoutGraph(nodes, first.positions, mode).positions]).toEqual([
        ...first.positions,
      ]);
      expect(layoutGraph(nodes, first.positions, mode).order).toEqual(first.order);
    }
  });

  it("puts the user at the root, the lead under it, and every agent under the one that started it", () => {
    const graph = makeGraph();
    const tree = layoutGraph(graph.nodes, undefined, "indented");
    expect(tree.order).toEqual([
      "user",
      "agent:a1",
      "agent:sub1",
      "agent:sub2",
      "agent:a2",
      "agent:a3",
      "agent:sub3",
      "repo:api",
      "repo:web",
      "repo:docs",
    ]);
    expect(tree.parent.get("agent:sub3")).toBe("agent:a3");
    expect(tree.children.get("agent:a1")).toEqual([
      "agent:sub1",
      "agent:sub2",
      "agent:a2",
      "agent:a3",
    ]);
    // One row each, one step in per level.
    expect(tree.positions.get("agent:sub3")).toEqual({ x: 3 * INDENT, y: 6 * SLOT });
    const columns = layoutGraph(graph.nodes, undefined, "columns");
    // A parent sits beside its first child.
    expect(columns.positions.get("agent:a1")?.y).toBe(columns.positions.get("agent:sub1")?.y);
    expect(columns.positions.get("agent:sub3")).toEqual({ x: 3 * COLUMN_STEP, y: 3 * SLOT });
  });

  it("keeps siblings in the order they first appeared, whatever order they come in later", () => {
    for (const mode of modes) {
      const nodes = wide(6);
      const first = layoutGraph(nodes, undefined, mode);
      const shuffled = [
        nodes[0],
        nodes[1],
        nodes[5],
        nodes[2],
        nodes[7],
        nodes[4],
        nodes[3],
        nodes[6],
        nodes[9],
        nodes[8],
      ];
      const again = layoutGraph(shuffled, first.positions, mode);
      expect(Object.fromEntries(again.positions)).toEqual(Object.fromEntries(first.positions));
      // A newcomer goes after the siblings already there, even when it is listed first.
      const withNew = layoutGraph(
        [nodes[0], nodes[1], sub("late", "agent:a1"), ...nodes.slice(2)],
        first.positions,
        mode,
      );
      expect(withNew.children.get("agent:a1")?.at(-1)).toBe("agent:late");
    }
  });

  it("has no two nodes overlapping, for wide, deep and mixed trees, up to 200 nodes", () => {
    for (const mode of modes) {
      for (const nodes of [wide(40), deep(12), mixed(60), mixed(200), makeGraph().nodes]) {
        expect(overlaps(nodes, layoutGraph(nodes, undefined, mode))).toEqual([]);
      }
    }
  });

  // The measured bounds: how far existing nodes move when one leaf is added.
  const cases: {
    name: string;
    mode: LayoutMode;
    before: () => GraphNode[];
    leaf: GraphNode;
    /** Nodes that move, and by how much each (always straight down). */
    moved: number;
    by: number;
  }[] = [
    // Last child of a wide parent: only the repo band under the tree moves.
    {
      name: "wide, last sibling",
      mode: "indented",
      before: () => wide(40),
      leaf: sub("new", "agent:a1"),
      moved: 2,
      by: SLOT,
    },
    {
      name: "wide, last sibling",
      mode: "columns",
      before: () => wide(40),
      leaf: sub("new", "agent:a1"),
      moved: 0,
      by: 0,
    },
    // First child of the first of 40 siblings: the 39 after it move one row.
    {
      name: "wide, under the first sibling",
      mode: "indented",
      before: () => wide(40),
      leaf: sub("new", "agent:w0"),
      moved: 41,
      by: SLOT,
    },
    {
      name: "wide, under the first sibling",
      mode: "columns",
      before: () => wide(40),
      leaf: sub("new", "agent:w0"),
      moved: 0,
      by: 0,
    },
    // A second child there does take a row in the column layout.
    {
      name: "wide, second child under the first sibling",
      mode: "columns",
      before: () => [...wide(40), sub("first", "agent:w0")],
      leaf: sub("new", "agent:w0"),
      moved: 39,
      by: SLOT,
    },
    {
      name: "deep, at the end of the chain",
      mode: "indented",
      before: () => deep(12),
      leaf: sub("new", "agent:d11"),
      moved: 1,
      by: SLOT,
    },
    {
      name: "deep, at the end of the chain",
      mode: "columns",
      before: () => deep(12),
      leaf: sub("new", "agent:d11"),
      moved: 1,
      by: COLUMN_STEP,
    },
    {
      name: "deep, a sibling half way",
      mode: "indented",
      before: () => deep(12),
      leaf: sub("new", "agent:d5"),
      moved: 1,
      by: SLOT,
    },
    {
      name: "deep, a sibling half way",
      mode: "columns",
      before: () => deep(12),
      leaf: sub("new", "agent:d5"),
      moved: 0,
      by: 0,
    },
  ];

  it.each(cases)("adding a leaf ($name, $mode) moves $moved nodes by $by px", (one) => {
    const nodes = one.before();
    const before = layoutGraph(nodes, undefined, one.mode);
    const withLeaf = [...nodes, one.leaf];
    const after = layoutGraph(withLeaf, before.positions, one.mode);
    const moved = [...displacements(before.positions, after.positions)].filter(
      ([, distance]) => distance > 0,
    );
    expect(moved.map(([, distance]) => distance)).toEqual(
      Array.from({ length: one.moved }, () => one.by),
    );
    // Nothing above the newcomer moves: not its ancestors, not their earlier siblings.
    const allowed = mayMove(after, one.leaf.id, withLeaf);
    expect(moved.filter(([id]) => !allowed.has(id))).toEqual([]);
    // Agents only ever move straight down.
    for (const [id] of moved) {
      if (id.startsWith("repo:")) continue;
      expect(after.positions.get(id)?.x).toBe(before.positions.get(id)?.x);
    }
  });

  it("stays within one row per arrival over a 200-node session replayed node by node", () => {
    for (const mode of modes) {
      const all = mixed(200);
      let previous: ReadonlyMap<string, Point> | undefined;
      let worst = 0;
      let total = 0;
      let movers = 0;
      let existing = 0;
      for (let count = 1; count <= all.length; count += 1) {
        const layout = layoutGraph(all.slice(0, count), previous, mode);
        if (previous) {
          const moved = displacements(previous, layout.positions);
          for (const distance of moved.values()) {
            worst = Math.max(worst, distance);
            total += distance;
            if (distance > 0) movers += 1;
          }
          existing += moved.size;
        }
        previous = layout.positions;
      }
      const mean = total / existing;
      const share = movers / existing;
      // Measured: indented 16.5 px on average, 25.8% of the existing nodes move per arrival;
      // columns 11.8 px and 18.5%. In both, no node ever moves more than one row at once.
      expect(worst).toBe(SLOT);
      expect(mean).toBeLessThan(mode === "indented" ? 18 : 13);
      expect(share).toBeLessThan(mode === "indented" ? 0.28 : 0.2);
    }
  });

  it("places the repos in a band of their own: under the tree, or in a column beside it", () => {
    const graph = makeGraph();
    const repos = graph.nodes.filter((node) => node.kind === "repo");
    const agents = graph.nodes.filter((node) => node.kind !== "repo");
    const indented = layoutGraph(graph.nodes, undefined, "indented");
    const lowest = Math.max(
      ...agents.map((node) => (indented.positions.get(node.id)?.y ?? 0) + sizeOf(node).h),
    );
    expect(repos.every((repo) => (indented.positions.get(repo.id)?.y ?? 0) > lowest)).toBe(true);
    // Two to a row: what a 360px phone holds.
    expect(indented.positions.get("repo:api")?.y).toBe(indented.positions.get("repo:web")?.y);
    expect(indented.positions.get("repo:docs")?.x).toBe(indented.positions.get("repo:api")?.x);
    expect(indented.positions.get("repo:docs")?.y).toBeGreaterThan(
      indented.positions.get("repo:api")?.y ?? 0,
    );
    expect(indented.width).toBeLessThanOrEqual(360 - 32);

    const columns = layoutGraph(graph.nodes, undefined, "columns");
    const rightmost = Math.max(
      ...agents.map((node) => (columns.positions.get(node.id)?.x ?? 0) + sizeOf(node).w),
    );
    expect(repos.every((repo) => (columns.positions.get(repo.id)?.x ?? 0) > rightmost)).toBe(true);
    expect(new Set(repos.map((repo) => columns.positions.get(repo.id)?.x)).size).toBe(1);
  });

  it("keeps the repo column where it is while the tree grows, in the column layout", () => {
    const nodes = wide(3);
    const before = layoutGraph(nodes, undefined, "columns");
    // Deeper and taller, up to the four levels the contract allows.
    const grown = [...nodes, sub("x", "agent:w0"), sub("y", "agent:w1"), sub("z", "agent:w1")];
    const after = layoutGraph(grown, before.positions, "columns");
    expect(after.positions.get("repo:api")).toEqual(before.positions.get("repo:api"));
    expect(after.positions.get("repo:web")).toEqual(before.positions.get("repo:web"));
    // A new repo joins the band without moving the others.
    const more = layoutGraph([...grown, repoNode("docs")], after.positions, "indented");
    const indented = layoutGraph(grown, after.positions, "indented");
    expect(more.positions.get("repo:api")).toEqual(indented.positions.get("repo:api"));
    expect(more.positions.get("repo:web")).toEqual(indented.positions.get("repo:web"));
  });

  it("still places an agent whose parent is missing, and survives a cycle", () => {
    const orphan = sub("orphan", "agent:gone");
    const a = sub("ca", "agent:cb");
    const b = sub("cb", "agent:ca");
    for (const mode of modes) {
      const nodes = [userNode(), lead(), orphan, a, b];
      const layout = layoutGraph(nodes, undefined, mode);
      expect(layout.positions.size).toBe(5);
      expect(overlaps(nodes, layout)).toEqual([]);
      expect(layout.order.at(0)).toBe("user");
    }
  });
});
