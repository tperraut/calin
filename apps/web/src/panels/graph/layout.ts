import type { GraphNode } from "../../gen/GraphNode";
import type { NodeKind } from "../../gen/NodeKind";

export type Point = { readonly x: number; readonly y: number };
export type Size = { readonly w: number; readonly h: number };

/**
 * `indented`: one row per node, a child one step to the right of its parent
 * and below it; fits a phone. `columns`: one column per depth, a parent beside
 * its first child; for a wide pane.
 */
export type LayoutMode = "indented" | "columns";

export const NODE_SIZE: Readonly<Record<NodeKind, Size>> = {
  // No card is shorter than a 44px touch target.
  user: { w: 96, h: 44 },
  agent: { w: 176, h: 52 },
  repo: { w: 132, h: 44 },
};

/** Height of one row of the tree: the tallest card plus the gap under it. */
export const SLOT = 64;
/** How far a child is indented under its parent, in `indented` mode. */
export const INDENT = 28;
/** Distance between two columns, in `columns` mode. */
export const COLUMN_STEP = 220;
const REPO_GAP = 12;
const BAND_GAP = 20;
/** Repos per row of the band under the tree: two fit a 360px phone. */
const REPOS_PER_ROW = 2;
/**
 * Columns kept for the tree before the repo column: user, lead, a child's lead
 * or a subagent, a child's subagent (a child cannot dispatch, api.rs). The repo
 * column therefore only moves for a tree deeper than the contract allows.
 */
const MIN_COLUMNS = 4;

export const sizeOf = (node: Pick<GraphNode, "kind">): Size =>
  NODE_SIZE[node.kind] ?? NODE_SIZE.agent;

export type Layout = {
  readonly mode: LayoutMode;
  /** Top-left corner of every node. */
  readonly positions: ReadonlyMap<string, Point>;
  /** The tree depth-first, a parent before its children, then the repos: the order focus moves in. */
  readonly order: readonly string[];
  /** The node each tree node hangs under. */
  readonly parent: ReadonlyMap<string, string>;
  readonly children: ReadonlyMap<string, readonly string[]>;
  readonly width: number;
  readonly height: number;
};

/** Earlier on screen first: by row, then by column. */
function byPrevious(
  previous: ReadonlyMap<string, Point> | undefined,
): (a: GraphNode, b: GraphNode) => number {
  return (a, b) => {
    const pa = previous?.get(a.id);
    const pb = previous?.get(b.id);
    // A node that was already placed keeps its place before any newcomer.
    if (pa && pb) return pa.y - pb.y || pa.x - pb.x;
    if (pa) return -1;
    if (pb) return 1;
    return 0;
  };
}

/**
 * Places the spawn tree (`parent_id`) and the repo band. Pure, and stable by
 * construction: siblings keep the order in which they first appeared (read
 * from `previous`, the positions of the last layout; newcomers follow in input
 * order), x depends on depth only, and a node's row depends only on what is
 * before it in the tree. Adding a leaf therefore moves nothing above it and
 * moves what is below it (later siblings of its ancestors, and the repo band
 * when it lies under the tree) down by exactly one `SLOT`.
 */
export function layoutGraph(
  nodes: readonly GraphNode[],
  previous: ReadonlyMap<string, Point> | undefined,
  mode: LayoutMode,
): Layout {
  const { kids, roots, repos } = groupNodes(nodes, byPrevious(previous));
  const placement = new Placement(kids, mode);
  for (const root of roots) placement.placeTree(root);
  // Members of a cycle hang under nothing reachable: they still get a row.
  for (const node of nodes) {
    if (node.kind !== "repo" && !placement.holds(node.id)) placement.placeTree(node);
  }
  repos.forEach((repo, index) => placement.placeRepo(repo, index));
  return placement.layout();
}

/** The node a node hangs under, when the graph holds it. */
function heldParent(node: GraphNode, ids: ReadonlySet<string>): string | undefined {
  const parent = node.parent_id;
  return parent !== null && parent !== node.id && ids.has(parent) ? parent : undefined;
}

/** The children of each node, the roots of the tree and the repos, each in the order they show. */
function groupNodes(
  nodes: readonly GraphNode[],
  earlier: (a: GraphNode, b: GraphNode) => number,
): { kids: ReadonlyMap<string, readonly GraphNode[]>; roots: GraphNode[]; repos: GraphNode[] } {
  const ids = new Set(nodes.map((node) => node.id));
  const kids = new Map<string, GraphNode[]>();
  const roots: GraphNode[] = [];
  const repos = nodes.filter((node) => node.kind === "repo");
  for (const node of nodes) {
    if (node.kind === "repo") continue;
    const parent = heldParent(node, ids);
    // The user, or an agent whose parent this graph does not hold, starts a tree.
    if (parent === undefined) roots.push(node);
    else kids.set(parent, [...(kids.get(parent) ?? []), node]);
  }
  // `sort` is stable: equal ranks keep the input order.
  for (const list of kids.values()) list.sort(earlier);
  roots.sort((a, b) => Number(b.kind === "user") - Number(a.kind === "user") || earlier(a, b));
  repos.sort(earlier);
  return { kids, roots, repos };
}

/** Where a repo of the band sits: under the tree on a phone, in a column of its own when wide. */
function repoSpot(mode: LayoutMode, index: number, treeRows: number, depthCount: number): Point {
  const size = NODE_SIZE.repo;
  if (mode === "indented") {
    return {
      x: (index % REPOS_PER_ROW) * (size.w + REPO_GAP),
      y: treeRows * SLOT + BAND_GAP + Math.floor(index / REPOS_PER_ROW) * (size.h + REPO_GAP),
    };
  }
  return { x: Math.max(MIN_COLUMNS, depthCount) * COLUMN_STEP, y: index * (size.h + REPO_GAP) };
}

/** The layout as it is built: the trees one under the other, then the repo band. */
class Placement {
  private readonly positions = new Map<string, Point>();
  private readonly order: string[] = [];
  private readonly parent = new Map<string, string>();
  private readonly children = new Map<string, readonly string[]>();
  private readonly seen = new Set<string>();
  private readonly kids: ReadonlyMap<string, readonly GraphNode[]>;
  private readonly mode: LayoutMode;
  private readonly step: number;
  /** Rows a node takes before its first child: its own, or none when the child sits beside it. */
  private readonly own: number;
  private depthCount = 0;
  private treeRows = 0;
  private width = 0;
  private height = 0;

  constructor(kids: ReadonlyMap<string, readonly GraphNode[]>, mode: LayoutMode) {
    this.kids = kids;
    this.mode = mode;
    this.step = mode === "indented" ? INDENT : COLUMN_STEP;
    this.own = mode === "indented" ? 1 : 0;
  }

  holds(id: string): boolean {
    return this.seen.has(id);
  }

  /** A tree under the ones already placed. */
  placeTree(root: GraphNode): void {
    this.treeRows += this.place(root, 0, this.treeRows);
    this.height = this.treeRows * SLOT - (SLOT - NODE_SIZE.agent.h);
  }

  /** A node and everything under it from row `top`; the rows they take. */
  private place(node: GraphNode, depth: number, top: number): number {
    // A cycle in `parent_id` would otherwise never end.
    if (this.seen.has(node.id)) return 0;
    this.seen.add(node.id);
    this.put(node.id, { x: depth * this.step, y: top * SLOT });
    this.depthCount = Math.max(this.depthCount, depth + 1);
    this.width = Math.max(this.width, depth * this.step + sizeOf(node).w);
    const mine = this.kids.get(node.id) ?? [];
    this.children.set(
      node.id,
      mine.map((child) => child.id),
    );
    let rows = this.own;
    for (const child of mine) {
      this.parent.set(child.id, node.id);
      rows += this.place(child, depth + 1, top + rows);
    }
    return Math.max(rows, 1);
  }

  placeRepo(repo: GraphNode, index: number): void {
    const at = repoSpot(this.mode, index, this.treeRows, this.depthCount);
    this.put(repo.id, at);
    this.width = Math.max(this.width, at.x + NODE_SIZE.repo.w);
    this.height = Math.max(this.height, at.y + NODE_SIZE.repo.h);
  }

  private put(id: string, at: Point): void {
    this.positions.set(id, at);
    this.order.push(id);
  }

  layout(): Layout {
    const { mode, positions, order, parent, children, width, height } = this;
    return { mode, positions, order, parent, children, width, height };
  }
}

/** How far each node that both layouts hold moved, in pixels. */
export function displacements(
  before: ReadonlyMap<string, Point>,
  after: ReadonlyMap<string, Point>,
): Map<string, number> {
  const moved = new Map<string, number>();
  for (const [id, was] of before) {
    const now = after.get(id);
    if (now) moved.set(id, Math.hypot(now.x - was.x, now.y - was.y));
  }
  return moved;
}
