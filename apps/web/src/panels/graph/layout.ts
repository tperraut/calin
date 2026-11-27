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
  const ids = new Set(nodes.map((node) => node.id));
  const earlier = byPrevious(previous);
  const kids = new Map<string, GraphNode[]>();
  const roots: GraphNode[] = [];
  const repos: GraphNode[] = [];
  for (const node of nodes) {
    if (node.kind === "repo") {
      repos.push(node);
    } else if (node.parent_id !== null && node.parent_id !== node.id && ids.has(node.parent_id)) {
      const list = kids.get(node.parent_id);
      if (list) list.push(node);
      else kids.set(node.parent_id, [node]);
    } else {
      // The user, or an agent whose parent this graph does not hold.
      roots.push(node);
    }
  }
  // `sort` is stable: equal ranks keep the input order.
  for (const list of kids.values()) list.sort(earlier);
  roots.sort((a, b) => Number(b.kind === "user") - Number(a.kind === "user") || earlier(a, b));
  repos.sort(earlier);

  const positions = new Map<string, Point>();
  const order: string[] = [];
  const parent = new Map<string, string>();
  const children = new Map<string, readonly string[]>();
  const step = mode === "indented" ? INDENT : COLUMN_STEP;
  // Rows a node takes before its first child: its own, or none when the child sits beside it.
  const own = mode === "indented" ? 1 : 0;
  let depthCount = 0;
  let treeWidth = 0;
  const seen = new Set<string>();

  const place = (node: GraphNode, depth: number, top: number): number => {
    // A cycle in `parent_id` would otherwise never end.
    if (seen.has(node.id)) return 0;
    seen.add(node.id);
    order.push(node.id);
    positions.set(node.id, { x: depth * step, y: top * SLOT });
    depthCount = Math.max(depthCount, depth + 1);
    treeWidth = Math.max(treeWidth, depth * step + sizeOf(node).w);
    const mine = kids.get(node.id) ?? [];
    children.set(
      node.id,
      mine.map((child) => child.id),
    );
    let rows = own;
    for (const child of mine) {
      parent.set(child.id, node.id);
      rows += place(child, depth + 1, top + rows);
    }
    return Math.max(rows, 1);
  };
  let treeRows = 0;
  for (const root of roots) treeRows += place(root, 0, treeRows);
  // Members of a cycle hang under nothing reachable: they still get a row.
  for (const node of nodes) {
    if (node.kind !== "repo" && !seen.has(node.id)) treeRows += place(node, 0, treeRows);
  }

  const repoSize = NODE_SIZE.repo;
  let width = treeWidth;
  let height = Math.max(0, treeRows * SLOT - (SLOT - NODE_SIZE.agent.h));
  repos.forEach((repo, index) => {
    let at: Point;
    if (mode === "indented") {
      at = {
        x: (index % REPOS_PER_ROW) * (repoSize.w + REPO_GAP),
        y: treeRows * SLOT + BAND_GAP + Math.floor(index / REPOS_PER_ROW) * (repoSize.h + REPO_GAP),
      };
    } else {
      at = {
        x: Math.max(MIN_COLUMNS, depthCount) * COLUMN_STEP,
        y: index * (repoSize.h + REPO_GAP),
      };
    }
    positions.set(repo.id, at);
    order.push(repo.id);
    width = Math.max(width, at.x + repoSize.w);
    height = Math.max(height, at.y + repoSize.h);
  });

  return { mode, positions, order, parent, children, width, height };
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
