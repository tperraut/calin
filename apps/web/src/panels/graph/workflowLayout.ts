import type { WorkflowEdge } from "../../gen/WorkflowEdge";
import type { WorkflowGraph } from "../../gen/WorkflowGraph";
import type { Point, Size } from "./layout";

/**
 * `across`: layers left to right, for a pane with room. `down`: layers top to
 * bottom, for a phone.
 */
export type Orientation = "across" | "down";

/** Below this pane width the drawing runs top to bottom. */
export const ACROSS_MIN_WIDTH = 560;

/** No card is shorter than a 44px touch target; three lines of text fit. */
export const WF_NODE: Size = { w: 168, h: 60 };

const PAD = 16;
/** Room between two layers: an edge's port name is written there, next to where it leaves. */
const LAYER_GAP_ACROSS = 96;
const LAYER_GAP_DOWN = 64;
/** Room between two nodes of one layer. */
const SLOT_GAP_ACROSS = 32;
const SLOT_GAP_DOWN = 24;
/** Where the first back edge runs outside the nodes, and how far apart the next ones run. */
const LANE_OFFSET = 24;
const LANE_STEP = 16;
const CORNER = 8;

export type PlacedNode = {
  id: string;
  x: number;
  y: number;
  w: number;
  h: number;
  layer: number;
  /** Position within the layer, from 0. */
  slot: number;
};

export type PlacedEdge = {
  /** Index of the edge in the graph's `edges`: its id. */
  index: number;
  edge: WorkflowEdge;
  /** True when it closes a cycle (see `backEdges`); drawn around the outside. */
  back: boolean;
  /** SVG path data, from where it leaves to the side of the node it enters. */
  d: string;
  /** Where its port name is written, near its start. */
  label: Point & { anchor: "start" | "end" };
  /** Where a traversal count sits. */
  middle: Point;
};

export type WorkflowLayout = {
  orientation: Orientation;
  /** In the graph's file order. */
  nodes: readonly PlacedNode[];
  byId: ReadonlyMap<string, PlacedNode>;
  /** Only edges between two nodes of the graph. */
  edges: readonly PlacedEdge[];
  /** Layer by layer, each in file order: the order the keyboard moves in. */
  order: readonly string[];
  layers: readonly (readonly string[])[];
  width: number;
  height: number;
};

const round = (value: number): number => Math.round(value * 10) / 10;

/**
 * The edges that close a cycle in file order: one that enters a node declared
 * no later than the one it leaves, when that node leads back to it. Every
 * cycle has one (a cycle cannot keep going forward in the file), so the rest
 * is acyclic. Indices into `graph.edges`.
 */
export function backEdges(graph: WorkflowGraph): ReadonlySet<number> {
  const position = new Map(graph.nodes.map((node, index) => [node.id, index]));
  const next = new Map<string, string[]>();
  for (const edge of graph.edges) {
    next.set(edge.from, [...(next.get(edge.from) ?? []), edge.to]);
  }
  const reaches = (from: string, to: string): boolean => {
    const seen = new Set<string>([from]);
    const stack = [from];
    while (stack.length > 0) {
      const at = stack.pop() as string;
      if (at === to) return true;
      for (const one of next.get(at) ?? []) {
        if (!seen.has(one)) {
          seen.add(one);
          stack.push(one);
        }
      }
    }
    return false;
  };
  const back = new Set<number>();
  graph.edges.forEach((edge, index) => {
    const from = position.get(edge.from);
    const to = position.get(edge.to);
    if (from === undefined || to === undefined || to > from) return;
    if (reaches(edge.to, edge.from)) back.add(index);
  });
  return back;
}

/**
 * Each node's layer: the longest path to it from `start` (from any node
 * nothing enters, for a graph without one), over the edges that are not back edges.
 */
export function layersOf(
  graph: WorkflowGraph,
  back: ReadonlySet<number> = backEdges(graph),
): ReadonlyMap<string, number> {
  const ids = new Set(graph.nodes.map((node) => node.id));
  const forward = graph.edges.filter(
    (edge, index) => !back.has(index) && ids.has(edge.from) && ids.has(edge.to),
  );
  const incoming = new Map(graph.nodes.map((node) => [node.id, 0]));
  for (const edge of forward) incoming.set(edge.to, (incoming.get(edge.to) ?? 0) + 1);
  const layer = new Map(graph.nodes.map((node) => [node.id, 0]));
  // Kahn's order, ready nodes taken in file order, so the result never depends on anything else.
  const ready = graph.nodes.filter((node) => incoming.get(node.id) === 0).map((node) => node.id);
  const done = new Set<string>();
  while (ready.length > 0) {
    const at = ready.shift() as string;
    done.add(at);
    for (const edge of forward) {
      if (edge.from !== at) continue;
      layer.set(edge.to, Math.max(layer.get(edge.to) ?? 0, (layer.get(at) ?? 0) + 1));
      const left = (incoming.get(edge.to) ?? 0) - 1;
      incoming.set(edge.to, left);
      if (left === 0) ready.push(edge.to);
    }
  }
  return layer;
}

/** A path through `points` with rounded corners; every segment is horizontal or vertical. */
function orthogonal(points: readonly Point[]): string {
  let d = `M${round(points[0].x)},${round(points[0].y)}`;
  for (let index = 1; index < points.length - 1; index += 1) {
    const before = points[index - 1];
    const at = points[index];
    const after = points[index + 1];
    const inLength = Math.hypot(at.x - before.x, at.y - before.y);
    const outLength = Math.hypot(after.x - at.x, after.y - at.y);
    const r = Math.min(CORNER, inLength / 2, outLength / 2);
    const inX = at.x - (Math.sign(at.x - before.x) * r || 0);
    const inY = at.y - (Math.sign(at.y - before.y) * r || 0);
    const outX = at.x + (Math.sign(after.x - at.x) * r || 0);
    const outY = at.y + (Math.sign(after.y - at.y) * r || 0);
    d += `L${round(inX)},${round(inY)}Q${round(at.x)},${round(at.y)},${round(outX)},${round(outY)}`;
  }
  const last = points[points.length - 1];
  return `${d}L${round(last.x)},${round(last.y)}`;
}

function cubic(x0: number, y0: number, x1: number, y1: number, across: boolean): string {
  const reach = across ? Math.max(24, (x1 - x0) / 2) : Math.max(24, (y1 - y0) / 2);
  const [c1x, c1y, c2x, c2y] = across
    ? [x0 + reach, y0, x1 - reach, y1]
    : [x0, y0 + reach, x1, y1 - reach];
  return (
    `M${round(x0)},${round(y0)}C${round(c1x)},${round(c1y)},${round(c2x)},${round(c2y)},` +
    `${round(x1)},${round(y1)}`
  );
}

/**
 * Lays a workflow out in layers (see `layersOf`), each layer's nodes in file
 * order and centred on the widest layer. Edges leave a node from the side that
 * faces the next layer, one place per port in port order, and enter the
 * middle of the opposite side; a back edge goes out into the gap after its
 * node's layer, runs around the outside of the drawing in a lane of its own,
 * and comes back through the gap before its target's layer. Pure and deterministic.
 */
export function layoutWorkflow(graph: WorkflowGraph, orientation: Orientation): WorkflowLayout {
  const across = orientation === "across";
  const back = backEdges(graph);
  const layerOf = layersOf(graph, back);
  const layerCount = Math.max(0, ...layerOf.values()) + (graph.nodes.length > 0 ? 1 : 0);
  const layers: string[][] = Array.from({ length: layerCount }, () => []);
  for (const node of graph.nodes) layers[layerOf.get(node.id) ?? 0].push(node.id);
  const widest = Math.max(0, ...layers.map((layer) => layer.length));

  const { w, h } = WF_NODE;
  const layerStep = across ? w + LAYER_GAP_ACROSS : h + LAYER_GAP_DOWN;
  const slotStep = across ? h + SLOT_GAP_ACROSS : w + SLOT_GAP_DOWN;
  const placed = new Map<string, PlacedNode>();
  layers.forEach((layer, index) => {
    const offset = ((widest - layer.length) * slotStep) / 2;
    layer.forEach((id, slot) => {
      const along = PAD + index * layerStep;
      const side = PAD + offset + slot * slotStep;
      placed.set(id, {
        id,
        x: across ? along : side,
        y: across ? side : along,
        w,
        h,
        layer: index,
        slot,
      });
    });
  });
  const nodes = graph.nodes.flatMap((node) => {
    const one = placed.get(node.id);
    return one ? [one] : [];
  });

  // The far side of the nodes: back edges run beyond it.
  const nodesEnd = across
    ? PAD + Math.max(0, widest * slotStep - SLOT_GAP_ACROSS)
    : PAD + Math.max(0, widest * slotStep - SLOT_GAP_DOWN);
  const backCount = back.size;

  // Where each edge leaves its node: one place per outgoing edge, in port order.
  const portRank = (edge: WorkflowEdge): number => {
    const node = graph.nodes.find((one) => one.id === edge.from);
    const rank = node?.ports.findIndex((port) => port.name === edge.port) ?? -1;
    return rank === -1 ? Number.MAX_SAFE_INTEGER : rank;
  };
  const drawn = graph.edges
    .map((edge, index) => ({ edge, index }))
    .filter(({ edge }) => placed.has(edge.from) && placed.has(edge.to));
  const leaving = new Map<string, number[]>();
  for (const { edge, index } of drawn.toSorted(
    (a, b) => portRank(a.edge) - portRank(b.edge) || a.index - b.index,
  )) {
    leaving.set(edge.from, [...(leaving.get(edge.from) ?? []), index]);
  }

  let lane = 0;
  const edges: PlacedEdge[] = drawn.map(({ edge, index }) => {
    const from = placed.get(edge.from) as PlacedNode;
    const to = placed.get(edge.to) as PlacedNode;
    const mine = leaving.get(edge.from) ?? [index];
    const share = (mine.indexOf(index) + 1) / (mine.length + 1);
    // Leaving from the side that faces the next layer.
    const x0 = across ? from.x + from.w : from.x + from.w * share;
    const y0 = across ? from.y + from.h * share : from.y + from.h;
    const isBack = back.has(index);
    const label = across
      ? { x: x0 + 6, y: y0 - 5, anchor: "start" as const }
      : { x: x0 + 5, y: y0 + 14, anchor: "start" as const };
    if (!isBack) {
      const x1 = across ? to.x : to.x + to.w / 2;
      const y1 = across ? to.y + to.h / 2 : to.y;
      return {
        index,
        edge,
        back: false,
        d: cubic(x0, y0, x1, y1, across),
        label,
        middle: { x: (x0 + x1) / 2, y: (y0 + y1) / 2 },
      };
    }
    const mineLane = lane;
    lane += 1;
    // Out into the gap after the source's layer, around the outside, back through the gap before the target's.
    const inset = Math.min(10 + mineLane * 6, (across ? LAYER_GAP_ACROSS : LAYER_GAP_DOWN) / 2 - 4);
    const outside = nodesEnd + LANE_OFFSET + mineLane * LANE_STEP;
    const points: Point[] = across
      ? [
          { x: x0, y: y0 },
          { x: from.x + from.w + inset, y: y0 },
          { x: from.x + from.w + inset, y: outside },
          { x: to.x - inset, y: outside },
          { x: to.x - inset, y: to.y + to.h * 0.75 },
          { x: to.x, y: to.y + to.h * 0.75 },
        ]
      : [
          { x: x0, y: y0 },
          { x: x0, y: from.y + from.h + inset },
          { x: outside, y: from.y + from.h + inset },
          { x: outside, y: to.y - inset },
          { x: to.x + to.w * 0.75, y: to.y - inset },
          { x: to.x + to.w * 0.75, y: to.y },
        ];
    const middle = across
      ? { x: (points[2].x + points[3].x) / 2, y: outside }
      : { x: outside, y: (points[2].y + points[3].y) / 2 };
    return { index, edge, back: true, d: orthogonal(points), label, middle };
  });

  const lanesRoom = backCount === 0 ? 0 : LANE_OFFSET + (backCount - 1) * LANE_STEP + 8;
  const layersEnd =
    PAD + Math.max(0, layerCount * layerStep - (across ? LAYER_GAP_ACROSS : LAYER_GAP_DOWN));
  const width = across ? layersEnd + PAD : nodesEnd + lanesRoom + PAD;
  const height = across ? nodesEnd + lanesRoom + PAD : layersEnd + PAD;
  return {
    orientation,
    nodes,
    byId: placed,
    edges,
    order: layers.flat(),
    layers,
    width,
    height,
  };
}
